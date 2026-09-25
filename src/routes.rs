use std::{
    io::ErrorKind,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

pub const ROUTES_FILE: &str = "data/routes.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RouteTarget {
    pub provider_id: String,
    pub endpoint_id: String,
    #[serde(default)]
    pub api_key_id: String,
    pub upstream_model: String,
    pub weight: u32,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModelRoute {
    pub pattern: String,
    pub targets: Vec<RouteTarget>,
    #[serde(skip, default = "default_cursor")]
    pub(crate) cursor: Arc<AtomicU64>,
}

impl ModelRoute {
    /// Keeps the destinations that still exist and hands their share to the survivors.
    ///
    /// Traffic shares describe how one request splits across the destinations that
    /// exist, so a deletion has to redistribute the removed share: a route whose
    /// remaining weights no longer add up to 100% would be rejected the next time the
    /// stored configuration is loaded. A route left without a usable destination has
    /// no valid share to persist, so the caller drops it.
    pub fn prune_targets(&mut self, keep: impl Fn(&RouteTarget) -> bool) {
        self.targets.retain(keep);
        let total: u64 = self
            .targets
            .iter()
            .filter(|target| target.enabled && target.weight > 0)
            .map(|target| u64::from(target.weight))
            .sum();
        // Without a destination that can receive traffic there is no share to hand out,
        // and no weight total the stored file would accept: the caller drops this route.
        if total == 0 {
            for target in &mut self.targets {
                target.weight = 0;
            }
            return;
        }
        let shares: Vec<u32> = self
            .targets
            .iter()
            .map(|target| {
                if target.enabled && target.weight > 0 {
                    (u64::from(target.weight) * 100 / total) as u32
                } else {
                    // A destination that receives no traffic also adds nothing to the
                    // 100% a request has to distribute.
                    0
                }
            })
            .collect();
        let remainder = 100 - shares.iter().map(|share| u64::from(*share)).sum::<u64>();
        let largest = shares
            .iter()
            .enumerate()
            .max_by_key(|(index, share)| (**share, std::cmp::Reverse(*index)))
            .map(|(index, _)| index);
        for (target, share) in self.targets.iter_mut().zip(shares) {
            target.weight = share;
        }
        if let Some(target) = largest.and_then(|index| self.targets.get_mut(index)) {
            // Rounding down cannot reach 100% on its own; the largest survivor takes the
            // remainder so the stored route keeps the invariant it is validated against.
            target.weight += remainder as u32;
        }
    }

    /// True while at least one destination can still receive a request.
    pub fn has_usable_target(&self) -> bool {
        self.targets
            .iter()
            .any(|target| target.enabled && target.weight > 0)
    }

    pub fn select_target(&self) -> Option<&RouteTarget> {
        let divisor = self
            .targets
            .iter()
            .filter(|target| target.enabled && target.weight > 0)
            .map(|target| u64::from(target.weight))
            .reduce(greatest_common_divisor)?;
        let total: u64 = self
            .targets
            .iter()
            .filter(|target| target.enabled && target.weight > 0)
            .map(|target| u64::from(target.weight) / divisor)
            .sum();
        let position = self.cursor.fetch_add(1, Ordering::Relaxed) % total;
        let mut cumulative = 0;
        self.targets
            .iter()
            .filter(|target| target.enabled && target.weight > 0)
            .find(|target| {
                cumulative += u64::from(target.weight) / divisor;
                position < cumulative
            })
    }
}

#[derive(Clone, Default)]
pub struct RouteStore(pub Arc<RwLock<Vec<ModelRoute>>>);

impl RouteStore {
    pub async fn load() -> Result<Self, String> {
        let contents = match tokio::fs::read(ROUTES_FILE).await {
            Ok(contents) => contents,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(format!("read {ROUTES_FILE}: {err}")),
        };
        let routes: Vec<ModelRoute> = serde_json::from_slice(&contents)
            .map_err(|err| format!("parse {ROUTES_FILE}: {err}"))?;
        validate_route_identities(&routes)?;
        Ok(Self(Arc::new(RwLock::new(routes))))
    }

    pub async fn save_value(routes: &[ModelRoute]) -> Result<(), std::io::Error> {
        crate::storage::write_json_atomic(ROUTES_FILE, &routes).await
    }

    pub async fn resolve(&self, model: &str) -> Option<RouteTarget> {
        let routes = self.0.read().await;
        let selected = routes
            .iter()
            .find(|route| route.pattern == model)
            .or_else(|| {
                routes
                    .iter()
                    .filter_map(|route| {
                        model_pattern_specificity(&route.pattern, model)
                            .map(|specificity| (specificity, route))
                    })
                    .max_by_key(|(specificity, _)| *specificity)
                    .map(|(_, route)| route)
            });
        selected.and_then(ModelRoute::select_target).cloned()
    }
}

fn validate_route_identities(routes: &[ModelRoute]) -> Result<(), String> {
    let mut patterns = std::collections::HashSet::new();
    for route in routes {
        if !valid_model_pattern(&route.pattern) || !patterns.insert(route.pattern.as_str()) {
            return Err(format!(
                "parse {ROUTES_FILE}: model route patterns must be unique exact IDs or non-empty prefixes ending in one '*'"
            ));
        }
        if route.targets.is_empty()
            || route
                .targets
                .iter()
                .any(|target| target.upstream_model.trim().is_empty() || target.weight > 100)
            || route
                .targets
                .iter()
                .map(|target| u64::from(target.weight))
                .sum::<u64>()
                != 100
        {
            return Err(format!(
                "parse {ROUTES_FILE}: model route '{}' must have non-empty targets, valid Provider model IDs, and traffic totaling 100",
                route.pattern
            ));
        }
    }
    Ok(())
}

pub(crate) fn valid_model_pattern(pattern: &str) -> bool {
    !pattern.trim().is_empty()
        && (pattern.matches('*').count() == 0
            || (pattern.ends_with('*') && pattern.matches('*').count() == 1 && pattern.len() > 1))
}

/// Exact patterns sort above every wildcard. Wildcards use their literal prefix
/// length, so the longest matching prefix wins.
pub(crate) fn model_pattern_specificity(pattern: &str, model: &str) -> Option<usize> {
    if pattern == model {
        return Some(usize::MAX);
    }
    let prefix = pattern.strip_suffix('*')?;
    model.starts_with(prefix).then_some(prefix.len())
}

fn greatest_common_divisor(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn default_cursor() -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(0))
}

const fn enabled_by_default() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{ModelRoute, RouteStore, RouteTarget};

    fn route(pattern: &str, model: &str, weight: u32) -> ModelRoute {
        ModelRoute {
            pattern: pattern.to_owned(),
            targets: vec![RouteTarget {
                provider_id: "provider".to_owned(),
                endpoint_id: "endpoint".to_owned(),
                api_key_id: "key".to_owned(),
                upstream_model: model.to_owned(),
                weight,
                enabled: true,
            }],
            cursor: Default::default(),
        }
    }

    #[test]
    fn rejects_ambiguous_persisted_route_patterns() {
        assert!(
            super::validate_route_identities(&[
                route("duplicate", "one", 100),
                route("duplicate", "two", 100),
            ])
            .is_err()
        );
    }

    #[test]
    fn rejects_invalid_persisted_route_targets_and_traffic() {
        let mut invalid = route("invalid", "model", 100);
        invalid.targets.clear();
        assert!(super::validate_route_identities(&[invalid]).is_err());

        let invalid = route("invalid", "model", 99);
        assert!(super::validate_route_identities(&[invalid]).is_err());

        let invalid = route("invalid", "", 100);
        assert!(super::validate_route_identities(&[invalid]).is_err());
    }

    fn target(provider_id: &str, model: &str, weight: u32, enabled: bool) -> RouteTarget {
        RouteTarget {
            provider_id: provider_id.to_owned(),
            endpoint_id: "endpoint".to_owned(),
            api_key_id: "key".to_owned(),
            upstream_model: model.to_owned(),
            weight,
            enabled,
        }
    }

    fn split(weights: &[(&str, u32, bool)]) -> ModelRoute {
        ModelRoute {
            pattern: "split".to_owned(),
            targets: weights
                .iter()
                .map(|(provider_id, weight, enabled)| {
                    target(provider_id, "model", *weight, *enabled)
                })
                .collect(),
            cursor: Default::default(),
        }
    }

    fn shares(route: &ModelRoute) -> Vec<u32> {
        route.targets.iter().map(|target| target.weight).collect()
    }

    #[test]
    fn dropping_a_destination_gives_its_share_to_the_survivors() {
        // Removing one destination must leave a route that still passes the same
        // validation the stored file is read with, or the next start rejects it.
        let mut route = split(&[("gone", 70, true), ("stays", 30, true)]);
        assert!(super::validate_route_identities(&[route.clone()]).is_ok());
        route.prune_targets(|target| target.provider_id != "gone");
        assert_eq!(shares(&route), [100]);
        assert!(super::validate_route_identities(&[route]).is_ok());

        // Rounding down cannot reach 100%, so the largest survivor takes the remainder.
        let mut route = split(&[("gone", 30, true), ("first", 40, true), ("second", 30, true)]);
        route.prune_targets(|target| target.provider_id != "gone");
        assert_eq!(shares(&route), [58, 42]);
        assert!(super::validate_route_identities(&[route]).is_ok());

        // Surviving destinations that already split evenly keep their shares equal.
        let mut route = split(&[("gone", 40, true), ("first", 30, true), ("second", 30, true)]);
        route.prune_targets(|target| target.provider_id != "gone");
        assert_eq!(shares(&route), [50, 50]);
        assert!(super::validate_route_identities(&[route]).is_ok());

        // A destination that receives no traffic must not add to the 100% a request
        // distributes, so it stays at zero while the live share is scaled up.
        let mut route = split(&[("gone", 50, true), ("stays", 25, true), ("paused", 25, false)]);
        route.prune_targets(|target| target.provider_id != "gone");
        assert_eq!(shares(&route), [100, 0]);
        assert!(super::validate_route_identities(&[route]).is_ok());
    }

    #[test]
    fn pruning_the_last_usable_destination_leaves_the_route_droppable() {
        let mut route = split(&[("gone", 60, true), ("paused", 40, false)]);
        route.prune_targets(|target| target.provider_id != "gone");
        assert_eq!(shares(&route), [0]);
        assert!(!route.has_usable_target());
        assert!(route.select_target().is_none());
    }

    #[test]
    fn survivor_shares_always_total_one_hundred() {
        for weights in [
            vec![1, 99],
            vec![33, 33, 34],
            vec![1, 1, 1],
            vec![10, 20, 30, 40],
            vec![99, 1, 0],
        ] {
            let mut route = split(&[("gone", 1, true)]);
            route.targets.extend(
                weights
                    .iter()
                    .map(|weight| target("stays", "model", *weight, true)),
            );
            route.prune_targets(|target| target.provider_id != "gone");
            assert_eq!(shares(&route).iter().sum::<u32>(), 100, "{weights:?}");
            assert!(shares(&route).iter().all(|weight| *weight <= 100));
            assert!(super::validate_route_identities(&[route]).is_ok());
        }
    }

    #[tokio::test]
    async fn exact_route_wins_over_wildcard() {
        let store = RouteStore::default();
        *store.0.write().await = vec![
            route("model-*", "wildcard", 1),
            route("model-a", "exact", 1),
        ];

        assert_eq!(
            store.resolve("model-a").await.unwrap().upstream_model,
            "exact"
        );
    }

    #[tokio::test]
    async fn longest_wildcard_prefix_wins() {
        let store = RouteStore::default();
        *store.0.write().await = vec![
            route("model-*", "short", 1),
            route("model-fast-*", "long", 1),
        ];

        assert_eq!(
            store.resolve("model-fast-a").await.unwrap().upstream_model,
            "long"
        );
    }

    #[test]
    fn target_selection_respects_enabled_weights() {
        let route = ModelRoute {
            pattern: "model".to_owned(),
            targets: vec![
                RouteTarget {
                    provider_id: "provider".to_owned(),
                    endpoint_id: "disabled".to_owned(),
                    api_key_id: "key".to_owned(),
                    upstream_model: "disabled".to_owned(),
                    weight: 100,
                    enabled: false,
                },
                RouteTarget {
                    provider_id: "provider".to_owned(),
                    endpoint_id: "one".to_owned(),
                    api_key_id: "key".to_owned(),
                    upstream_model: "one".to_owned(),
                    weight: 1,
                    enabled: true,
                },
                RouteTarget {
                    provider_id: "provider".to_owned(),
                    endpoint_id: "two".to_owned(),
                    api_key_id: "key".to_owned(),
                    upstream_model: "two".to_owned(),
                    weight: 2,
                    enabled: true,
                },
            ],
            cursor: Default::default(),
        };

        let selected: Vec<_> = (0..6)
            .map(|_| route.select_target().unwrap().upstream_model.as_str())
            .collect();
        assert_eq!(selected, ["one", "two", "two", "one", "two", "two"]);
    }

    #[test]
    fn zero_weight_targets_are_inactive_even_if_legacy_data_marks_them_enabled() {
        let route = ModelRoute {
            pattern: "model".to_owned(),
            targets: vec![
                RouteTarget {
                    provider_id: "provider".to_owned(),
                    endpoint_id: "zero".to_owned(),
                    api_key_id: "key".to_owned(),
                    upstream_model: "zero".to_owned(),
                    weight: 0,
                    enabled: true,
                },
                route("active", "active", 100).targets.remove(0),
            ],
            cursor: Default::default(),
        };

        for _ in 0..4 {
            assert_eq!(route.select_target().unwrap().upstream_model, "active");
        }
    }

    #[test]
    fn percentage_weights_use_the_smallest_equivalent_cycle() {
        let route = ModelRoute {
            pattern: "model".to_owned(),
            targets: vec![
                route("one", "one", 50).targets.remove(0),
                route("two", "two", 50).targets.remove(0),
            ],
            cursor: Default::default(),
        };
        let selected: Vec<_> = (0..4)
            .map(|_| route.select_target().unwrap().upstream_model.as_str())
            .collect();
        assert_eq!(selected, ["one", "two", "one", "two"]);
    }
}

use std::{
    collections::BTreeMap,
    io::ErrorKind,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

pub const ROUTES_FILE: &str = "data/routes.json";

/// How a route chooses one request's destination.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteMode {
    /// Every enabled destination receives its configured share, whether or not
    /// the identities behind it are cooling down. A rate limit therefore changes
    /// nothing here: the destination keeps its share and answers for itself.
    #[default]
    Weighted,
    /// Destinations are grouped by priority, lowest first. The lowest-numbered
    /// group with a destination that still has an eligible identity carries the
    /// traffic, and a group is left only while every destination in it is
    /// cooling down. A rate limit observed on every identity of the carrying
    /// group is what moves traffic to the next one.
    Failover,
}

impl RouteMode {
    /// The mode as Activity records it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Weighted => "weighted",
            Self::Failover => "failover",
        }
    }
}

/// What Yabane knows about one destination at the moment of a decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationAvailability {
    /// A request could leave through it: it needs no identity, or it still has
    /// one that is not cooling down.
    Eligible,
    /// Every identity it could use is cooling down under an explicit Endpoint
    /// cooldown policy.
    Cooling,
    /// It cannot serve at all until its configuration is repaired: a missing
    /// Provider, Endpoint or pinned identity, a disabled Extension, or no
    /// enabled identity left. Never treated as exhaustion, so a standby group
    /// cannot hide it.
    Unusable,
}

/// How a destination stands in the console. Runtime state only: nothing here is
/// stored, and a restart starts with every cooldown forgotten.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationState {
    /// Receives traffic now, or would at its configured share.
    Serving,
    /// Eligible, but a lower-numbered group receives the traffic first.
    Standby,
    /// Every identity it could use is cooling down.
    Cooling,
    /// Cannot serve until its configuration is repaired.
    Unusable,
    /// Turned off or given a zero share.
    Inactive,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RouteTarget {
    pub provider_id: String,
    pub endpoint_id: String,
    /// Empty means the Endpoint chooses the identity from its own credential
    /// policy; a non-empty value pins one credential to this destination.
    #[serde(default)]
    pub credential_id: String,
    pub upstream_model: String,
    pub weight: u32,
    /// The group this destination belongs to in `failover` mode, lowest first.
    /// A `weighted` route keeps whatever an administrator stored here and never
    /// reads it, so switching a route between the two modes never rewrites the
    /// shares an operator configured for the other one.
    #[serde(default = "default_route_priority")]
    pub priority: u32,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModelRoute {
    pub pattern: String,
    #[serde(default)]
    pub mode: RouteMode,
    pub targets: Vec<RouteTarget>,
    #[serde(skip, default = "default_cursor")]
    pub(crate) cursor: Arc<AtomicU64>,
}

/// One request's destination, with the facts Activity records about the choice.
#[derive(Clone, Debug)]
pub struct RouteChoice {
    pub target: RouteTarget,
    pub mode: RouteMode,
    /// True when the destination was taken while a lower-numbered group could
    /// not serve because every destination in it was cooling down.
    pub failover: bool,
    /// True when every destination was cooling down, so the lowest-priority
    /// group carried the request anyway instead of Yabane inventing a failure.
    pub all_cooling: bool,
}

impl ModelRoute {
    /// Destinations this route could use at all. A destination that is turned
    /// off or given a zero share is inactive, not unavailable.
    fn usable_targets(&self) -> impl Iterator<Item = &RouteTarget> {
        self.targets
            .iter()
            .filter(|target| target.enabled && target.weight > 0)
    }

    /// The destinations of this route grouped by priority, lowest first.
    fn priority_groups(&self) -> BTreeMap<u32, Vec<&RouteTarget>> {
        let mut groups: BTreeMap<u32, Vec<&RouteTarget>> = BTreeMap::new();
        for target in self.usable_targets() {
            groups.entry(target.priority).or_default().push(target);
        }
        groups
    }

    /// Weighted round robin over the destinations it is given. Shares are
    /// reduced by their greatest common divisor first, so an even split
    /// alternates instead of drifting two of one destination together.
    fn pick_weighted<'a>(
        &self,
        candidates: impl IntoIterator<Item = &'a RouteTarget>,
    ) -> Option<&'a RouteTarget> {
        let candidates: Vec<&RouteTarget> = candidates.into_iter().collect();
        let divisor = candidates
            .iter()
            .map(|target| u64::from(target.weight))
            .reduce(greatest_common_divisor)?;
        let total: u64 = candidates
            .iter()
            .map(|target| u64::from(target.weight) / divisor)
            .sum();
        let position = self.cursor.fetch_add(1, Ordering::Relaxed) % total;
        let mut cumulative = 0;
        candidates.into_iter().find(|target| {
            cumulative += u64::from(target.weight) / divisor;
            position < cumulative
        })
    }

    /// The group that carries the next request, given what is known now. It
    /// mirrors `select` so the console can name the group that is serving.
    fn serving_priority(
        &self,
        availability: &impl Fn(&RouteTarget) -> DestinationAvailability,
    ) -> Option<u32> {
        let groups = self.priority_groups();
        let lowest = *groups.keys().next()?;
        for (priority, group) in &groups {
            if group
                .iter()
                .any(|target| availability(target) == DestinationAvailability::Eligible)
            {
                return Some(*priority);
            }
            if group
                .iter()
                .any(|target| availability(target) == DestinationAvailability::Unusable)
            {
                return Some(*priority);
            }
        }
        Some(lowest)
    }

    pub fn select(
        &self,
        availability: impl Fn(&RouteTarget) -> DestinationAvailability,
    ) -> Option<RouteChoice> {
        match self.mode {
            RouteMode::Weighted => {
                self.pick_weighted(self.usable_targets())
                    .map(|target| RouteChoice {
                        target: target.clone(),
                        mode: self.mode,
                        failover: false,
                        all_cooling: false,
                    })
            }
            RouteMode::Failover => self.select_failover(&availability),
        }
    }

    fn select_failover(
        &self,
        availability: &impl Fn(&RouteTarget) -> DestinationAvailability,
    ) -> Option<RouteChoice> {
        let groups = self.priority_groups();
        let lowest = *groups.keys().next()?;
        let mut skipped_a_group = false;
        for (priority, group) in &groups {
            let eligible: Vec<&RouteTarget> = group
                .iter()
                .copied()
                .filter(|target| availability(target) == DestinationAvailability::Eligible)
                .collect();
            if !eligible.is_empty() {
                return self.pick_weighted(eligible).map(|target| RouteChoice {
                    target: target.clone(),
                    mode: self.mode,
                    failover: skipped_a_group,
                    all_cooling: false,
                });
            }
            let unusable: Vec<&RouteTarget> = group
                .iter()
                .copied()
                .filter(|target| availability(target) == DestinationAvailability::Unusable)
                .collect();
            if !unusable.is_empty() {
                // A destination that cannot serve because of its configuration
                // is not a rate limit. The request fails on it instead of being
                // handed to a standby group, so a broken destination stays
                // visible rather than being papered over by a fallback.
                return self.pick_weighted(unusable).map(|target| RouteChoice {
                    target: target.clone(),
                    mode: self.mode,
                    failover: *priority != lowest,
                    all_cooling: false,
                });
            }
            // Every destination in this group is cooling down; a lower-priority
            // group is a plan for exactly this state.
            skipped_a_group = true;
        }
        // Every destination is cooling down. Yabane does not turn that into an
        // error of its own: the lowest-priority group carries the request so the
        // Provider's own answer reaches the caller.
        let group = &groups[&lowest];
        self.pick_weighted(group.iter().copied())
            .map(|target| RouteChoice {
                target: target.clone(),
                mode: self.mode,
                failover: false,
                all_cooling: true,
            })
    }

    /// What the console reports about every destination, in configured order.
    pub fn destination_states(
        &self,
        availability: impl Fn(&RouteTarget) -> DestinationAvailability,
    ) -> Vec<DestinationState> {
        let active = |target: &RouteTarget| target.enabled && target.weight > 0;
        match self.mode {
            RouteMode::Weighted => self
                .targets
                .iter()
                .map(|target| match (active(target), availability(target)) {
                    (false, _) => DestinationState::Inactive,
                    // Shares apply whether or not an identity is cooling down,
                    // so a cooling destination still carries its share here and
                    // reporting it as out would be untrue.
                    (true, DestinationAvailability::Unusable) => DestinationState::Unusable,
                    (true, _) => DestinationState::Serving,
                })
                .collect(),
            RouteMode::Failover => {
                let serving = self.serving_priority(&availability);
                self.targets
                    .iter()
                    .map(|target| {
                        if !active(target) {
                            return DestinationState::Inactive;
                        }
                        match availability(target) {
                            DestinationAvailability::Unusable => DestinationState::Unusable,
                            DestinationAvailability::Cooling => DestinationState::Cooling,
                            DestinationAvailability::Eligible
                                if Some(target.priority) == serving =>
                            {
                                DestinationState::Serving
                            }
                            DestinationAvailability::Eligible => DestinationState::Standby,
                        }
                    })
                    .collect()
            }
        }
    }

    /// Keeps the destinations that still exist and hands their share to the
    /// survivors of the same split.
    ///
    /// A `weighted` route splits one traffic total, so a removal has to
    /// redistribute across everything that is left: a route whose remaining
    /// weights no longer add up to 100% would be rejected the next time the
    /// stored configuration is loaded. A `failover` route splits each priority
    /// group on its own, so a removal only redistributes inside the group it
    /// happened in; a group left without a usable destination disappears with
    /// its shares. A route left without a usable destination has no valid share
    /// to persist, so the caller drops it.
    pub fn prune_targets(&mut self, keep: impl Fn(&RouteTarget) -> bool) {
        self.targets.retain(keep);
        match self.mode {
            RouteMode::Weighted => redistribute_shares(&mut self.targets, ShareScope::Whole),
            RouteMode::Failover => {
                let priorities: std::collections::BTreeSet<u32> =
                    self.targets.iter().map(|target| target.priority).collect();
                for priority in priorities {
                    redistribute_shares(&mut self.targets, ShareScope::Group(priority));
                }
            }
        }
    }

    /// True while at least one destination can still receive a request.
    pub fn has_usable_target(&self) -> bool {
        self.usable_targets().next().is_some()
    }
}

/// Which split a redistribution repairs: the whole route, or one priority group
/// of a route that gives every group its own 100%.
#[derive(Clone, Copy)]
enum ShareScope {
    Whole,
    Group(u32),
}

/// Hands the removed destination's share to the survivors of one split, so the
/// stored weights still satisfy the rule the file is read back with.
fn redistribute_shares(targets: &mut [RouteTarget], scope: ShareScope) {
    let members = |target: &RouteTarget| match scope {
        ShareScope::Whole => target.enabled && target.weight > 0,
        ShareScope::Group(priority) => {
            target.enabled && target.weight > 0 && target.priority == priority
        }
    };
    let total: u64 = targets
        .iter()
        .filter(|target| members(target))
        .map(|target| u64::from(target.weight))
        .sum();
    // A destination that is switched off carries nothing, so it is stored with a
    // zero weight however its share was written; the split around it is repaired
    // separately below.
    for target in targets.iter_mut().filter(|target| !target.enabled) {
        target.weight = 0;
    }
    if total == 0 {
        return;
    }
    let shares: Vec<u32> = targets
        .iter()
        .map(|target| {
            if members(target) {
                (u64::from(target.weight) * 100 / total) as u32
            } else {
                0
            }
        })
        .collect();
    let remainder = 100 - shares.iter().map(|share| u64::from(*share)).sum::<u64>();
    // Rounding down cannot reach 100% on its own; the largest survivor takes the
    // remainder so the split keeps the total it is validated against.
    let largest = shares
        .iter()
        .enumerate()
        .filter(|(index, _)| members(&targets[*index]))
        .max_by_key(|(index, share)| (**share, std::cmp::Reverse(*index)))
        .map(|(index, _)| index);
    for (target, share) in targets.iter_mut().zip(shares) {
        if members(target) {
            target.weight = share;
        }
    }
    if let Some(target) = largest.and_then(|index| targets.get_mut(index)) {
        target.weight += remainder as u32;
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

    pub async fn resolve(
        &self,
        model: &str,
        availability: impl Fn(&RouteTarget) -> DestinationAvailability,
    ) -> Option<RouteChoice> {
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
        selected.and_then(|route| route.select(&availability))
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
            || route.targets.iter().any(|target| target.priority == 0)
            || !targets_total_as_required(route.mode, &route.targets)
        {
            return Err(format!(
                "parse {ROUTES_FILE}: model route '{}' must have non-empty targets, valid Provider model IDs, a priority of at least 1, and traffic totaling 100 ({})",
                route.pattern,
                match route.mode {
                    RouteMode::Weighted => "one split for the whole route",
                    RouteMode::Failover => "one split per priority group",
                }
            ));
        }
    }
    Ok(())
}

/// Whether a route's shares add up the way its mode requires: one split for the
/// whole `weighted` route, or one split for each priority group that carries
/// destinations in `failover` mode. The rule is shared by the stored file and
/// the management API so a route can never be written that this instance would
/// refuse to read back.
pub fn targets_total_as_required(mode: RouteMode, targets: &[RouteTarget]) -> bool {
    match mode {
        RouteMode::Weighted => {
            targets
                .iter()
                .map(|target| u64::from(target.weight))
                .sum::<u64>()
                == 100
        }
        RouteMode::Failover => {
            let mut groups: BTreeMap<u32, u64> = BTreeMap::new();
            for target in targets
                .iter()
                .filter(|target| target.enabled && target.weight > 0)
            {
                *groups.entry(target.priority).or_default() += u64::from(target.weight);
            }
            !groups.is_empty() && groups.values().all(|total| *total == 100)
        }
    }
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

fn default_route_priority() -> u32 {
    1
}

const fn enabled_by_default() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{
        DestinationAvailability, DestinationState, ModelRoute, RouteMode, RouteStore, RouteTarget,
    };
    use std::collections::HashMap;

    fn route(pattern: &str, model: &str, weight: u32) -> ModelRoute {
        ModelRoute {
            pattern: pattern.to_owned(),
            mode: RouteMode::Weighted,
            targets: vec![RouteTarget {
                provider_id: "provider".to_owned(),
                endpoint_id: "endpoint".to_owned(),
                credential_id: "key".to_owned(),
                upstream_model: model.to_owned(),
                weight,
                priority: 1,
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

    #[test]
    fn rejects_a_destination_in_group_zero() {
        let mut invalid = route("invalid", "model", 100);
        invalid.targets[0].priority = 0;
        assert!(super::validate_route_identities(&[invalid]).is_err());
    }

    fn target(provider_id: &str, model: &str, weight: u32, enabled: bool) -> RouteTarget {
        RouteTarget {
            provider_id: provider_id.to_owned(),
            endpoint_id: "endpoint".to_owned(),
            credential_id: "key".to_owned(),
            upstream_model: model.to_owned(),
            weight,
            priority: 1,
            enabled,
        }
    }

    fn split(weights: &[(&str, u32, bool)]) -> ModelRoute {
        ModelRoute {
            pattern: "split".to_owned(),
            mode: RouteMode::Weighted,
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
        let mut route = split(&[
            ("gone", 30, true),
            ("first", 40, true),
            ("second", 30, true),
        ]);
        route.prune_targets(|target| target.provider_id != "gone");
        assert_eq!(shares(&route), [58, 42]);
        assert!(super::validate_route_identities(&[route]).is_ok());

        // Surviving destinations that already split evenly keep their shares equal.
        let mut route = split(&[
            ("gone", 40, true),
            ("first", 30, true),
            ("second", 30, true),
        ]);
        route.prune_targets(|target| target.provider_id != "gone");
        assert_eq!(shares(&route), [50, 50]);
        assert!(super::validate_route_identities(&[route]).is_ok());

        // A destination that receives no traffic must not add to the 100% a request
        // distributes, so it stays at zero while the live share is scaled up.
        let mut route = split(&[
            ("gone", 50, true),
            ("stays", 25, true),
            ("paused", 25, false),
        ]);
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
        assert!(
            route
                .select(|_| DestinationAvailability::Eligible)
                .is_none()
        );
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

        let choice = store
            .resolve("model-a", |_| DestinationAvailability::Eligible)
            .await
            .expect("a route matches");
        assert_eq!(choice.target.upstream_model, "exact");
    }

    #[tokio::test]
    async fn longest_wildcard_prefix_wins() {
        let store = RouteStore::default();
        *store.0.write().await = vec![
            route("model-*", "short", 1),
            route("model-fast-*", "long", 1),
        ];

        let choice = store
            .resolve("model-fast-a", |_| DestinationAvailability::Eligible)
            .await
            .expect("a route matches");
        assert_eq!(choice.target.upstream_model, "long");
    }

    #[test]
    fn target_selection_respects_enabled_weights() {
        let route = ModelRoute {
            pattern: "model".to_owned(),
            mode: RouteMode::Weighted,
            targets: vec![
                RouteTarget {
                    provider_id: "provider".to_owned(),
                    endpoint_id: "disabled".to_owned(),
                    credential_id: "key".to_owned(),
                    upstream_model: "disabled".to_owned(),
                    weight: 100,
                    priority: 1,
                    enabled: false,
                },
                RouteTarget {
                    provider_id: "provider".to_owned(),
                    endpoint_id: "one".to_owned(),
                    credential_id: "key".to_owned(),
                    upstream_model: "one".to_owned(),
                    weight: 1,
                    priority: 1,
                    enabled: true,
                },
                RouteTarget {
                    provider_id: "provider".to_owned(),
                    endpoint_id: "two".to_owned(),
                    credential_id: "key".to_owned(),
                    upstream_model: "two".to_owned(),
                    weight: 2,
                    priority: 1,
                    enabled: true,
                },
            ],
            cursor: Default::default(),
        };

        let selected: Vec<_> = (0..6)
            .map(|_| {
                route
                    .select(|_| DestinationAvailability::Eligible)
                    .unwrap()
                    .target
                    .upstream_model
            })
            .collect();
        assert_eq!(selected, ["one", "two", "two", "one", "two", "two"]);
    }

    #[test]
    fn zero_weight_targets_are_inactive_even_if_legacy_data_marks_them_enabled() {
        let route = ModelRoute {
            pattern: "model".to_owned(),
            mode: RouteMode::Weighted,
            targets: vec![
                RouteTarget {
                    provider_id: "provider".to_owned(),
                    endpoint_id: "zero".to_owned(),
                    credential_id: "key".to_owned(),
                    upstream_model: "zero".to_owned(),
                    weight: 0,
                    priority: 1,
                    enabled: true,
                },
                route("active", "active", 100).targets.remove(0),
            ],
            cursor: Default::default(),
        };

        for _ in 0..4 {
            assert_eq!(
                route
                    .select(|_| DestinationAvailability::Eligible)
                    .unwrap()
                    .target
                    .upstream_model,
                "active"
            );
        }
    }

    #[test]
    fn percentage_weights_use_the_smallest_equivalent_cycle() {
        let route = ModelRoute {
            pattern: "model".to_owned(),
            mode: RouteMode::Weighted,
            targets: vec![
                route("one", "one", 50).targets.remove(0),
                route("two", "two", 50).targets.remove(0),
            ],
            cursor: Default::default(),
        };
        let selected: Vec<_> = (0..4)
            .map(|_| {
                route
                    .select(|_| DestinationAvailability::Eligible)
                    .unwrap()
                    .target
                    .upstream_model
            })
            .collect();
        assert_eq!(selected, ["one", "two", "one", "two"]);
    }

    /// Two destinations in different priority groups, so a test can say which
    /// group carries the request and whether a group was left behind.
    fn failover_route() -> ModelRoute {
        ModelRoute {
            pattern: "failover".to_owned(),
            mode: RouteMode::Failover,
            targets: vec![
                RouteTarget {
                    provider_id: "preferred".to_owned(),
                    endpoint_id: "endpoint".to_owned(),
                    credential_id: String::new(),
                    upstream_model: "model".to_owned(),
                    weight: 100,
                    priority: 1,
                    enabled: true,
                },
                RouteTarget {
                    provider_id: "standby".to_owned(),
                    endpoint_id: "endpoint".to_owned(),
                    credential_id: String::new(),
                    upstream_model: "model".to_owned(),
                    weight: 100,
                    priority: 2,
                    enabled: true,
                },
            ],
            cursor: Default::default(),
        }
    }

    fn availability<'a>(
        states: &'a [(&'a str, DestinationAvailability)],
    ) -> impl Fn(&RouteTarget) -> DestinationAvailability + 'a {
        let states: HashMap<&'a str, DestinationAvailability> = states.iter().copied().collect();
        move |target: &RouteTarget| {
            states
                .get(target.provider_id.as_str())
                .copied()
                .unwrap_or(DestinationAvailability::Eligible)
        }
    }

    #[test]
    fn failover_keeps_the_preferred_group_while_it_can_serve() {
        let route = failover_route();
        let choice = route
            .select(availability(&[(
                "preferred",
                DestinationAvailability::Eligible,
            )]))
            .expect("a destination is chosen");
        assert_eq!(choice.target.provider_id, "preferred");
        assert!(!choice.failover);
        assert!(!choice.all_cooling);
    }

    #[test]
    fn failover_moves_on_only_when_the_whole_group_is_cooling() {
        let route = failover_route();
        let choice = route
            .select(availability(&[(
                "preferred",
                DestinationAvailability::Cooling,
            )]))
            .expect("a destination is chosen");
        assert_eq!(choice.target.provider_id, "standby");
        assert!(choice.failover);
        assert!(!choice.all_cooling);
        assert_eq!(choice.mode, RouteMode::Failover);
    }

    #[test]
    fn failover_stays_in_a_group_that_still_has_an_eligible_destination() {
        // One Provider with two Destinations in the same group: a rate limit on
        // one of them is not a reason to leave the group.
        let mut route = failover_route();
        route.targets[0].weight = 50;
        route.targets[1].weight = 50;
        route.targets[1].priority = 1;
        route.targets.push(RouteTarget {
            provider_id: "third".to_owned(),
            endpoint_id: "endpoint".to_owned(),
            credential_id: String::new(),
            upstream_model: "model".to_owned(),
            weight: 100,
            priority: 2,
            enabled: true,
        });
        let choice = route
            .select(availability(&[
                ("preferred", DestinationAvailability::Cooling),
                ("standby", DestinationAvailability::Eligible),
            ]))
            .expect("a destination is chosen");
        assert_eq!(choice.target.provider_id, "standby");
        assert!(!choice.failover);
    }

    #[test]
    fn failover_splits_the_serving_group_by_weight() {
        let mut route = failover_route();
        route.targets[0].weight = 1;
        route.targets[1].weight = 2;
        route.targets[1].priority = 1;
        let selected: Vec<_> = (0..6)
            .map(|_| {
                route
                    .select(|_| DestinationAvailability::Eligible)
                    .unwrap()
                    .target
                    .provider_id
            })
            .collect();
        assert_eq!(
            selected,
            [
                "preferred",
                "standby",
                "standby",
                "preferred",
                "standby",
                "standby"
            ]
        );
    }

    #[test]
    fn failover_serves_the_preferred_group_when_everything_is_cooling() {
        let route = failover_route();
        let choice = route
            .select(|_| DestinationAvailability::Cooling)
            .expect("the preferred destination serves anyway");
        assert_eq!(choice.target.provider_id, "preferred");
        assert!(!choice.failover);
        assert!(choice.all_cooling);
    }

    #[test]
    fn failover_reports_a_broken_group_instead_of_using_a_standby() {
        // A destination that cannot serve because of its configuration is not
        // exhaustion, so the standby group must not hide it.
        let route = failover_route();
        let choice = route
            .select(availability(&[
                ("preferred", DestinationAvailability::Unusable),
                ("standby", DestinationAvailability::Eligible),
            ]))
            .expect("a destination is chosen");
        assert_eq!(choice.target.provider_id, "preferred");
        assert!(!choice.failover);
    }

    #[test]
    fn weighted_routes_ignore_rate_limit_state() {
        let mut route = failover_route();
        route.mode = RouteMode::Weighted;
        route.targets[0].weight = 50;
        route.targets[1].weight = 50;
        let selected: Vec<_> = (0..4)
            .map(|_| {
                route
                    .select(|_| DestinationAvailability::Cooling)
                    .unwrap()
                    .target
                    .provider_id
            })
            .collect();
        assert_eq!(selected, ["preferred", "standby", "preferred", "standby"]);
        assert!(
            route
                .select(|_| DestinationAvailability::Cooling)
                .is_some_and(|choice| !choice.failover && !choice.all_cooling)
        );
    }

    #[test]
    fn destination_states_name_the_serving_standby_and_cooling_destinations() {
        let route = failover_route();
        let states = route.destination_states(availability(&[(
            "preferred",
            DestinationAvailability::Cooling,
        )]));
        assert_eq!(
            states,
            [DestinationState::Cooling, DestinationState::Serving]
        );

        let states = route.destination_states(availability(&[(
            "preferred",
            DestinationAvailability::Eligible,
        )]));
        assert_eq!(
            states,
            [DestinationState::Serving, DestinationState::Standby]
        );

        let states = route.destination_states(availability(&[(
            "preferred",
            DestinationAvailability::Unusable,
        )]));
        // The group that cannot serve is still the one a request is sent to, so
        // the request fails on it instead of quietly using the standby group.
        assert_eq!(
            states,
            [DestinationState::Unusable, DestinationState::Standby]
        );
    }

    #[test]
    fn a_weighted_route_reports_its_cooling_destinations_as_still_carrying_their_share() {
        let mut route = failover_route();
        route.mode = RouteMode::Weighted;
        let states = route.destination_states(|_| DestinationAvailability::Cooling);
        assert_eq!(
            states,
            [DestinationState::Serving, DestinationState::Serving]
        );
    }

    #[test]
    fn failover_traffic_is_validated_once_per_priority_group() {
        let mut route = failover_route();
        assert!(super::targets_total_as_required(route.mode, &route.targets));

        // One group that adds up to 200% is refused, even though a weighted
        // route with the same weights would be accepted.
        route.targets[1].priority = 1;
        assert!(!super::targets_total_as_required(
            route.mode,
            &route.targets
        ));
        assert!(super::validate_route_identities(&[route.clone()]).is_err());
        route.targets[0].weight = 50;
        route.targets[1].weight = 50;
        assert!(super::targets_total_as_required(route.mode, &route.targets));

        // A group with no enabled destination is not a split at all.
        route
            .targets
            .iter_mut()
            .for_each(|target| target.enabled = false);
        assert!(!super::targets_total_as_required(
            route.mode,
            &route.targets
        ));
    }

    #[test]
    fn pruning_rebalances_the_group_a_destination_left() {
        let mut route = failover_route();
        route.targets[0].weight = 30;
        route.targets[1].weight = 70;
        route.targets[1].priority = 1;
        route.targets.push(RouteTarget {
            provider_id: "third".to_owned(),
            endpoint_id: "endpoint".to_owned(),
            credential_id: String::new(),
            upstream_model: "model".to_owned(),
            weight: 100,
            priority: 2,
            enabled: true,
        });
        route.prune_targets(|target| target.provider_id != "preferred");
        assert_eq!(shares(&route), [100, 100]);
        assert!(super::validate_route_identities(&[route]).is_ok());
    }
}

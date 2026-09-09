use std::{
    io::ErrorKind,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

const ROUTES_FILE: &str = "data/routes.json";

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
    pub fn select_target(&self) -> Option<&RouteTarget> {
        let total: u64 = self
            .targets
            .iter()
            .filter(|target| target.enabled)
            .map(|target| u64::from(target.weight))
            .sum();
        if total == 0 {
            return None;
        }
        let position = self.cursor.fetch_add(1, Ordering::Relaxed) % total;
        let mut cumulative = 0;
        self.targets
            .iter()
            .filter(|target| target.enabled)
            .find(|target| {
                cumulative += u64::from(target.weight);
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
        let routes = serde_json::from_slice(&contents)
            .map_err(|err| format!("parse {ROUTES_FILE}: {err}"))?;
        Ok(Self(Arc::new(RwLock::new(routes))))
    }

    pub async fn save_value(routes: &[ModelRoute]) -> Result<(), std::io::Error> {
        crate::storage::write_json_atomic(ROUTES_FILE, &routes).await
    }

    pub async fn resolve(&self, model: &str) -> Option<RouteTarget> {
        let routes = self.0.read().await;
        routes
            .iter()
            .find(|route| route.pattern == model)
            .or_else(|| {
                routes
                    .iter()
                    .filter_map(|route| {
                        let prefix = route.pattern.strip_suffix('*')?;
                        model.starts_with(prefix).then_some((prefix.len(), route))
                    })
                    .max_by_key(|(length, _)| *length)
                    .map(|(_, route)| route)
            })
            .and_then(ModelRoute::select_target)
            .cloned()
    }
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
}

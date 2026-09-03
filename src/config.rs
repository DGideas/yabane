use std::{
    collections::HashMap,
    io::ErrorKind,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::auth::SharedAuth;

pub const PROVIDERS_FILE: &str = "data/providers.json";

#[derive(Clone)]
pub struct AppState {
    pub client: reqwest::Client,
    pub providers: Arc<RwLock<HashMap<String, Provider>>>,
    pub auth: SharedAuth,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiType {
    OpenaiCompatible,
    Anthropic,
}

impl ApiType {
    pub fn default_endpoint_id(self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "openai",
            Self::Anthropic => "anthropic",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub secret: String,
    pub weight: u32,
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ApiEndpoint {
    pub id: String,
    pub api_type: ApiType,
    pub base_url: String,
    pub requires_api_key: bool,
    pub api_keys: Vec<ApiKey>,
    #[serde(skip, default = "default_cursor")]
    pub(crate) cursor: Arc<AtomicU64>,
}

impl Default for ApiEndpoint {
    fn default() -> Self {
        Self {
            id: String::new(),
            api_type: ApiType::OpenaiCompatible,
            base_url: String::new(),
            requires_api_key: true,
            api_keys: Vec::new(),
            cursor: default_cursor(),
        }
    }
}

impl ApiEndpoint {
    pub fn select_api_key(&self) -> Option<&ApiKey> {
        let total_weight: u64 = self
            .api_keys
            .iter()
            .filter(|key| key.enabled)
            .map(|key| u64::from(key.weight))
            .sum();
        if total_weight == 0 {
            return None;
        }

        let position = self.cursor.fetch_add(1, Ordering::Relaxed) % total_weight;
        let mut cumulative = 0;
        self.api_keys.iter().filter(|key| key.enabled).find(|key| {
            cumulative += u64::from(key.weight);
            position < cumulative
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModelRoute {
    pub pattern: String,
    pub endpoint_id: String,
    pub api_key_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub endpoints: Vec<ApiEndpoint>,
    #[serde(default)]
    pub discovered_models: Vec<String>,
    #[serde(default)]
    pub models_discovered_at: Option<u64>,
    #[serde(default)]
    pub model_discovery_error: Option<String>,
    #[serde(default)]
    pub model_routes: Vec<ModelRoute>,
}

impl Provider {
    pub fn route_for_model(&self, model: &str) -> Option<&ModelRoute> {
        self.model_routes
            .iter()
            .find(|route| route.pattern == model)
            .or_else(|| {
                self.model_routes
                    .iter()
                    .filter_map(|route| {
                        let prefix = route.pattern.strip_suffix('*')?;
                        model.starts_with(prefix).then_some((prefix.len(), route))
                    })
                    .max_by_key(|(prefix_len, _)| *prefix_len)
                    .map(|(_, route)| route)
            })
    }

    pub fn endpoint_and_key(
        &self,
        endpoint_id: &str,
        api_key_id: &str,
    ) -> Option<(&ApiEndpoint, &ApiKey)> {
        let endpoint = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == endpoint_id)?;
        let key = endpoint.api_keys.iter().find(|key| key.id == api_key_id)?;
        Some((endpoint, key))
    }
}

pub async fn load_providers() -> Result<HashMap<String, Provider>, String> {
    let contents = match tokio::fs::read(PROVIDERS_FILE).await {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => return Err(format!("read {PROVIDERS_FILE}: {err}")),
    };
    let providers: Vec<Provider> = serde_json::from_slice(&contents)
        .map_err(|err| format!("parse {PROVIDERS_FILE}: {err}"))?;
    Ok(providers
        .into_iter()
        .map(|provider| (provider.id.clone(), provider))
        .collect())
}

pub async fn save_providers(providers: &HashMap<String, Provider>) -> Result<(), std::io::Error> {
    tokio::fs::create_dir_all("data").await?;
    let mut values: Vec<_> = providers.values().cloned().collect();
    values.sort_by(|a, b| a.id.cmp(&b.id));
    let contents = serde_json::to_vec_pretty(&values).expect("serialize providers");
    tokio::fs::write(PROVIDERS_FILE, contents).await
}

fn default_cursor() -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(0))
}

#[cfg(test)]
mod tests {
    use super::{ApiEndpoint, ApiKey, ApiType, ModelRoute, Provider};

    fn key(id: &str, weight: u32, enabled: bool) -> ApiKey {
        ApiKey {
            id: id.to_owned(),
            name: id.to_owned(),
            secret: "secret".to_owned(),
            weight,
            enabled,
        }
    }

    #[test]
    fn weighted_key_selection_respects_weights_and_disabled_keys() {
        let endpoint = ApiEndpoint {
            id: "openai".to_owned(),
            api_type: ApiType::OpenaiCompatible,
            base_url: "https://example.com/v1".to_owned(),
            requires_api_key: true,
            api_keys: vec![
                key("primary", 2, true),
                key("secondary", 1, true),
                key("off", 9, false),
            ],
            cursor: super::default_cursor(),
        };
        let selected: Vec<_> = (0..6)
            .map(|_| endpoint.select_api_key().expect("select key").id.as_str())
            .collect();

        assert_eq!(
            selected,
            [
                "primary",
                "primary",
                "secondary",
                "primary",
                "primary",
                "secondary"
            ]
        );
    }

    #[test]
    fn exact_route_wins_then_longest_prefix() {
        let provider = Provider {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            endpoints: Vec::new(),
            discovered_models: Vec::new(),
            models_discovered_at: None,
            model_discovery_error: None,
            model_routes: vec![
                ModelRoute {
                    pattern: "qwen*".to_owned(),
                    endpoint_id: "openai".to_owned(),
                    api_key_id: "a".to_owned(),
                },
                ModelRoute {
                    pattern: "qwen3.8*".to_owned(),
                    endpoint_id: "openai".to_owned(),
                    api_key_id: "b".to_owned(),
                },
                ModelRoute {
                    pattern: "qwen3.8-27b".to_owned(),
                    endpoint_id: "openai".to_owned(),
                    api_key_id: "c".to_owned(),
                },
            ],
        };

        assert_eq!(
            provider
                .route_for_model("qwen3.8-27b")
                .expect("exact")
                .api_key_id,
            "c"
        );
        assert_eq!(
            provider
                .route_for_model("qwen3.8-max")
                .expect("prefix")
                .api_key_id,
            "b"
        );
    }
}

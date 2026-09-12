use std::{
    collections::HashMap,
    io::ErrorKind,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::{
    activity::ActivityStore, admin_user::AdminState, auth::SharedAuth, routes::RouteStore,
};

pub const PROVIDERS_FILE: &str = "data/providers.json";

#[derive(Clone)]
pub struct AppState {
    pub client: reqwest::Client,
    pub providers: Arc<RwLock<HashMap<String, Provider>>>,
    pub auth: SharedAuth,
    pub admin: AdminState,
    pub activity: ActivityStore,
    pub routes: RouteStore,
    pub extensions: Arc<crate::extensions::ExtensionRegistry>,
    #[cfg(feature = "extension-traffic-capture")]
    pub traffic_capture: Arc<yabane_extension_traffic_capture::TrafficCapture>,
    pub openai_oauth: crate::openai_subscription::OAuthState,
}

#[derive(Clone, Copy, Debug, Deserialize, Hash, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiType {
    OpenaiCompatible,
    OpenaiChatCompletions,
    OpenaiResponses,
    OpenaiCodex,
    Anthropic,
}

impl ApiType {
    pub fn default_endpoint_id(self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "openai",
            Self::OpenaiChatCompletions => "openai-chat",
            Self::OpenaiResponses => "openai-responses",
            Self::OpenaiCodex => "chatgpt",
            Self::Anthropic => "anthropic",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub secret: String,
    pub weight: u32,
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenAiSubscription {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub account_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ApiEndpoint {
    pub id: String,
    pub api_type: ApiType,
    pub base_url: String,
    #[serde(default)]
    pub socks5_proxy: Option<String>,
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    #[serde(default)]
    pub extra_body: serde_json::Map<String, serde_json::Value>,
    pub requires_api_key: bool,
    pub api_keys: Vec<ApiKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_subscription: Option<OpenAiSubscription>,
    #[serde(skip, default = "default_cursor")]
    pub(crate) cursor: Arc<AtomicU64>,
    #[serde(skip, default = "default_proxy_client")]
    pub(crate) proxy_client: Arc<OnceLock<Result<reqwest::Client, String>>>,
}

impl Default for ApiEndpoint {
    fn default() -> Self {
        Self {
            id: String::new(),
            api_type: ApiType::OpenaiCompatible,
            base_url: String::new(),
            socks5_proxy: None,
            extra_headers: HashMap::new(),
            extra_body: serde_json::Map::new(),
            requires_api_key: true,
            api_keys: Vec::new(),
            openai_subscription: None,
            cursor: default_cursor(),
            proxy_client: default_proxy_client(),
        }
    }
}

impl ApiEndpoint {
    pub fn client(&self, default: &reqwest::Client) -> Result<reqwest::Client, String> {
        let Some(proxy_url) = &self.socks5_proxy else {
            return Ok(default.clone());
        };
        self.proxy_client
            .get_or_init(|| {
                let proxy = reqwest::Proxy::all(proxy_url).map_err(|err| {
                    format!("invalid SOCKS5 proxy for endpoint '{}': {err}", self.id)
                })?;
                reqwest::Client::builder()
                    .proxy(proxy)
                    .pool_max_idle_per_host(64)
                    .tcp_nodelay(true)
                    .build()
                    .map_err(|err| format!("build client for endpoint '{}': {err}", self.id))
            })
            .clone()
    }

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
pub struct ModelEndpointPreference {
    pub model: String,
    pub api_type: ApiType,
    pub endpoint_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    #[serde(default)]
    pub extra_body: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub defaults_endpoint_ids: Vec<String>,
    pub endpoints: Vec<ApiEndpoint>,
    #[serde(default)]
    pub discovered_models: Vec<String>,
    #[serde(default)]
    pub model_endpoints: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub model_endpoint_preferences: Vec<ModelEndpointPreference>,
    #[serde(default)]
    pub models_discovered_at: Option<u64>,
    #[serde(default)]
    pub model_discovery_error: Option<String>,
}

impl Provider {
    #[cfg(feature = "extension-request-defaults")]
    pub fn request_defaults_apply_to(&self, endpoint_id: &str) -> bool {
        self.defaults_endpoint_ids.is_empty()
            || self
                .defaults_endpoint_ids
                .iter()
                .any(|configured| configured == endpoint_id)
    }

    pub fn preferred_endpoint_id(&self, model: &str, api_type: ApiType) -> Option<&str> {
        self.model_endpoint_preferences
            .iter()
            .find(|preference| preference.model == model && preference.api_type == api_type)
            .map(|preference| preference.endpoint_id.as_str())
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
    validate_provider_identities(&providers)?;
    Ok(providers
        .into_iter()
        .map(|provider| (provider.id.clone(), provider))
        .collect())
}

fn validate_provider_identities(providers: &[Provider]) -> Result<(), String> {
    let mut provider_ids = std::collections::HashSet::new();
    for provider in providers {
        if provider.id.is_empty() || !provider_ids.insert(provider.id.as_str()) {
            return Err(format!(
                "parse {PROVIDERS_FILE}: Provider IDs must be non-empty and unique"
            ));
        }
        let mut endpoint_ids = std::collections::HashSet::new();
        for endpoint in &provider.endpoints {
            if endpoint.id.is_empty() || !endpoint_ids.insert(endpoint.id.as_str()) {
                return Err(format!(
                    "parse {PROVIDERS_FILE}: Endpoint IDs within Provider '{}' must be non-empty and unique",
                    provider.id
                ));
            }
            let mut key_ids = std::collections::HashSet::new();
            if endpoint
                .api_keys
                .iter()
                .any(|key| key.id.is_empty() || !key_ids.insert(key.id.as_str()))
            {
                return Err(format!(
                    "parse {PROVIDERS_FILE}: API key IDs within Endpoint '{}/{}' must be non-empty and unique",
                    provider.id, endpoint.id
                ));
            }
        }
    }
    Ok(())
}

pub async fn save_providers(providers: &HashMap<String, Provider>) -> Result<(), std::io::Error> {
    let mut values: Vec<_> = providers.values().cloned().collect();
    values.sort_by(|a, b| a.id.cmp(&b.id));
    crate::storage::write_json_atomic(PROVIDERS_FILE, &values).await
}

fn default_cursor() -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(0))
}

fn default_proxy_client() -> Arc<OnceLock<Result<reqwest::Client, String>>> {
    Arc::new(OnceLock::new())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{ApiEndpoint, ApiKey, ApiType, Provider};

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
    fn rejects_ambiguous_persisted_resource_identities() {
        let endpoint = ApiEndpoint {
            id: "shared".to_owned(),
            api_keys: vec![key("duplicate", 50, true), key("duplicate", 50, true)],
            ..ApiEndpoint::default()
        };
        let provider = Provider {
            id: "provider".to_owned(),
            name: "Provider".to_owned(),
            extra_headers: HashMap::new(),
            extra_body: serde_json::Map::new(),
            defaults_endpoint_ids: Vec::new(),
            endpoints: vec![endpoint],
            discovered_models: Vec::new(),
            model_endpoints: HashMap::new(),
            model_endpoint_preferences: Vec::new(),
            models_discovered_at: None,
            model_discovery_error: None,
        };
        assert!(super::validate_provider_identities(&[provider]).is_err());
    }

    #[test]
    fn rejects_duplicate_provider_and_endpoint_ids() {
        let provider = |id: &str, endpoints: Vec<ApiEndpoint>| Provider {
            id: id.to_owned(),
            name: id.to_owned(),
            extra_headers: HashMap::new(),
            extra_body: serde_json::Map::new(),
            defaults_endpoint_ids: Vec::new(),
            endpoints,
            discovered_models: Vec::new(),
            model_endpoints: HashMap::new(),
            model_endpoint_preferences: Vec::new(),
            models_discovered_at: None,
            model_discovery_error: None,
        };
        assert!(
            super::validate_provider_identities(&[
                provider("same", Vec::new()),
                provider("same", Vec::new()),
            ])
            .is_err()
        );
        assert!(
            super::validate_provider_identities(&[provider(
                "provider",
                vec![
                    ApiEndpoint {
                        id: "same".to_owned(),
                        ..ApiEndpoint::default()
                    },
                    ApiEndpoint {
                        id: "same".to_owned(),
                        ..ApiEndpoint::default()
                    },
                ],
            )])
            .is_err()
        );
    }

    #[test]
    fn weighted_key_selection_respects_weights_and_disabled_keys() {
        let endpoint = ApiEndpoint {
            id: "openai".to_owned(),
            api_type: ApiType::OpenaiCompatible,
            base_url: "https://example.com/v1".to_owned(),
            socks5_proxy: None,
            extra_headers: HashMap::new(),
            extra_body: serde_json::Map::new(),
            requires_api_key: true,
            openai_subscription: None,
            api_keys: vec![
                key("primary", 2, true),
                key("secondary", 1, true),
                key("off", 9, false),
            ],
            cursor: super::default_cursor(),
            proxy_client: super::default_proxy_client(),
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
}

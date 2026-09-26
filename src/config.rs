use std::{
    collections::HashMap,
    io::ErrorKind,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::{
    activity::ActivityStore,
    admin_user::AdminState,
    auth::{AuthConfig, SharedAuth},
    routes::{ModelRoute, RouteStore},
};

pub const PROVIDERS_FILE: &str = "data/providers.json";

#[derive(Clone, Copy, Debug)]
pub struct UpstreamTimeouts {
    pub connect: Duration,
    pub read: Duration,
    pub total: Duration,
}

#[derive(Clone)]
pub struct AppState {
    pub client: reqwest::Client,
    pub upstream_timeouts: UpstreamTimeouts,
    pub providers: Arc<RwLock<HashMap<String, Provider>>>,
    pub pricing: Arc<RwLock<crate::pricing::PricingTable>>,
    pub auth: SharedAuth,
    pub admin: AdminState,
    pub activity: ActivityStore,
    pub routes: RouteStore,
    /// Runtime-only identity health. Cooldowns are never persisted.
    pub credential_health: crate::health::CredentialHealth,
    pub extensions: Arc<crate::extensions::ExtensionRegistry>,
    #[cfg(feature = "extension-traffic-capture")]
    pub traffic_capture: Arc<yabane_extension_traffic_capture::TrafficCapture>,
    pub sign_in: crate::endpoint_signin::SignInState,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ApiType {
    OpenaiCompatible,
    OpenaiChatCompletions,
    OpenaiResponses,
    Anthropic,
    /// An Endpoint type that an Extension owns. Core knows only the identifier
    /// the Extension declares; wire behavior, catalog, identity kinds, and
    /// sign-in flows all come from that declaration.
    Extension(&'static str),
}

impl ApiType {
    /// The Endpoint types Core implements itself.
    pub const NATIVE: &'static [ApiType] = &[
        Self::OpenaiCompatible,
        Self::OpenaiChatCompletions,
        Self::OpenaiResponses,
        Self::Anthropic,
    ];

    pub fn default_endpoint_id(self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "openai",
            Self::OpenaiChatCompletions => "openai-chat",
            Self::OpenaiResponses => "openai-responses",
            Self::Anthropic => "anthropic",
            Self::Extension(endpoint_type) => endpoint_type,
        }
    }

    /// The native Endpoint type with this identifier, when Core implements it.
    pub fn native(endpoint_type: &str) -> Option<Self> {
        Self::NATIVE
            .iter()
            .copied()
            .find(|api_type| api_type.id() == endpoint_type)
    }

    /// The Endpoint type identifier when an Extension owns this Endpoint.
    pub fn extension_endpoint_type(self) -> Option<&'static str> {
        match self {
            Self::Extension(endpoint_type) => Some(endpoint_type),
            _ => None,
        }
    }

    fn native_id(self) -> Option<&'static str> {
        match self {
            Self::OpenaiCompatible => Some("openai_compatible"),
            Self::OpenaiChatCompletions => Some("openai_chat_completions"),
            Self::OpenaiResponses => Some("openai_responses"),
            Self::Anthropic => Some("anthropic"),
            Self::Extension(_) => None,
        }
    }

    /// The identifier this Endpoint type is known by, in configuration and in
    /// the console.
    pub fn id(self) -> &'static str {
        self.native_id()
            .unwrap_or_else(|| self.default_endpoint_id())
    }

    /// Remembers an Endpoint type identifier for the life of the process, so an
    /// Endpoint type stays a cheap copyable value while its identifier may come
    /// from configuration.
    fn intern(endpoint_type: &str) -> &'static str {
        Box::leak(endpoint_type.to_owned().into_boxed_str())
    }
}

impl Serialize for ApiType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id())
    }
}

impl<'de> Deserialize<'de> for ApiType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "openai_compatible" => Self::OpenaiCompatible,
            "openai_chat_completions" => Self::OpenaiChatCompletions,
            "openai_responses" => Self::OpenaiResponses,
            "anthropic" => Self::Anthropic,
            endpoint_type => Self::Extension(Self::intern(endpoint_type)),
        })
    }
}

/// The identity a request can leave with. A credential is either a pasted
/// secret or a signed-in account; both are identities from the caller's
/// perspective, and both carry weight, enablement, and runtime health.
///
/// The kind identifier and the material shape are declared by the Endpoint type
/// that accepts them: Core stores and displays both without deciding what they
/// mean.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Credential {
    pub id: String,
    pub name: String,
    pub weight: u32,
    pub enabled: bool,
    pub kind: String,
    #[serde(flatten)]
    pub material: CredentialMaterial,
}

/// The two material shapes Core can store. Which kind identifiers map to which
/// shape is declared by the Endpoint type, never inferred here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CredentialMaterial {
    Secret {
        secret: String,
    },
    Subscription {
        access_token: String,
        refresh_token: String,
        expires_at: u64,
        account_id: String,
    },
}

impl CredentialMaterial {
    pub fn secret(&self) -> Option<&str> {
        match self {
            Self::Secret { secret } => Some(secret),
            Self::Subscription { .. } => None,
        }
    }

    pub fn subscription(&self) -> Option<SubscriptionMaterial<'_>> {
        match self {
            Self::Secret { .. } => None,
            Self::Subscription {
                access_token,
                refresh_token,
                expires_at,
                account_id,
            } => Some(SubscriptionMaterial {
                access_token,
                refresh_token,
                expires_at: *expires_at,
                account_id,
            }),
        }
    }

    pub fn flow(&self) -> yabane_extension_api::CredentialFlow {
        match self {
            Self::Secret { .. } => yabane_extension_api::CredentialFlow::Secret,
            Self::Subscription { .. } => yabane_extension_api::CredentialFlow::Subscription,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubscriptionMaterial<'a> {
    pub access_token: &'a str,
    pub refresh_token: &'a str,
    pub expires_at: u64,
    pub account_id: &'a str,
}

impl From<yabane_extension_api::SubscriptionCredential> for CredentialMaterial {
    fn from(credential: yabane_extension_api::SubscriptionCredential) -> Self {
        Self::Subscription {
            access_token: credential.access_token,
            refresh_token: credential.refresh_token,
            expires_at: credential.expires_at,
            account_id: credential.account_id,
        }
    }
}

impl Credential {
    pub fn secret(&self) -> Option<&str> {
        self.material.secret()
    }

    pub fn subscription(&self) -> Option<SubscriptionMaterial<'_>> {
        self.material.subscription()
    }

    /// True while the credential can receive traffic at all. Runtime health is a
    /// separate question answered by [`ApiEndpoint::select_credential`].
    pub fn is_usable(&self) -> bool {
        self.enabled && self.weight > 0
    }
}

/// Explicit, Endpoint-scoped policy for how a Provider's rate-limit answer
/// affects its credentials. Yabane never derives a cooldown from the status
/// code alone: `seconds` is the configured duration and zero disables the
/// whole behavior, while `honor_retry_after` opts into the Provider's own
/// explicit retry hint when it sends one.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RateLimitCooldown {
    #[serde(default)]
    pub seconds: u64,
    #[serde(default)]
    pub honor_retry_after: bool,
}

impl RateLimitCooldown {
    pub const MAX_SECONDS: u64 = 30 * 24 * 60 * 60;

    pub fn enabled(&self) -> bool {
        self.seconds > 0
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<crate::pricing::PricingTable>,
    /// Whether this Endpoint needs an identity at all. An Endpoint that needs
    /// one cannot proxy while it has no usable credential, and an Endpoint that
    /// needs none must not carry any.
    pub requires_credential: bool,
    #[serde(default)]
    pub credentials: Vec<Credential>,
    #[serde(default)]
    pub rate_limit_cooldown: RateLimitCooldown,
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
            pricing: None,
            requires_credential: true,
            credentials: Vec::new(),
            rate_limit_cooldown: RateLimitCooldown::default(),
            cursor: default_cursor(),
            proxy_client: default_proxy_client(),
        }
    }
}

impl ApiEndpoint {
    /// The Endpoint type identifier when an Extension owns this Endpoint.
    pub fn extension_endpoint_type(&self) -> Option<&'static str> {
        self.api_type.extension_endpoint_type()
    }

    pub fn client(
        &self,
        default: &reqwest::Client,
        timeouts: UpstreamTimeouts,
    ) -> Result<reqwest::Client, String> {
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
                    .connect_timeout(timeouts.connect)
                    .read_timeout(timeouts.read)
                    .timeout(timeouts.total)
                    .pool_max_idle_per_host(64)
                    .tcp_nodelay(true)
                    .build()
                    .map_err(|err| format!("build client for endpoint '{}': {err}", self.id))
            })
            .clone()
    }

    /// Chooses the identity a request leaves with.
    ///
    /// Eligible credentials are enabled, carry a positive weight, and are not
    /// cooling down. When every credential is cooling down the pool falls back
    /// to an exhausted credential instead of inventing its own error, so the
    /// Provider's own rate-limit answer still reaches the caller.
    pub fn select_credential(
        &self,
        health: &crate::health::CredentialHealth,
        provider_id: &str,
    ) -> Option<CredentialChoice> {
        let eligible: Vec<&Credential> = self
            .credentials
            .iter()
            .filter(|credential| {
                credential.is_usable()
                    && !health.is_cooling(&crate::health::credential_key(
                        provider_id,
                        &self.id,
                        &credential.id,
                    ))
            })
            .collect();
        let all_exhausted = eligible.is_empty();
        let pool: Vec<&Credential> = if all_exhausted {
            self.credentials
                .iter()
                .filter(|credential| credential.is_usable())
                .collect()
        } else {
            eligible
        };
        let total_weight: u64 = pool
            .iter()
            .map(|credential| u64::from(credential.weight))
            .sum();
        if total_weight == 0 {
            return None;
        }

        let position = self.cursor.fetch_add(1, Ordering::Relaxed) % total_weight;
        let mut cumulative = 0;
        pool.into_iter()
            .find(|credential| {
                cumulative += u64::from(credential.weight);
                position < cumulative
            })
            .cloned()
            .map(|credential| CredentialChoice {
                credential,
                all_exhausted,
            })
    }
}

#[derive(Clone, Debug)]
pub struct CredentialChoice {
    pub credential: Credential,
    /// True when every usable credential was cooling down, so the pool served
    /// the request from an exhausted identity.
    pub all_exhausted: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<crate::pricing::PricingTable>,
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
        if let Some(pricing) = &provider.pricing {
            crate::pricing::validate_table(pricing, &format!("Provider '{}'", provider.id), false)?;
        }
        if provider.id.is_empty() || !provider_ids.insert(provider.id.as_str()) {
            return Err(format!(
                "parse {PROVIDERS_FILE}: Provider IDs must be non-empty and unique"
            ));
        }
        let mut endpoint_ids = std::collections::HashSet::new();
        for endpoint in &provider.endpoints {
            if let Some(pricing) = &endpoint.pricing {
                crate::pricing::validate_table(
                    pricing,
                    &format!("Endpoint '{}/{}'", provider.id, endpoint.id),
                    false,
                )?;
            }
            if endpoint.id.is_empty() || !endpoint_ids.insert(endpoint.id.as_str()) {
                return Err(format!(
                    "parse {PROVIDERS_FILE}: Endpoint IDs within Provider '{}' must be non-empty and unique",
                    provider.id
                ));
            }
            if endpoint.rate_limit_cooldown.seconds > RateLimitCooldown::MAX_SECONDS {
                return Err(format!(
                    "parse {PROVIDERS_FILE}: Endpoint '{}/{}' rate-limit cooldown must not exceed {} seconds",
                    provider.id,
                    endpoint.id,
                    RateLimitCooldown::MAX_SECONDS
                ));
            }
            let mut credential_ids = std::collections::HashSet::new();
            if endpoint.credentials.iter().any(|credential| {
                credential.id.is_empty() || !credential_ids.insert(credential.id.as_str())
            }) {
                return Err(format!(
                    "parse {PROVIDERS_FILE}: Credential IDs within Endpoint '{}/{}' must be non-empty and unique",
                    provider.id, endpoint.id
                ));
            }
            if !endpoint.requires_credential && !endpoint.credentials.is_empty() {
                return Err(format!(
                    "parse {PROVIDERS_FILE}: Endpoint '{}/{}' does not use credentials but carries {}",
                    provider.id,
                    endpoint.id,
                    endpoint.credentials.len()
                ));
            }
        }
    }
    Ok(())
}

/// Checks every identity against the kinds its Endpoint type declares.
///
/// Only a declaration says which kinds an Endpoint may own, and only Extensions
/// declare the kinds they own, so this runs once the registry is known. An
/// Endpoint whose Extension is not enabled has no declaration to check against;
/// routing reports that condition when the Endpoint is used.
pub fn validate_provider_declarations(
    providers: &[Provider],
    extensions: &crate::extensions::ExtensionRegistry,
) -> Result<(), String> {
    for provider in providers {
        for endpoint in &provider.endpoints {
            let Some(kinds) = extensions.credential_kinds(endpoint.api_type) else {
                continue;
            };
            if endpoint.requires_credential && kinds.is_empty() {
                return Err(format!(
                    "Endpoint '{}/{}' requires an identity but Endpoint type '{}' declares no identity kind",
                    provider.id,
                    endpoint.id,
                    endpoint.api_type.id()
                ));
            }
            for credential in &endpoint.credentials {
                let Some(kind) = kinds.iter().find(|kind| kind.id == credential.kind) else {
                    return Err(format!(
                        "Endpoint '{}/{}' of type '{}' does not accept '{}' credentials",
                        provider.id,
                        endpoint.id,
                        endpoint.api_type.id(),
                        credential.kind
                    ));
                };
                if kind.flow != credential.material.flow() {
                    return Err(format!(
                        "Credential '{}/{}/{}' declares kind '{}' but carries material of another shape",
                        provider.id, endpoint.id, credential.id, credential.kind
                    ));
                }
            }
        }
    }
    Ok(())
}

pub fn validate_configuration_references(
    providers: &HashMap<String, Provider>,
    auth: &AuthConfig,
    routes: &[ModelRoute],
) -> Result<(), String> {
    for provider in providers.values() {
        for endpoint_id in &provider.defaults_endpoint_ids {
            if !provider
                .endpoints
                .iter()
                .any(|endpoint| endpoint.id == *endpoint_id)
            {
                return Err(format!(
                    "Provider '{}' Request Defaults refer to unknown Endpoint '{}'",
                    provider.id, endpoint_id
                ));
            }
        }
        for (model, endpoint_ids) in &provider.model_endpoints {
            let mut seen = std::collections::HashSet::new();
            for endpoint_id in endpoint_ids {
                if !seen.insert(endpoint_id) {
                    return Err(format!(
                        "Provider '{}' model '{}' lists Endpoint '{}' more than once",
                        provider.id, model, endpoint_id
                    ));
                }
                if !provider
                    .endpoints
                    .iter()
                    .any(|endpoint| endpoint.id == *endpoint_id)
                {
                    return Err(format!(
                        "Provider '{}' model '{}' refers to unknown Endpoint '{}'",
                        provider.id, model, endpoint_id
                    ));
                }
            }
        }
        for preference in &provider.model_endpoint_preferences {
            let endpoint = provider
                .endpoints
                .iter()
                .find(|endpoint| endpoint.id == preference.endpoint_id)
                .ok_or_else(|| {
                    format!(
                        "Provider '{}' model preference refers to unknown Endpoint '{}'",
                        provider.id, preference.endpoint_id
                    )
                })?;
            if endpoint.api_type != preference.api_type
                || !provider
                    .model_endpoints
                    .get(&preference.model)
                    .is_some_and(|endpoint_ids| endpoint_ids.contains(&preference.endpoint_id))
            {
                return Err(format!(
                    "Provider '{}' model preference for '{}' is not available through Endpoint '{}' with the configured API type",
                    provider.id, preference.model, preference.endpoint_id
                ));
            }
        }
    }

    for key in &auth.api_keys {
        for provider_id in &key.provider_ids {
            if !providers.contains_key(provider_id) {
                return Err(format!(
                    "Gateway API key '{}' refers to unknown Provider '{}'",
                    key.id, provider_id
                ));
            }
        }
    }

    for route in routes {
        for target in &route.targets {
            let provider = providers.get(&target.provider_id).ok_or_else(|| {
                format!(
                    "model route '{}' refers to unknown Provider '{}'",
                    route.pattern, target.provider_id
                )
            })?;
            let endpoint = provider
                .endpoints
                .iter()
                .find(|endpoint| endpoint.id == target.endpoint_id)
                .ok_or_else(|| {
                    format!(
                        "model route '{}' refers to unknown Endpoint '{}/{}'",
                        route.pattern, target.provider_id, target.endpoint_id
                    )
                })?;
            if endpoint.requires_credential {
                if !target.credential_id.is_empty()
                    && !endpoint
                        .credentials
                        .iter()
                        .any(|credential| credential.id == target.credential_id)
                {
                    return Err(format!(
                        "model route '{}' refers to unknown credential '{}/{}/{}'",
                        route.pattern, target.provider_id, target.endpoint_id, target.credential_id
                    ));
                }
            } else if !target.credential_id.is_empty() {
                return Err(format!(
                    "model route '{}' pins credential '{}' on Endpoint '{}/{}' that does not use credentials",
                    route.pattern, target.credential_id, target.provider_id, target.endpoint_id
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

    use super::{ApiEndpoint, ApiType, Credential, CredentialMaterial, Provider};
    use crate::{
        auth::{AuthConfig, GatewayApiKey},
        routes::{ModelRoute, RouteTarget},
    };

    fn credential(id: &str, weight: u32, enabled: bool) -> Credential {
        Credential {
            id: id.to_owned(),
            name: id.to_owned(),
            weight,
            enabled,
            kind: crate::extensions::SECRET_CREDENTIAL_KIND.to_owned(),
            material: CredentialMaterial::Secret {
                secret: "secret".to_owned(),
            },
        }
    }

    #[test]
    fn rejects_ambiguous_persisted_resource_identities() {
        let endpoint = ApiEndpoint {
            id: "shared".to_owned(),
            credentials: vec![
                credential("duplicate", 50, true),
                credential("duplicate", 50, true),
            ],
            ..ApiEndpoint::default()
        };
        let provider = Provider {
            id: "provider".to_owned(),
            name: "Provider".to_owned(),
            extra_headers: HashMap::new(),
            extra_body: serde_json::Map::new(),
            pricing: None,
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
    fn rejects_credentials_that_do_not_match_their_endpoint() {
        fn provider(endpoint: ApiEndpoint) -> Provider {
            Provider {
                id: "provider".to_owned(),
                name: "Provider".to_owned(),
                extra_headers: HashMap::new(),
                extra_body: serde_json::Map::new(),
                pricing: None,
                defaults_endpoint_ids: Vec::new(),
                endpoints: vec![endpoint],
                discovered_models: Vec::new(),
                model_endpoints: HashMap::new(),
                model_endpoint_preferences: Vec::new(),
                models_discovered_at: None,
                model_discovery_error: None,
            }
        }
        fn account(kind: &str) -> Credential {
            Credential {
                id: "account".to_owned(),
                name: "Account".to_owned(),
                weight: 100,
                enabled: true,
                kind: kind.to_owned(),
                material: CredentialMaterial::Subscription {
                    access_token: "access".to_owned(),
                    refresh_token: "refresh".to_owned(),
                    expires_at: 1,
                    account_id: "account-id".to_owned(),
                },
            }
        }

        // An Endpoint whose Extension is not enabled declares no kinds, so its
        // stored identities cannot be checked against it; routing reports the
        // unavailable Endpoint type instead of accepting or rejecting a guess.
        let unavailable = crate::extensions::ExtensionRegistry::without_endpoint_types();
        // The test asks the declaration for the kind identifier instead of naming
        // one itself, exactly as Core does.
        let declared = crate::extensions::ExtensionRegistry::for_tests();
        let declared_kind = declared
            .endpoint_type_declaration("openai_codex")
            .expect("the compiled Endpoint type")
            .credential_kinds
            .first()
            .expect("a declared account kind")
            .id;
        let account_on_an_extension_endpoint = provider(ApiEndpoint {
            id: "chatgpt".to_owned(),
            api_type: ApiType::Extension("openai_codex"),
            credentials: vec![account(declared_kind)],
            ..ApiEndpoint::default()
        });
        assert!(
            super::validate_provider_declarations(
                std::slice::from_ref(&account_on_an_extension_endpoint),
                &unavailable
            )
            .is_ok()
        );

        // With the declaration present, the Endpoint accepts the kind it declares
        // and rejects Core's own secret kind.
        assert!(
            super::validate_provider_declarations(&[account_on_an_extension_endpoint], &declared)
                .is_ok()
        );
        let secret_on_an_account_endpoint = provider(ApiEndpoint {
            id: "chatgpt".to_owned(),
            api_type: ApiType::Extension("openai_codex"),
            credentials: vec![credential("secret", 100, true)],
            ..ApiEndpoint::default()
        });
        assert!(
            super::validate_provider_declarations(&[secret_on_an_account_endpoint], &declared)
                .is_err()
        );

        let account_on_a_secret_endpoint = provider(ApiEndpoint {
            id: "plain".to_owned(),
            credentials: vec![account(declared_kind)],
            ..ApiEndpoint::default()
        });
        assert!(
            super::validate_provider_declarations(&[account_on_a_secret_endpoint], &declared)
                .is_err()
        );

        // A kind Core stores but the Endpoint type never declares is rejected, as
        // is material whose shape contradicts the declared kind.
        let secret_declared_as_an_account = provider(ApiEndpoint {
            id: "misdeclared".to_owned(),
            credentials: vec![Credential {
                kind: declared_kind.to_owned(),
                ..credential("account", 100, true)
            }],
            ..ApiEndpoint::default()
        });
        assert!(
            super::validate_provider_declarations(&[secret_declared_as_an_account], &declared)
                .is_err()
        );

        let credential_on_an_endpoint_that_uses_none = provider(ApiEndpoint {
            id: "keyless".to_owned(),
            requires_credential: false,
            credentials: vec![credential("account", 100, true)],
            ..ApiEndpoint::default()
        });
        assert!(
            super::validate_provider_identities(&[credential_on_an_endpoint_that_uses_none])
                .is_err()
        );
    }

    #[test]
    fn rejects_duplicate_provider_and_endpoint_ids() {
        let provider = |id: &str, endpoints: Vec<ApiEndpoint>| Provider {
            id: id.to_owned(),
            name: id.to_owned(),
            extra_headers: HashMap::new(),
            extra_body: serde_json::Map::new(),
            pricing: None,
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
    fn rejects_dangling_cross_configuration_references() {
        let provider = Provider {
            id: "provider".to_owned(),
            name: "Provider".to_owned(),
            extra_headers: HashMap::new(),
            extra_body: serde_json::Map::new(),
            pricing: None,
            defaults_endpoint_ids: Vec::new(),
            endpoints: vec![ApiEndpoint {
                id: "endpoint".to_owned(),
                requires_credential: true,
                credentials: vec![credential("key", 100, true)],
                ..ApiEndpoint::default()
            }],
            discovered_models: Vec::new(),
            model_endpoints: HashMap::new(),
            model_endpoint_preferences: Vec::new(),
            models_discovered_at: None,
            model_discovery_error: None,
        };
        let providers = HashMap::from([(provider.id.clone(), provider)]);
        let gateway_key = |provider_ids| GatewayApiKey {
            id: "gateway-key".to_owned(),
            note: String::new(),
            secret_hash: "hash".to_owned(),
            secret: String::new(),
            prefix: "sk-…test".to_owned(),
            created_at: 0,
            expires_at: None,
            provider_ids,
        };
        let route = |provider_id: &str, endpoint_id: &str, credential_id: &str| ModelRoute {
            pattern: "alias".to_owned(),
            targets: vec![RouteTarget {
                provider_id: provider_id.to_owned(),
                endpoint_id: endpoint_id.to_owned(),
                credential_id: credential_id.to_owned(),
                upstream_model: "model".to_owned(),
                weight: 100,
                enabled: true,
            }],
            cursor: Default::default(),
        };

        assert!(
            super::validate_configuration_references(
                &providers,
                &AuthConfig {
                    enabled: true,
                    api_keys: vec![gateway_key(vec!["missing".to_owned()])],
                },
                &[],
            )
            .is_err()
        );
        let mut invalid_provider = providers["provider"].clone();
        invalid_provider.defaults_endpoint_ids = vec!["missing".to_owned()];
        assert!(
            super::validate_configuration_references(
                &HashMap::from([(invalid_provider.id.clone(), invalid_provider)]),
                &AuthConfig::default(),
                &[],
            )
            .is_err()
        );
        let mut invalid_provider = providers["provider"].clone();
        invalid_provider
            .model_endpoints
            .insert("model".to_owned(), vec!["missing".to_owned()]);
        assert!(
            super::validate_configuration_references(
                &HashMap::from([(invalid_provider.id.clone(), invalid_provider)]),
                &AuthConfig::default(),
                &[],
            )
            .is_err()
        );
        let auth = AuthConfig {
            enabled: true,
            api_keys: vec![gateway_key(vec!["provider".to_owned()])],
        };
        assert!(
            super::validate_configuration_references(
                &providers,
                &auth,
                &[route("missing", "endpoint", "key")],
            )
            .is_err()
        );
        assert!(
            super::validate_configuration_references(
                &providers,
                &auth,
                &[route("provider", "missing", "key")],
            )
            .is_err()
        );
        assert!(
            super::validate_configuration_references(
                &providers,
                &auth,
                &[route("provider", "endpoint", "missing")],
            )
            .is_err()
        );
        assert!(
            super::validate_configuration_references(
                &providers,
                &auth,
                &[route("provider", "endpoint", "key")],
            )
            .is_ok()
        );
    }

    #[test]
    fn weighted_credential_selection_respects_weights_and_disabled_credentials() {
        let endpoint = ApiEndpoint {
            id: "openai".to_owned(),
            api_type: ApiType::OpenaiCompatible,
            base_url: "https://example.com/v1".to_owned(),
            socks5_proxy: None,
            extra_headers: HashMap::new(),
            extra_body: serde_json::Map::new(),
            pricing: None,
            requires_credential: true,
            credentials: vec![
                credential("primary", 2, true),
                credential("secondary", 1, true),
                credential("off", 9, false),
            ],
            cursor: super::default_cursor(),
            proxy_client: super::default_proxy_client(),
            ..ApiEndpoint::default()
        };
        let health = crate::health::CredentialHealth::default();
        let selected: Vec<_> = (0..6)
            .map(|_| {
                endpoint
                    .select_credential(&health, "provider")
                    .expect("select credential")
                    .credential
                    .id
            })
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
    fn cooling_credentials_leave_selection_and_fall_back_when_none_remain() {
        let endpoint = ApiEndpoint {
            id: "zen".to_owned(),
            credentials: vec![
                credential("account-a", 1, true),
                credential("account-b", 1, true),
            ],
            ..ApiEndpoint::default()
        };
        let health = crate::health::CredentialHealth::default();
        health.cool_down(
            crate::health::credential_key("provider", "zen", "account-a"),
            std::time::Duration::from_secs(60),
        );

        let selected: Vec<_> = (0..4)
            .map(|_| {
                let choice = endpoint
                    .select_credential(&health, "provider")
                    .expect("select credential");
                assert!(!choice.all_exhausted);
                choice.credential.id
            })
            .collect();
        assert_eq!(
            selected,
            ["account-b", "account-b", "account-b", "account-b"]
        );

        health.cool_down(
            crate::health::credential_key("provider", "zen", "account-b"),
            std::time::Duration::from_secs(60),
        );
        let fallback = endpoint
            .select_credential(&health, "provider")
            .expect("fall back to an exhausted credential");
        assert!(fallback.all_exhausted);
    }
}

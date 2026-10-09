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
    routes::{DestinationAvailability, ModelRoute, RouteStore},
};

pub const PROVIDERS_FILE: &str = "data/providers.json";

#[derive(Clone, Copy, Debug)]
pub struct UpstreamTimeouts {
    pub connect: Duration,
    pub read: Duration,
    pub total: Duration,
}

impl UpstreamTimeouts {
    pub fn client_builder(self) -> reqwest::ClientBuilder {
        reqwest::Client::builder()
            // Redirects can replay prompts and nonstandard authentication headers
            // to another origin. The caller must see the Provider's response.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(self.connect)
            .read_timeout(self.read)
            .timeout(self.total)
            .pool_max_idle_per_host(64)
            .tcp_nodelay(true)
    }
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

/// Upper bound on Endpoint type identifiers remembered for the life of the
/// process. Bundled Endpoint types are matched before interning and never count
/// against it.
const MAX_INTERNED_ENDPOINT_TYPES: usize = 256;

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
    /// from configuration. The table is bounded because the identifier is also
    /// read from API input: an unknown type must not grow memory for the life of
    /// the process each time one is submitted.
    fn intern(endpoint_type: &str) -> Result<&'static str, String> {
        static INTERNED: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashSet<&'static str>>,
        > = std::sync::OnceLock::new();
        let interned =
            INTERNED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
        let mut interned = interned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = interned.get(endpoint_type) {
            return Ok(existing);
        }
        if interned.len() >= MAX_INTERNED_ENDPOINT_TYPES {
            return Err(format!(
                "At most {MAX_INTERNED_ENDPOINT_TYPES} distinct Endpoint types are supported"
            ));
        }
        let identifier: &'static str = Box::leak(endpoint_type.to_owned().into_boxed_str());
        interned.insert(identifier);
        Ok(identifier)
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
            endpoint_type => {
                Self::Extension(Self::intern(endpoint_type).map_err(serde::de::Error::custom)?)
            }
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
    /// Which group this identity belongs to, lowest first. A request uses the
    /// lowest-numbered group that still has an eligible identity, so a standby
    /// identity carries traffic only while every identity above it is
    /// exhausted. Configuration written before priorities existed is one group.
    #[serde(default = "default_credential_priority")]
    pub priority: u32,
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

/// Where the length of a rate-limit cooldown comes from. Yabane never derives a
/// cooldown from a status code alone, so choosing a source is the whole
/// configuration; `seconds` is the ceiling in every mode, which is the only
/// statement that stays true whichever source supplies the number.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitCooldownMode {
    /// Use the configured length and ignore anything the Provider reports.
    #[default]
    Fixed,
    /// Use the Provider's own delay when it reports one, otherwise the
    /// configured length.
    PreferProvider,
    /// Use the Provider's own delay when it reports one and do nothing when it
    /// reports none, so no length is ever invented for it.
    ProviderOnly,
}

/// Explicit, Endpoint-scoped policy for how a Provider's rate-limit answer
/// affects its credentials. `seconds` of zero disables the whole behavior.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct RateLimitCooldown {
    pub seconds: u64,
    pub mode: RateLimitCooldownMode,
}

impl RateLimitCooldown {
    pub const MAX_SECONDS: u64 = 30 * 24 * 60 * 60;

    pub fn enabled(&self) -> bool {
        self.seconds > 0
    }
}

/// Configuration written before delay sources existed carried a boolean instead
/// of a mode, so an honored `Retry-After` keeps meaning the Provider-first mode
/// instead of being silently reinterpreted as a fixed duration.
impl<'de> Deserialize<'de> for RateLimitCooldown {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        RateLimitCooldownInput::deserialize(deserializer)?
            .policy()
            .map_err(serde::de::Error::custom)
    }
}

/// The policy as it arrives from a caller or a configuration file, before the
/// delay source is checked. Both shapes go through here so there is one place
/// that decides what an accepted policy means: a stored file fails to load on an
/// unrecognized source, while the admin API reports it as the rejected field it
/// is instead of answering with a body the console cannot read.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RateLimitCooldownInput {
    #[serde(default)]
    pub seconds: u64,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub honor_retry_after: bool,
}

impl RateLimitCooldownInput {
    pub fn policy(&self) -> Result<RateLimitCooldown, String> {
        let mode = match self.mode.as_deref() {
            Some("fixed") => RateLimitCooldownMode::Fixed,
            Some("prefer_provider") => RateLimitCooldownMode::PreferProvider,
            Some("provider_only") => RateLimitCooldownMode::ProviderOnly,
            Some(unknown) => {
                return Err(format!(
                    "Rate-limit cooldown delay source must be one of fixed, prefer_provider, provider_only, not '{unknown}'"
                ));
            }
            // An honored `Retry-After` predates delay sources and meant exactly
            // the Provider-first mode.
            None if self.honor_retry_after => RateLimitCooldownMode::PreferProvider,
            None => RateLimitCooldownMode::Fixed,
        };
        Ok(RateLimitCooldown {
            seconds: self.seconds,
            mode,
        })
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
                timeouts
                    .client_builder()
                    .proxy(proxy)
                    .build()
                    .map_err(|err| format!("build client for endpoint '{}': {err}", self.id))
            })
            .clone()
    }

    /// What a request that delegates identity selection would find here,
    /// without choosing one: asking must not advance the rotation a later
    /// request depends on, and it must not be confused with a decision.
    ///
    /// An Endpoint with no usable identity is unusable rather than exhausted:
    /// turning every identity off is a configuration that cannot serve, and
    /// treating it as a rate limit would let it hide behind a standby group.
    pub fn credential_availability(
        &self,
        health: &crate::health::CredentialHealth,
        provider_id: &str,
    ) -> DestinationAvailability {
        let mut usable = false;
        for credential in self
            .credentials
            .iter()
            .filter(|credential| credential.is_usable())
        {
            usable = true;
            if !health.is_cooling(&crate::health::credential_key(
                provider_id,
                &self.id,
                &credential.id,
            )) {
                return DestinationAvailability::Eligible;
            }
        }
        if usable {
            DestinationAvailability::Cooling
        } else {
            DestinationAvailability::Unusable
        }
    }

    /// Chooses the identity a request leaves with.
    ///
    /// Identities are grouped by priority, and only the lowest-numbered group
    /// that still has an eligible identity takes part in a choice: within that
    /// group the traffic is shared by weight, exactly as a single pool always
    /// was. A higher-numbered group is therefore a standby, not a participant —
    /// it carries nothing while a preferred identity can serve — and identities
    /// sharing one group are interchangeable.
    ///
    /// When every identity is cooling down the pool falls back to the
    /// lowest-numbered usable group instead of inventing its own error, so the
    /// Provider's own rate-limit answer still reaches the caller.
    pub fn select_credential(
        &self,
        health: &crate::health::CredentialHealth,
        provider_id: &str,
    ) -> Option<CredentialChoice> {
        let usable: Vec<&Credential> = self
            .credentials
            .iter()
            .filter(|credential| credential.is_usable())
            .collect();
        let eligible: Vec<&Credential> = usable
            .iter()
            .copied()
            .filter(|credential| {
                !health.is_cooling(&crate::health::credential_key(
                    provider_id,
                    &self.id,
                    &credential.id,
                ))
            })
            .collect();
        let all_exhausted = eligible.is_empty();
        let candidates = if all_exhausted { usable } else { eligible };
        let priority = candidates
            .iter()
            .map(|credential| credential.priority)
            .min()?;
        let pool: Vec<&Credential> = candidates
            .into_iter()
            .filter(|credential| credential.priority == priority)
            .collect();
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

/// What Yabane knows about one route destination before choosing it.
///
/// A pinned identity answers for itself. A destination that delegates identity is
/// out only while every identity its Endpoint could use is cooling down: one
/// rate-limited identity among several is the Endpoint's own rotation to solve,
/// not a reason to leave the destination. Anything that stops a destination from
/// serving at all is unusable rather than exhausted, so a standby group can
/// never hide a broken destination. Shared by the proxy path and the console so
/// both describe the same state.
pub fn destination_availability(
    providers: &HashMap<String, Provider>,
    extensions: &crate::extensions::ExtensionRegistry,
    health: &crate::health::CredentialHealth,
    target: &crate::routes::RouteTarget,
) -> DestinationAvailability {
    let Some(provider) = providers.get(&target.provider_id) else {
        return DestinationAvailability::Unusable;
    };
    let Some(endpoint) = provider
        .endpoints
        .iter()
        .find(|endpoint| endpoint.id == target.endpoint_id)
    else {
        return DestinationAvailability::Unusable;
    };
    if let Some(endpoint_type) = endpoint.extension_endpoint_type()
        && extensions.provider_endpoint(endpoint_type).is_none()
    {
        return DestinationAvailability::Unusable;
    }
    if !endpoint.requires_credential {
        // Nothing about a request to an Endpoint without identities can be
        // rate-limited here, so there is no exhaustion to observe.
        return DestinationAvailability::Eligible;
    }
    if target.credential_id.is_empty() {
        return endpoint.credential_availability(health, &provider.id);
    }
    let Some(credential) = endpoint
        .credentials
        .iter()
        .find(|credential| credential.id == target.credential_id)
    else {
        return DestinationAvailability::Unusable;
    };
    if !credential.enabled {
        return DestinationAvailability::Unusable;
    }
    if health.is_cooling(&crate::health::credential_key(
        &provider.id,
        &endpoint.id,
        &credential.id,
    )) {
        DestinationAvailability::Cooling
    } else {
        DestinationAvailability::Eligible
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

/// The path an Endpoint base URL already names, without scheme, authority,
/// query, or trailing slash.
///
/// A base URL is the shared API root its Provider serves, and that root is not
/// always spelled `/v1`: the same OpenAI-compatible surface is published as
/// `/v2` (Tencent CodeBuddy), `/api/paas/v4` and `/api/paas/v4/saas` (Zhipu),
/// `/v1beta/openai` (Google), and `/compatible-mode/v1` (Alibaba). Reading the
/// path as a value, rather than matching a known spelling, is what lets
/// configuration validation and the upstream URL join agree on which part of a
/// base URL is the operation path and which part the Provider already owns.
pub(crate) fn base_url_path(base_url: &str) -> &str {
    base_url
        .split_once("://")
        .map(|(_, rest)| rest.split_once('/').map_or("", |(_, path)| path))
        .unwrap_or(base_url)
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
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

/// The only group an installation had before priorities existed, and the group
/// every new identity joins until an administrator separates them.
pub(crate) fn default_credential_priority() -> u32 {
    1
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

    /// An Endpoint type identifier from configuration is remembered, but the
    /// table is bounded: API input cannot grow memory for the life of the process.
    #[test]
    fn unknown_endpoint_types_are_remembered_until_the_table_is_full() {
        let first: ApiType = serde_json::from_str("\"config-test-acme\"").unwrap();
        let second: ApiType = serde_json::from_str("\"config-test-acme\"").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.id(), "config-test-acme");
        // Built-in types never touch the table, so they keep working after it is full.
        assert_eq!(
            serde_json::from_str::<ApiType>("\"openai_responses\"").unwrap(),
            ApiType::OpenaiResponses
        );
        let mut refused = None;
        for index in 0..super::MAX_INTERNED_ENDPOINT_TYPES + 8 {
            let identifier = format!("config-test-overflow-{index}");
            match serde_json::from_str::<ApiType>(&format!("\"{identifier}\"")) {
                Ok(api_type) => assert_eq!(api_type.id(), identifier),
                Err(error) => {
                    refused = Some(error.to_string());
                    break;
                }
            }
        }
        let message = refused.expect("the intern table must refuse unbounded identifiers");
        assert!(message.contains("distinct Endpoint types"), "{message}");
        assert_eq!(
            serde_json::from_str::<ApiType>("\"config-test-acme\"").unwrap(),
            first,
            "known identifiers still deserialize after the table is full"
        );
    }

    fn credential(id: &str, weight: u32, enabled: bool) -> Credential {
        Credential {
            id: id.to_owned(),
            name: id.to_owned(),
            weight,
            enabled,
            priority: 1,
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
                priority: 1,
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
        let declared = crate::extensions::tests::registry_with_an_acme_endpoint();
        let declared_kind = declared
            .endpoint_type_declaration("acme_plan")
            .expect("the compiled Endpoint type")
            .credential_kinds
            .first()
            .expect("a declared account kind")
            .id;
        let account_on_an_extension_endpoint = provider(ApiEndpoint {
            id: "account".to_owned(),
            api_type: ApiType::Extension("acme_plan"),
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
            id: "account".to_owned(),
            api_type: ApiType::Extension("acme_plan"),
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
            mode: crate::routes::RouteMode::Weighted,
            targets: vec![RouteTarget {
                provider_id: provider_id.to_owned(),
                endpoint_id: endpoint_id.to_owned(),
                credential_id: credential_id.to_owned(),
                upstream_model: "model".to_owned(),
                weight: 100,
                priority: 1,
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
    fn a_standby_group_carries_traffic_only_while_the_preferred_group_is_exhausted() {
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
                Credential {
                    priority: 1,
                    ..credential("preferred", 70, true)
                },
                Credential {
                    priority: 2,
                    ..credential("standby", 30, true)
                },
            ],
            cursor: super::default_cursor(),
            proxy_client: super::default_proxy_client(),
            ..ApiEndpoint::default()
        };
        let health = crate::health::CredentialHealth::default();
        let selection = |health: &crate::health::CredentialHealth| {
            endpoint
                .select_credential(health, "provider")
                .map(|choice| (choice.credential.id, choice.all_exhausted))
        };
        let cool = |id: &str| {
            health.cool_down(
                crate::health::credential_key("provider", "openai", id),
                std::time::Duration::from_secs(60),
            );
        };

        // A standby waits; it does not take a share, however often the cursor moves.
        for _ in 0..4 {
            assert_eq!(selection(&health), Some(("preferred".to_owned(), false)));
        }
        // Only an exhausted preferred group hands the traffic over.
        cool("preferred");
        assert_eq!(selection(&health), Some(("standby".to_owned(), false)));
        assert_eq!(selection(&health), Some(("standby".to_owned(), false)));
        // When nothing is left, the request still leaves with the preferred group,
        // so the Provider's own answer reaches the caller.
        cool("standby");
        assert_eq!(selection(&health), Some(("preferred".to_owned(), true)));
        // An identity that is enabled again is preferred again, without restarting.
        health.clear(&crate::health::credential_key(
            "provider",
            "openai",
            "preferred",
        ));
        assert_eq!(selection(&health), Some(("preferred".to_owned(), false)));
    }

    #[test]
    fn configuration_without_priorities_keeps_one_group_and_todays_rotation() {
        let endpoint: ApiEndpoint = serde_json::from_str(
            r#"{
                "id": "openai",
                "api_type": "openai_compatible",
                "base_url": "https://example.com/v1",
                "requires_credential": true,
                "credentials": [
                    {"id": "primary", "name": "Primary", "weight": 2, "enabled": true, "kind": "secret", "secret": "a"},
                    {"id": "secondary", "name": "Secondary", "weight": 1, "enabled": true, "kind": "secret", "secret": "b"}
                ]
            }"#,
        )
        .expect("parse an Endpoint written before priorities existed");
        assert!(
            endpoint
                .credentials
                .iter()
                .all(|credential| credential.priority == 1)
        );
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

        // The same sequence the weighted pool produced before priorities existed.
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
    fn delay_sources_read_legacy_and_current_policies_identically() {
        let legacy = |json: &str| -> super::RateLimitCooldown {
            serde_json::from_str(json).expect("parse a cooldown policy")
        };

        // Configuration written before delay sources existed keeps its meaning:
        // an honored Retry-After was the Provider-first mode, and a fixed length
        // never consulted the Provider at all.
        assert_eq!(
            legacy(r#"{"seconds":3600,"honor_retry_after":true}"#).mode,
            super::RateLimitCooldownMode::PreferProvider
        );
        assert_eq!(
            legacy(r#"{"seconds":3600,"honor_retry_after":false}"#).mode,
            super::RateLimitCooldownMode::Fixed
        );
        assert_eq!(legacy("{}").mode, super::RateLimitCooldownMode::Fixed);

        // The current shape round-trips, and a mode it does not define is
        // rejected instead of being read as one of the three.
        let current = legacy(r#"{"seconds":600,"mode":"provider_only"}"#);
        assert_eq!(current.seconds, 600);
        assert_eq!(current.mode, super::RateLimitCooldownMode::ProviderOnly);
        assert_eq!(
            serde_json::to_value(current).expect("serialize a cooldown policy"),
            serde_json::json!({"seconds": 600, "mode": "provider_only"})
        );
        assert!(
            serde_json::from_str::<super::RateLimitCooldown>(
                r#"{"seconds":600,"mode":"sometimes"}"#
            )
            .is_err()
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

use std::{
    collections::{HashMap, HashSet},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
#[cfg(any(
    feature = "extension-request-defaults",
    feature = "extension-traffic-capture",
    feature = "extension-openai-subscription",
    test
))]
use yabane_extension_api::{EXTENSION_API_VERSION, Extension};
use yabane_extension_api::{
    ExtensionError, HookOutcome, ObservedUpstreamRequest, ProviderEndpoint, RequestContext,
    UpstreamExchangeHook, UpstreamExchangeObserver, UpstreamHeadersHook, UpstreamRequestHook,
};

pub const EXTENSIONS_FILE: &str = "data/extensions.json";

#[derive(Clone, Debug, Serialize)]
pub struct ExtensionView {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub api_version: u32,
    pub description: &'static str,
    pub implementation: &'static str,
    pub included: bool,
    pub enabled: bool,
    pub runtime_configurable: bool,
    pub hooks: Vec<&'static str>,
    /// The Endpoint types this Extension owns, so the console can count and
    /// describe them without knowing any Extension by name.
    pub endpoint_types: Vec<ExtensionEndpointTypeView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExtensionEndpointTypeView {
    pub id: &'static str,
    pub label: &'static str,
    pub sign_in: bool,
}

#[derive(Clone, Debug)]
struct ExtensionInfo {
    id: &'static str,
    name: &'static str,
    version: &'static str,
    api_version: u32,
    description: &'static str,
    hooks: Vec<&'static str>,
}

struct ExtensionEntry {
    info: ExtensionInfo,
    enabled: AtomicBool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ExtensionSettings {
    #[serde(default)]
    enabled: HashMap<String, bool>,
}

#[derive(Debug)]
pub enum UpdateError {
    NotFound,
    DisabledByCli,
    Persist(std::io::Error),
}

#[derive(Debug)]
pub enum DispatchOutcome<T> {
    Continue(T),
    Reject(HookRejection),
}

#[derive(Debug)]
pub struct HookRejection {
    extension_id: &'static str,
    instance_id: String,
    rejection: yabane_extension_api::ExtensionRejection,
}

#[derive(Default)]
pub struct RequestHooks<'a> {
    pub upstream_request: Vec<&'a dyn UpstreamRequestHook>,
    pub upstream_headers: Vec<&'a dyn UpstreamHeadersHook>,
    pub upstream_exchange: Vec<&'a dyn UpstreamExchangeHook>,
}

/// A credential kind as the console needs it: the identifier and label belong
/// to the Endpoint type that declares them.
#[derive(Serialize)]
pub struct CredentialKindView {
    pub id: &'static str,
    pub label: &'static str,
    pub flow: &'static str,
}

impl From<&yabane_extension_api::ProviderCredentialKind> for CredentialKindView {
    fn from(kind: &yabane_extension_api::ProviderCredentialKind) -> Self {
        Self {
            id: kind.id,
            label: kind.label,
            flow: credential_flow_name(kind.flow),
        }
    }
}

fn credential_flow_name(flow: yabane_extension_api::CredentialFlow) -> &'static str {
    match flow {
        yabane_extension_api::CredentialFlow::Secret => "secret",
        yabane_extension_api::CredentialFlow::Subscription => "subscription",
    }
}

#[derive(Serialize)]
pub struct SignInView {
    pub device_code: bool,
    pub browser: bool,
}

/// One Endpoint type a Provider may be connected to. Native types are Core's
/// own; every other entry is described entirely by the Extension that declares
/// it, so the console never needs a type list of its own.
#[derive(Serialize)]
pub struct EndpointTypeView {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub default_endpoint_id: &'static str,
    pub fixed_base_url: Option<&'static str>,
    pub credential_kinds: Vec<CredentialKindView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sign_in: Option<SignInView>,
    pub native: bool,
}

impl EndpointTypeView {
    fn native(
        api_type: crate::config::ApiType,
        credential_kinds: Option<&'static [yabane_extension_api::ProviderCredentialKind]>,
    ) -> Self {
        let (label, description) = native_endpoint_type_copy(api_type);
        Self {
            id: api_type.id(),
            label,
            description,
            default_endpoint_id: api_type.default_endpoint_id(),
            fixed_base_url: None,
            credential_kinds: credential_kinds
                .unwrap_or_default()
                .iter()
                .map(CredentialKindView::from)
                .collect(),
            sign_in: None,
            native: true,
        }
    }
}

/// Core's own copy for the Endpoint types Core implements. Extension-owned
/// types bring their own words instead of being named here.
fn native_endpoint_type_copy(api_type: crate::config::ApiType) -> (&'static str, &'static str) {
    match api_type {
        crate::config::ApiType::OpenaiCompatible => (
            "OpenAI compatible",
            "Endpoint supports both Chat Completions and Responses",
        ),
        crate::config::ApiType::OpenaiChatCompletions => (
            "Chat Completions only",
            "Other caller APIs convert to Chat Completions",
        ),
        crate::config::ApiType::OpenaiResponses => {
            ("Responses only", "Other caller APIs convert to Responses")
        }
        crate::config::ApiType::Anthropic => ("Anthropic", "Messages API"),
        crate::config::ApiType::Extension(_) => ("Extension", "Provided by an Extension"),
    }
}

/// The identifier of Core's own identity kind. Every other identifier is
/// declared by the Endpoint type that accepts it.
pub const SECRET_CREDENTIAL_KIND: &str = "secret";

/// The identity kind Core itself owns: a secret the caller pasted. Every other
/// kind is declared by the Endpoint type that accepts it.
pub fn native_credential_kinds() -> &'static [yabane_extension_api::ProviderCredentialKind] {
    static KINDS: &[yabane_extension_api::ProviderCredentialKind] =
        &[yabane_extension_api::ProviderCredentialKind {
            id: SECRET_CREDENTIAL_KIND,
            label: "API key",
            flow: yabane_extension_api::CredentialFlow::Secret,
        }];
    KINDS
}

pub struct ExtensionRegistry {
    extensions: Vec<ExtensionEntry>,
    provider_endpoints: Vec<&'static dyn ProviderEndpoint>,
    subscription_providers: Vec<&'static dyn yabane_extension_api::SubscriptionProvider>,
    settings: Mutex<ExtensionSettings>,
    settings_path: PathBuf,
    disabled_by_cli: bool,
}

impl ExtensionRegistry {
    pub async fn built_in(disabled_by_cli: bool) -> Result<Self, String> {
        let infos: Vec<ExtensionInfo> = vec![
            #[cfg(feature = "extension-request-defaults")]
            extension_info(yabane_extension_request_defaults::metadata())?,
            #[cfg(feature = "extension-traffic-capture")]
            extension_info(yabane_extension_traffic_capture::metadata())?,
            #[cfg(feature = "extension-openai-subscription")]
            extension_info(yabane_extension_openai_subscription::metadata())?,
        ];
        let settings = load_settings(EXTENSIONS_FILE).await?;
        #[allow(unused_mut)]
        let mut registry = Self::new(infos, settings, EXTENSIONS_FILE.into(), disabled_by_cli)?;
        #[cfg(feature = "extension-openai-subscription")]
        {
            registry
                .provider_endpoints
                .push(&yabane_extension_openai_subscription::ENDPOINT);
            registry
                .subscription_providers
                .push(&yabane_extension_openai_subscription::ENDPOINT);
        }
        Ok(registry)
    }

    /// The registry a process would build without touching disk, used by tests
    /// that need the declarations compiled into this binary.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        let infos: Vec<ExtensionInfo> = vec![
            #[cfg(feature = "extension-request-defaults")]
            extension_info(yabane_extension_request_defaults::metadata()).unwrap(),
            #[cfg(feature = "extension-traffic-capture")]
            extension_info(yabane_extension_traffic_capture::metadata()).unwrap(),
            #[cfg(feature = "extension-openai-subscription")]
            extension_info(yabane_extension_openai_subscription::metadata()).unwrap(),
        ];
        #[allow(unused_mut)]
        let mut registry = Self::new(
            infos,
            ExtensionSettings::default(),
            PathBuf::from("data/extensions.test.json"),
            false,
        )
        .expect("build test registry");
        #[cfg(feature = "extension-openai-subscription")]
        {
            registry
                .provider_endpoints
                .push(&yabane_extension_openai_subscription::ENDPOINT);
            registry
                .subscription_providers
                .push(&yabane_extension_openai_subscription::ENDPOINT);
        }
        registry
    }

    fn new(
        infos: Vec<ExtensionInfo>,
        settings: ExtensionSettings,
        settings_path: PathBuf,
        disabled_by_cli: bool,
    ) -> Result<Self, String> {
        let mut ids = HashSet::new();
        for extension in &infos {
            if !ids.insert(extension.id) {
                return Err(format!("duplicate extension ID '{}'", extension.id));
            }
        }
        let extensions = infos
            .into_iter()
            .map(|info| ExtensionEntry {
                enabled: AtomicBool::new(settings.enabled.get(info.id).copied().unwrap_or(true)),
                info,
            })
            .collect();
        Ok(Self {
            extensions,
            provider_endpoints: Vec::new(),
            subscription_providers: Vec::new(),
            settings: Mutex::new(settings),
            settings_path,
            disabled_by_cli,
        })
    }

    pub fn views(&self) -> Vec<ExtensionView> {
        self.extensions
            .iter()
            .map(|extension| ExtensionView {
                id: extension.info.id,
                name: extension.info.name,
                version: extension.info.version,
                api_version: extension.info.api_version,
                description: extension.info.description,
                implementation: "native_rust",
                included: true,
                enabled: !self.disabled_by_cli && extension.enabled.load(Ordering::Acquire),
                runtime_configurable: !self.disabled_by_cli,
                endpoint_types: self
                    .provider_endpoints
                    .iter()
                    .filter(|implementation| implementation.extension_id() == extension.info.id)
                    .map(|implementation| {
                        let declaration = implementation.endpoint_type();
                        ExtensionEndpointTypeView {
                            id: declaration.id,
                            label: declaration.display_name,
                            sign_in: declaration.sign_in.is_some(),
                        }
                    })
                    .collect(),
                hooks: extension.info.hooks.clone(),
            })
            .collect()
    }

    #[cfg_attr(
        not(any(
            feature = "extension-request-defaults",
            feature = "extension-traffic-capture",
            feature = "extension-openai-subscription"
        )),
        allow(dead_code)
    )]
    pub fn is_enabled(&self, id: &str) -> bool {
        !self.disabled_by_cli
            && self
                .extensions
                .iter()
                .find(|extension| extension.info.id == id)
                .is_some_and(|extension| extension.enabled.load(Ordering::Acquire))
    }

    pub fn provider_endpoint(&self, endpoint_type: &str) -> Option<&'static dyn ProviderEndpoint> {
        self.provider_endpoints
            .iter()
            .copied()
            .find(|implementation| {
                implementation.endpoint_type().id == endpoint_type
                    && self.is_enabled(implementation.extension_id())
            })
    }

    /// The Endpoint type declaration an Endpoint is governed by, if any.
    ///
    /// A native Endpoint type is Core's own and has no declaration; an
    /// Extension-owned one is described entirely by its Extension.
    pub fn endpoint_declaration(
        &self,
        api_type: crate::config::ApiType,
    ) -> Option<&'static yabane_extension_api::ProviderEndpointType> {
        self.endpoint_type_declaration(api_type.extension_endpoint_type()?)
    }

    /// The declaration of one Endpoint type, by identifier.
    pub fn endpoint_type_declaration(
        &self,
        endpoint_type: &str,
    ) -> Option<&'static yabane_extension_api::ProviderEndpointType> {
        self.provider_endpoints
            .iter()
            .copied()
            .find(|implementation| {
                implementation.endpoint_type().id == endpoint_type
                    && self.is_enabled(implementation.extension_id())
            })
            .map(|implementation| {
                let declaration: &'static yabane_extension_api::ProviderEndpointType =
                    Box::leak(Box::new(implementation.endpoint_type()));
                declaration
            })
    }

    /// A registry without any Endpoint type, used to test what an Endpoint whose
    /// Extension is not enabled can still do.
    #[cfg(test)]
    pub fn without_endpoint_types() -> Self {
        let mut registry = Self::for_tests();
        registry.provider_endpoints.clear();
        registry.subscription_providers.clear();
        registry
    }

    /// Whether an Extension owns an Endpoint type that connects accounts through
    /// sign-in. Core uses this to keep a sign-in in flight from racing with the
    /// Extension being disabled.
    pub fn owns_sign_in_endpoints(&self, extension_id: &str) -> bool {
        self.provider_endpoints.iter().any(|implementation| {
            implementation.extension_id() == extension_id
                && implementation.endpoint_type().sign_in.is_some()
        })
    }

    /// The identity kinds an Endpoint type accepts.
    ///
    /// Every declared kind, and every label that describes one, comes from the
    /// Endpoint type that owns it. Core's native Endpoint types declare only
    /// Core's own secret kind. `None` means the Endpoint type is unavailable
    /// because its Extension is not enabled, so nothing can be validated against
    /// it and routing reports the condition when the Endpoint is used.
    pub fn credential_kinds(
        &self,
        api_type: crate::config::ApiType,
    ) -> Option<&'static [yabane_extension_api::ProviderCredentialKind]> {
        match self.endpoint_declaration(api_type) {
            Some(declaration) => Some(declaration.credential_kinds),
            None if api_type.extension_endpoint_type().is_none() => Some(native_credential_kinds()),
            None => None,
        }
    }

    /// Every Endpoint type this process can offer, native ones first. The
    /// console builds its choices from this list instead of a name list of its
    /// own, so a new Extension appears without a console change.
    pub fn endpoint_types(&self) -> Vec<EndpointTypeView> {
        let mut endpoint_types: Vec<EndpointTypeView> = crate::config::ApiType::NATIVE
            .iter()
            .map(|api_type| EndpointTypeView::native(*api_type, self.credential_kinds(*api_type)))
            .collect();
        for implementation in &self.provider_endpoints {
            if !self.is_enabled(implementation.extension_id()) {
                continue;
            }
            let declaration = implementation.endpoint_type();
            endpoint_types.push(EndpointTypeView {
                id: declaration.id,
                label: declaration.display_name,
                description: declaration.description,
                default_endpoint_id: declaration.default_endpoint_id,
                fixed_base_url: declaration.fixed_base_url,
                credential_kinds: declaration
                    .credential_kinds
                    .iter()
                    .map(CredentialKindView::from)
                    .collect(),
                sign_in: declaration.sign_in.map(|sign_in| SignInView {
                    device_code: sign_in.device_code,
                    browser: sign_in.browser,
                }),
                native: false,
            });
        }
        endpoint_types
    }

    pub fn subscription_provider(
        &self,
        endpoint_type: &str,
    ) -> Option<&'static dyn yabane_extension_api::SubscriptionProvider> {
        self.subscription_providers
            .iter()
            .copied()
            .find(|implementation| {
                implementation.endpoint_type().id == endpoint_type
                    && self.is_enabled(implementation.extension_id())
            })
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<ExtensionView, UpdateError> {
        if self.disabled_by_cli {
            return Err(UpdateError::DisabledByCli);
        }
        let extension = self
            .extensions
            .iter()
            .find(|extension| extension.info.id == id)
            .ok_or(UpdateError::NotFound)?;
        let mut settings = self.settings.lock().await;
        let mut updated = settings.clone();
        updated.enabled.insert(id.to_owned(), enabled);
        crate::storage::write_json_atomic(&self.settings_path, &updated)
            .await
            .map_err(UpdateError::Persist)?;
        *settings = updated;
        extension.enabled.store(enabled, Ordering::Release);
        Ok(self
            .views()
            .into_iter()
            .find(|view| view.id == id)
            .expect("updated extension remains registered"))
    }

    pub fn run_upstream_request(
        &self,
        context: &RequestContext<'_>,
        mut body: Bytes,
        hooks: &[&dyn UpstreamRequestHook],
    ) -> Result<DispatchOutcome<Bytes>, HookFailure> {
        for hook in hooks {
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook.call(context, body)))
                    .map_err(|_| HookFailure::panicked(hook.extension_id(), hook.instance_id()))?
                    .map_err(|source| {
                        HookFailure::from_error(hook.extension_id(), hook.instance_id(), source)
                    })?;
            body = match outcome {
                HookOutcome::Continue(body) => body,
                HookOutcome::Reject(rejection) => {
                    return Ok(DispatchOutcome::Reject(HookRejection {
                        extension_id: hook.extension_id(),
                        instance_id: hook.instance_id().to_owned(),
                        rejection,
                    }));
                }
            };
        }
        Ok(DispatchOutcome::Continue(body))
    }

    pub fn interested_upstream_exchange<'a>(
        &self,
        context: &RequestContext<'_>,
        hooks: &[&'a dyn UpstreamExchangeHook],
    ) -> Vec<&'a dyn UpstreamExchangeHook> {
        hooks
            .iter()
            .copied()
            .filter(|hook| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    hook.is_interested(context)
                }))
                .unwrap_or_else(|_| {
                    tracing::error!(
                        extension = hook.extension_id(),
                        instance = hook.instance_id(),
                        "upstream exchange observer panicked during preflight"
                    );
                    false
                })
            })
            .collect()
    }

    pub fn begin_upstream_exchange(
        &self,
        context: &RequestContext<'_>,
        request: ObservedUpstreamRequest<'_>,
        hooks: &[&dyn UpstreamExchangeHook],
    ) -> Vec<Box<dyn UpstreamExchangeObserver>> {
        hooks
            .iter()
            .filter_map(|hook| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    hook.begin(
                        context,
                        ObservedUpstreamRequest {
                            headers: request.headers,
                            body: request.body,
                        },
                    )
                }))
                .map_err(|_| {
                    tracing::error!(
                        extension = hook.extension_id(),
                        instance = hook.instance_id(),
                        "upstream exchange observer panicked during initialization"
                    );
                })
                .ok()
                .flatten()
            })
            .collect()
    }

    /// Runs the ordered upstream-header Hook chain directly against the final
    /// upstream `HeaderMap`, after Core has sanitized caller-supplied headers but
    /// before Core injects its own reserved headers (authentication, subscription
    /// identity, hop-by-hop control). Hooks may add, override, or remove any
    /// header that is not Core-reserved; a reserved-header mutation is rejected
    /// and attributed to the offending hook instance, exactly as before, and the
    /// request fails closed without reaching the upstream.
    pub fn run_upstream_headers(
        &self,
        context: &RequestContext<'_>,
        headers: &mut HeaderMap,
        hooks: &[&dyn UpstreamHeadersHook],
    ) -> Result<DispatchOutcome<()>, HookFailure> {
        let mut snapshot = headers.clone();
        for hook in hooks {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                hook.call(context, headers)
            }))
            .map_err(|_| HookFailure::panicked(hook.extension_id(), hook.instance_id()))?
            .map_err(|source| {
                HookFailure::from_error(hook.extension_id(), hook.instance_id(), source)
            })?;
            match outcome {
                HookOutcome::Continue(()) => {}
                HookOutcome::Reject(rejection) => {
                    return Ok(DispatchOutcome::Reject(HookRejection {
                        extension_id: hook.extension_id(),
                        instance_id: hook.instance_id().to_owned(),
                        rejection,
                    }));
                }
            }
            validate_no_reserved_header_changes(&snapshot, headers).map_err(|source| {
                HookFailure::from_error(hook.extension_id(), hook.instance_id(), source)
            })?;
            snapshot = headers.clone();
        }
        Ok(DispatchOutcome::Continue(()))
    }
}

async fn load_settings(path: impl AsRef<Path>) -> Result<ExtensionSettings, String> {
    match tokio::fs::read(path).await {
        Ok(contents) => serde_json::from_slice(&contents)
            .map_err(|error| format!("parse extension settings: {error}")),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(ExtensionSettings::default()),
        Err(error) => Err(format!("read extension settings: {error}")),
    }
}

#[cfg(any(
    feature = "extension-request-defaults",
    feature = "extension-traffic-capture",
    feature = "extension-openai-subscription",
    test
))]
fn extension_info(extension: Extension) -> Result<ExtensionInfo, String> {
    if extension.id.is_empty() {
        return Err("extension ID cannot be empty".to_owned());
    }
    if extension.api_version != EXTENSION_API_VERSION {
        return Err(format!(
            "extension '{}' requires API v{}, but this build supports v{}",
            extension.id, extension.api_version, EXTENSION_API_VERSION
        ));
    }
    let mut hooks = Vec::with_capacity(extension.hooks.len());
    for hook in extension.hooks {
        let hook = hook.as_str();
        if hooks.contains(&hook) {
            return Err(format!(
                "extension '{}' declares duplicate Hook '{hook}'",
                extension.id
            ));
        }
        hooks.push(hook);
    }
    Ok(ExtensionInfo {
        id: extension.id,
        name: extension.name,
        version: extension.version,
        api_version: extension.api_version,
        description: extension.description,
        hooks,
    })
}

fn validate_no_reserved_header_changes(
    before: &HeaderMap,
    after: &HeaderMap,
) -> Result<(), ExtensionError> {
    let names: HashSet<&HeaderName> = before
        .keys()
        .chain(after.keys())
        .filter(|name| reserved_header(name.as_str()))
        .collect();
    for name in names {
        let before_values: Vec<&HeaderValue> = before.get_all(name).iter().collect();
        let after_values: Vec<&HeaderValue> = after.get_all(name).iter().collect();
        if before_values != after_values {
            return Err(ExtensionError::new(
                "reserved_header",
                format!("extension attempted to modify Core-managed header '{name}'"),
            ));
        }
    }
    Ok(())
}

fn reserved_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "authorization"
            | "x-api-key"
            | "cookie"
            | "set-cookie"
            | "content-length"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "chatgpt-account-id"
            | "originator"
    )
}

#[derive(Debug)]
pub struct HookFailure {
    extension_id: &'static str,
    instance_id: String,
    source: ExtensionError,
}

impl HookFailure {
    fn from_error(extension_id: &'static str, instance_id: &str, source: ExtensionError) -> Self {
        Self {
            extension_id,
            instance_id: instance_id.to_owned(),
            source,
        }
    }

    fn panicked(extension_id: &'static str, instance_id: &str) -> Self {
        Self::from_error(
            extension_id,
            instance_id,
            ExtensionError::new("hook_panicked", "Hook panicked"),
        )
    }
}

impl std::fmt::Display for HookFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "extension '{}' instance '{}' failed: {}",
            self.extension_id, self.instance_id, self.source
        )
    }
}

pub fn rejection_response(rejection: HookRejection) -> axum::response::Response {
    tracing::info!(
        extension = rejection.extension_id,
        instance = %rejection.instance_id,
        rejection_code = rejection.rejection.code,
        "request rejected by extension"
    );
    crate::error::api_error_with_type(
        rejection.rejection.status,
        rejection.rejection.message,
        rejection.rejection.code,
    )
}

pub fn execution_error(failure: HookFailure) -> axum::response::Response {
    tracing::error!(
        extension = failure.extension_id,
        instance = %failure.instance_id,
        error_code = failure.source.code,
        "request extension failed"
    );
    crate::error::api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Request extension '{}' failed", failure.extension_id),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use axum::http::{HeaderMap, HeaderName, HeaderValue};
    use bytes::Bytes;
    use yabane_extension_api::{
        EXTENSION_API_VERSION, Extension, ExtensionError, ExtensionHook, ExtensionRejection,
        HookOutcome, HookStage, ObservedUpstreamRequest, Protocol, RequestContext,
        UpstreamExchangeHook, UpstreamExchangeObserver, UpstreamHeadersHook, UpstreamRequestHook,
    };

    use super::{
        DispatchOutcome, ExtensionEntry, ExtensionInfo, ExtensionRegistry, ExtensionSettings,
        RequestHooks, extension_info,
    };

    /// A second, unrelated Endpoint-type Extension. It exists only to prove that
    /// the surfaces Core publishes are built from declarations: its identifier,
    /// words, identity kinds, and sign-in flows are its own, and no Core code
    /// mentions any of them.
    struct AcmeEndpoint;

    impl yabane_extension_api::ProviderEndpoint for AcmeEndpoint {
        fn extension_id(&self) -> &'static str {
            "acme-subscription"
        }

        fn endpoint_type(&self) -> yabane_extension_api::ProviderEndpointType {
            yabane_extension_api::ProviderEndpointType {
                id: "acme_plan",
                display_name: "Acme plan",
                description: "Acme plan Endpoints",
                default_endpoint_id: "acme",
                fixed_base_url: Some("https://api.acme.test/plan"),
                upstream_protocol: Protocol::OpenAiResponses,
                surfaces: &[Protocol::OpenAiResponses],
                always_event_stream: true,
                credential_kinds: &[yabane_extension_api::ProviderCredentialKind {
                    id: "acme_account",
                    label: "Acme account",
                    flow: yabane_extension_api::CredentialFlow::Subscription,
                }],
                sign_in: Some(yabane_extension_api::ProviderSignIn {
                    device_code: true,
                    browser: false,
                }),
            }
        }

        fn models(&self) -> &'static [&'static str] {
            &["acme-large"]
        }

        fn prepare_request(
            &self,
            _request: yabane_extension_api::ProviderEndpointRequest<'_>,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    pub(crate) fn registry_with_an_acme_endpoint() -> ExtensionRegistry {
        let mut registry = ExtensionRegistry::for_tests();
        let implementation: &'static AcmeEndpoint = Box::leak(Box::new(AcmeEndpoint));
        registry.provider_endpoints.push(implementation);
        // An Extension the process carries but does not enable publishes nothing,
        // so the test enables it exactly as an administrator would.
        registry.extensions.push(ExtensionEntry {
            info: ExtensionInfo {
                id: "acme-subscription",
                name: "Acme Subscription",
                version: "0.1.0",
                api_version: EXTENSION_API_VERSION,
                description: "Acme plan Endpoints",
                hooks: Vec::new(),
            },
            enabled: std::sync::atomic::AtomicBool::new(true),
        });
        registry
    }

    #[test]
    fn a_new_endpoint_type_publishes_its_own_words_and_kinds() {
        let registry = registry_with_an_acme_endpoint();
        let published = registry
            .endpoint_types()
            .into_iter()
            .find(|endpoint_type| endpoint_type.id == "acme_plan")
            .expect("the declaration is published without a Core change");
        assert_eq!(published.label, "Acme plan");
        assert_eq!(published.default_endpoint_id, "acme");
        assert_eq!(published.fixed_base_url, Some("https://api.acme.test/plan"));
        assert!(!published.native);
        assert_eq!(
            published
                .credential_kinds
                .iter()
                .map(|kind| (kind.id, kind.label, kind.flow))
                .collect::<Vec<_>>(),
            vec![("acme_account", "Acme account", "subscription")]
        );
        assert_eq!(
            published
                .sign_in
                .map(|sign_in| (sign_in.device_code, sign_in.browser)),
            Some((true, false))
        );
        // The identity kinds and the sign-in endpoints follow the declaration.
        assert_eq!(
            registry
                .credential_kinds(crate::config::ApiType::Extension("acme_plan"))
                .expect("declared kinds")
                .iter()
                .map(|kind| kind.id)
                .collect::<Vec<_>>(),
            vec!["acme_account"]
        );
        assert!(registry.owns_sign_in_endpoints("acme-subscription"));
        assert!(!registry.owns_sign_in_endpoints("request-defaults"));
    }

    struct Append {
        id: &'static str,
        value: &'static [u8],
    }

    impl ExtensionHook for Append {
        fn extension_id(&self) -> &'static str {
            self.id
        }
    }

    impl UpstreamRequestHook for Append {
        fn call(
            &self,
            _context: &RequestContext<'_>,
            mut body: Bytes,
        ) -> Result<HookOutcome<Bytes>, ExtensionError> {
            let mut value = body.to_vec();
            value.extend_from_slice(self.value);
            body = Bytes::from(value);
            Ok(HookOutcome::Continue(body))
        }
    }

    struct Reject;

    impl ExtensionHook for Reject {
        fn extension_id(&self) -> &'static str {
            "reject"
        }
    }

    impl UpstreamRequestHook for Reject {
        fn call(
            &self,
            _context: &RequestContext<'_>,
            _body: Bytes,
        ) -> Result<HookOutcome<Bytes>, ExtensionError> {
            Ok(HookOutcome::Reject(ExtensionRejection::new(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "test_rejection",
                "blocked",
            )))
        }
    }

    struct Panic;

    impl ExtensionHook for Panic {
        fn extension_id(&self) -> &'static str {
            "panic"
        }
    }

    impl UpstreamRequestHook for Panic {
        fn call(
            &self,
            _context: &RequestContext<'_>,
            _body: Bytes,
        ) -> Result<HookOutcome<Bytes>, ExtensionError> {
            panic!("sensitive panic detail")
        }
    }

    struct Exchange {
        interested: bool,
        begins: std::sync::atomic::AtomicUsize,
    }

    impl ExtensionHook for Exchange {
        fn extension_id(&self) -> &'static str {
            "exchange"
        }
    }

    impl UpstreamExchangeHook for Exchange {
        fn is_interested(&self, _context: &RequestContext<'_>) -> bool {
            self.interested
        }

        fn begin(
            &self,
            _context: &RequestContext<'_>,
            _request: ObservedUpstreamRequest<'_>,
        ) -> Option<Box<dyn UpstreamExchangeObserver>> {
            self.begins
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        }
    }

    struct ReservedHeader;

    impl ExtensionHook for ReservedHeader {
        fn extension_id(&self) -> &'static str {
            "reserved-header"
        }
    }

    impl UpstreamHeadersHook for ReservedHeader {
        fn call(
            &self,
            _context: &RequestContext<'_>,
            headers: &mut HeaderMap,
        ) -> Result<HookOutcome<()>, ExtensionError> {
            headers.insert(
                axum::http::header::AUTHORIZATION,
                HeaderValue::from_static("Bearer leaked"),
            );
            Ok(HookOutcome::Continue(()))
        }
    }

    struct RejectHeaders;

    impl ExtensionHook for RejectHeaders {
        fn extension_id(&self) -> &'static str {
            "reject-headers"
        }
    }

    impl UpstreamHeadersHook for RejectHeaders {
        fn call(
            &self,
            _context: &RequestContext<'_>,
            _headers: &mut HeaderMap,
        ) -> Result<HookOutcome<()>, ExtensionError> {
            Ok(HookOutcome::Reject(ExtensionRejection::new(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "test_rejection",
                "blocked",
            )))
        }
    }

    fn registry() -> ExtensionRegistry {
        ExtensionRegistry::new(
            Vec::new(),
            ExtensionSettings::default(),
            std::env::temp_dir().join("unused-extension-settings.json"),
            false,
        )
        .unwrap()
    }

    fn context() -> RequestContext<'static> {
        RequestContext {
            request_id: "req-test",
            public_model: "provider/model",
            upstream_model: "model",
            provider_id: "provider",
            endpoint_id: "endpoint",
            caller_protocol: Protocol::OpenAiChatCompletions,
            upstream_protocol: Protocol::OpenAiChatCompletions,
            requested_streaming: false,
        }
    }

    #[test]
    fn request_hooks_chain_in_declared_order() {
        let first = Append {
            id: "first",
            value: b"-first",
        };
        let second = Append {
            id: "second",
            value: b"-second",
        };
        let DispatchOutcome::Continue(body) = registry()
            .run_upstream_request(&context(), Bytes::from_static(b"body"), &[&first, &second])
            .unwrap()
        else {
            panic!("request unexpectedly rejected")
        };
        assert_eq!(body, "body-first-second");
    }

    #[test]
    fn rejection_stops_later_hooks() {
        let reject = Reject;
        let later = Append {
            id: "later",
            value: b"-must-not-run",
        };
        let outcome = registry()
            .run_upstream_request(&context(), Bytes::from_static(b"body"), &[&reject, &later])
            .unwrap();
        assert!(matches!(outcome, DispatchOutcome::Reject(_)));
    }

    #[test]
    fn hook_panic_is_contained_and_attributed() {
        let failure = registry()
            .run_upstream_request(&context(), Bytes::from_static(b"body"), &[&Panic])
            .unwrap_err();
        assert_eq!(failure.extension_id, "panic");
        assert_eq!(failure.instance_id, "panic");
        assert_eq!(failure.source.code, "hook_panicked");
        assert!(!failure.source.message.contains("sensitive"));
    }

    #[test]
    fn reserved_header_insertion_is_attributed_to_the_hook() {
        let mut headers = HeaderMap::new();
        let failure = registry()
            .run_upstream_headers(&context(), &mut headers, &[&ReservedHeader])
            .unwrap_err();
        assert_eq!(failure.extension_id, "reserved-header");
        assert_eq!(failure.instance_id, "reserved-header");
        assert_eq!(failure.source.code, "reserved_header");
    }

    struct RemoveCallerHeader(&'static str);

    impl ExtensionHook for RemoveCallerHeader {
        fn extension_id(&self) -> &'static str {
            "remove-caller-header"
        }
    }

    impl UpstreamHeadersHook for RemoveCallerHeader {
        fn call(
            &self,
            _context: &RequestContext<'_>,
            headers: &mut HeaderMap,
        ) -> Result<HookOutcome<()>, ExtensionError> {
            headers.remove(self.0);
            Ok(HookOutcome::Continue(()))
        }
    }

    struct RemoveReservedHeader(&'static str);

    impl ExtensionHook for RemoveReservedHeader {
        fn extension_id(&self) -> &'static str {
            "remove-reserved-header"
        }
    }

    impl UpstreamHeadersHook for RemoveReservedHeader {
        fn call(
            &self,
            _context: &RequestContext<'_>,
            headers: &mut HeaderMap,
        ) -> Result<HookOutcome<()>, ExtensionError> {
            headers.remove(self.0);
            Ok(HookOutcome::Continue(()))
        }
    }

    struct AddHeader {
        name: &'static str,
        value: &'static str,
    }

    impl ExtensionHook for AddHeader {
        fn extension_id(&self) -> &'static str {
            "add-header"
        }
    }

    impl UpstreamHeadersHook for AddHeader {
        fn call(
            &self,
            _context: &RequestContext<'_>,
            headers: &mut HeaderMap,
        ) -> Result<HookOutcome<()>, ExtensionError> {
            headers.insert(
                HeaderName::from_static(self.name),
                HeaderValue::from_static(self.value),
            );
            Ok(HookOutcome::Continue(()))
        }
    }

    #[test]
    fn header_hooks_may_remove_caller_supplied_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("cf-connecting-ip"),
            HeaderValue::from_static("203.0.113.7"),
        );
        headers.insert(
            HeaderName::from_static("x-keep"),
            HeaderValue::from_static("value"),
        );
        registry()
            .run_upstream_headers(
                &context(),
                &mut headers,
                &[&RemoveCallerHeader("cf-connecting-ip")],
            )
            .unwrap();
        assert!(headers.get("cf-connecting-ip").is_none());
        assert_eq!(headers["x-keep"], "value");
    }

    #[test]
    fn header_hooks_may_not_remove_reserved_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-api-key"),
            HeaderValue::from_static("secret"),
        );
        let failure = registry()
            .run_upstream_headers(
                &context(),
                &mut headers,
                &[&RemoveReservedHeader("x-api-key")],
            )
            .unwrap_err();
        assert_eq!(failure.extension_id, "remove-reserved-header");
        assert_eq!(failure.source.code, "reserved_header");
    }

    #[test]
    fn header_hooks_chain_in_declared_order_and_see_prior_changes() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-stale"),
            HeaderValue::from_static("old"),
        );
        let outcome = registry()
            .run_upstream_headers(
                &context(),
                &mut headers,
                &[
                    &RemoveCallerHeader("x-stale"),
                    &AddHeader {
                        name: "x-added",
                        value: "new",
                    },
                ],
            )
            .unwrap();
        assert!(matches!(outcome, DispatchOutcome::Continue(())));
        assert!(headers.get("x-stale").is_none());
        assert_eq!(headers["x-added"], "new");
    }

    #[test]
    fn header_hook_rejection_stops_later_hooks() {
        let mut headers = HeaderMap::new();
        let outcome = registry()
            .run_upstream_headers(
                &context(),
                &mut headers,
                &[
                    &RejectHeaders,
                    &AddHeader {
                        name: "x-must-not-run",
                        value: "1",
                    },
                ],
            )
            .unwrap();
        assert!(matches!(outcome, DispatchOutcome::Reject(_)));
        assert!(headers.get("x-must-not-run").is_none());
    }

    #[test]
    fn empty_header_hook_list_is_a_no_op() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-caller"),
            HeaderValue::from_static("value"),
        );
        let original = headers.clone();
        registry()
            .run_upstream_headers(&context(), &mut headers, &[])
            .unwrap();
        assert_eq!(headers, original);
    }

    #[test]
    fn metadata_drives_declared_hook_stages() {
        let extension = extension_info(Extension {
            id: "test",
            name: "Test",
            version: "1.0.0",
            api_version: EXTENSION_API_VERSION,
            description: "test extension",
            hooks: &[HookStage::UpstreamRequest, HookStage::UpstreamHeaders],
        })
        .unwrap();
        assert_eq!(extension.hooks, ["upstream_request", "upstream_headers"]);
    }

    #[test]
    fn incompatible_api_version_fails_loading() {
        let error = extension_info(Extension {
            id: "test",
            name: "Test",
            version: "1.0.0",
            api_version: EXTENSION_API_VERSION + 1,
            description: "test extension",
            hooks: &[],
        })
        .unwrap_err();
        assert!(error.contains("requires API v"));
    }

    #[test]
    fn duplicate_hook_declarations_fail_loading() {
        let error = extension_info(Extension {
            id: "test",
            name: "Test",
            version: "1.0.0",
            api_version: EXTENSION_API_VERSION,
            description: "test extension",
            hooks: &[HookStage::UpstreamRequest, HookStage::UpstreamRequest],
        })
        .unwrap_err();
        assert!(error.contains("duplicate Hook"));
    }

    #[test]
    fn persisted_and_cli_states_control_effective_enablement() {
        let info = extension_info(Extension {
            id: "test",
            name: "Test",
            version: "1.0.0",
            api_version: EXTENSION_API_VERSION,
            description: "test extension",
            hooks: &[],
        })
        .unwrap();
        let settings = ExtensionSettings {
            enabled: [("test".to_owned(), false)].into_iter().collect(),
        };
        let persisted_disabled =
            ExtensionRegistry::new(vec![info.clone()], settings, "unused.json".into(), false)
                .unwrap();
        assert!(!persisted_disabled.is_enabled("test"));

        let cli_disabled = ExtensionRegistry::new(
            vec![info],
            ExtensionSettings::default(),
            "unused.json".into(),
            true,
        )
        .unwrap();
        assert!(!cli_disabled.is_enabled("test"));
        assert!(!cli_disabled.views()[0].runtime_configurable);
    }

    #[tokio::test]
    async fn enabling_state_is_persisted_before_becoming_effective() {
        let path = std::env::temp_dir().join(format!(
            "yabane-extension-settings-{}.json",
            std::process::id()
        ));
        let _ = tokio::fs::remove_file(&path).await;
        let info = extension_info(Extension {
            id: "test",
            name: "Test",
            version: "1.0.0",
            api_version: EXTENSION_API_VERSION,
            description: "test extension",
            hooks: &[],
        })
        .unwrap();
        let registry = ExtensionRegistry::new(
            vec![info],
            ExtensionSettings::default(),
            path.clone(),
            false,
        )
        .unwrap();

        registry.set_enabled("test", false).await.unwrap();
        assert!(!registry.is_enabled("test"));
        let stored: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(stored["enabled"]["test"], false);
        tokio::fs::remove_file(path).await.unwrap();
    }

    #[test]
    fn exchange_preflight_selects_the_exact_hooks_initialized() {
        let interested = Exchange {
            interested: true,
            begins: std::sync::atomic::AtomicUsize::new(0),
        };
        let uninterested = Exchange {
            interested: false,
            begins: std::sync::atomic::AtomicUsize::new(0),
        };
        let registry = registry();
        let selected =
            registry.interested_upstream_exchange(&context(), &[&interested, &uninterested]);
        assert_eq!(selected.len(), 1);
        registry.begin_upstream_exchange(
            &context(),
            ObservedUpstreamRequest {
                headers: &[],
                body: &[],
            },
            &selected,
        );
        assert_eq!(
            interested.begins.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(
            uninterested
                .begins
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn zero_hook_snapshot_is_empty() {
        let hooks = RequestHooks::default();
        assert!(hooks.upstream_request.is_empty());
        assert!(hooks.upstream_headers.is_empty());
        assert!(hooks.upstream_exchange.is_empty());
    }
}

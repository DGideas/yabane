use bytes::Bytes;
use http::{HeaderMap, StatusCode};

pub const EXTENSION_API_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    OpenAiChatCompletions,
    OpenAiResponses,
    AnthropicMessages,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookStage {
    UpstreamRequest,
    UpstreamHeaders,
    UpstreamExchange,
    ProviderEndpoint,
}

impl HookStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UpstreamRequest => "upstream_request",
            Self::UpstreamHeaders => "upstream_headers",
            Self::UpstreamExchange => "upstream_exchange",
            Self::ProviderEndpoint => "provider_endpoint",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RequestContext<'a> {
    pub request_id: &'a str,
    pub public_model: &'a str,
    pub upstream_model: &'a str,
    pub provider_id: &'a str,
    pub endpoint_id: &'a str,
    pub caller_protocol: Protocol,
    pub upstream_protocol: Protocol,
    pub requested_streaming: bool,
}

#[derive(Debug)]
pub struct ExtensionRejection {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ExtensionRejection {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
pub enum HookOutcome<T> {
    Continue(T),
    Reject(ExtensionRejection),
}

#[derive(Debug)]
pub struct ExtensionError {
    pub code: &'static str,
    pub message: String,
}

impl ExtensionError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ExtensionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ExtensionError {}

pub trait ExtensionHook: Send + Sync {
    fn extension_id(&self) -> &'static str;

    fn instance_id(&self) -> &str {
        self.extension_id()
    }
}

pub trait UpstreamRequestHook: ExtensionHook {
    fn call(
        &self,
        context: &RequestContext<'_>,
        body: Bytes,
    ) -> Result<HookOutcome<Bytes>, ExtensionError>;
}

pub trait UpstreamHeadersHook: ExtensionHook {
    fn call(
        &self,
        context: &RequestContext<'_>,
        headers: &mut HeaderMap,
    ) -> Result<HookOutcome<()>, ExtensionError>;
}

/// A credential-safe snapshot of an HTTP header. Core replaces sensitive values
/// before an observer is invoked; observers can apply stricter redaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedHeader {
    pub name: String,
    pub value: String,
}

pub struct ObservedUpstreamRequest<'a> {
    pub headers: &'a [ObservedHeader],
    pub body: &'a [u8],
}

pub struct ObservedUpstreamResponseHead<'a> {
    pub status: StatusCode,
    pub headers: &'a [ObservedHeader],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExchangeOutcome {
    Complete,
    TransportError,
    ResponseReadError,
    Interrupted,
}

/// A per-request, read-only observer. Calls are synchronous and must remain
/// non-blocking; implementations should use bounded in-memory work and enqueue
/// persistence. Observer failures never alter the proxied response.
pub trait UpstreamExchangeObserver: Send {
    fn on_response_head(&mut self, response: ObservedUpstreamResponseHead<'_>);
    fn on_response_chunk(&mut self, chunk: &Bytes);
    fn on_complete(&mut self, outcome: ExchangeOutcome);
}

pub trait UpstreamExchangeHook: ExtensionHook {
    /// Cheap preflight used before Core constructs credential-safe Header snapshots.
    /// Return false when this request cannot produce an observer.
    fn is_interested(&self, _context: &RequestContext<'_>) -> bool {
        true
    }

    fn begin(
        &self,
        context: &RequestContext<'_>,
        request: ObservedUpstreamRequest<'_>,
    ) -> Option<Box<dyn UpstreamExchangeObserver>>;
}

/// The material shape of an identity. Core stores both shapes without knowing
/// what the fields mean to a particular Provider; the Extension decides which
/// shape its Endpoint type accepts and how the material reaches the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialFlow {
    /// A secret the caller pastes. Core puts it in the Provider's own header.
    Secret,
    /// A signed-in account that expires and is refreshed by its Extension.
    Subscription,
}

/// One kind of identity an Endpoint type accepts. The identifier and the label
/// belong to the Extension that declares them: Core only stores and displays
/// them, and never decides which kinds exist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderCredentialKind {
    pub id: &'static str,
    pub label: &'static str,
    pub flow: CredentialFlow,
}

/// The sign-in flows an Endpoint type offers for connecting an account.
#[derive(Clone, Copy, Debug)]
pub struct ProviderSignIn {
    pub device_code: bool,
    pub browser: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ProviderEndpointType {
    pub id: &'static str,
    pub display_name: &'static str,
    /// One line describing this Endpoint type, shown wherever a configured
    /// Endpoint names it.
    pub description: &'static str,
    pub default_endpoint_id: &'static str,
    pub fixed_base_url: Option<&'static str>,
    pub upstream_protocol: Protocol,
    pub always_event_stream: bool,
    /// Caller protocols this Endpoint type can serve. Core consults this instead
    /// of inferring capability from an API type name.
    pub surfaces: &'static [Protocol],
    /// Identity kinds an Endpoint of this type may own. An empty slice means the
    /// type carries no Extension-owned identity, so Core's own `secret` kind
    /// applies.
    pub credential_kinds: &'static [ProviderCredentialKind],
    pub sign_in: Option<ProviderSignIn>,
}

/// The identity material Core hands to an Endpoint implementation for one
/// request. Core stores these shapes and never interprets them further; the
/// Extension decides how they reach the wire.
pub enum ProviderEndpointMaterial<'a> {
    Secret {
        secret: &'a str,
    },
    Subscription {
        access_token: &'a str,
        account_id: &'a str,
    },
}

pub struct ProviderEndpointCredential<'a> {
    /// The kind identifier declared by the Endpoint type that owns this identity.
    pub kind: &'a str,
    pub material: ProviderEndpointMaterial<'a>,
}

pub struct ProviderEndpointRequest<'a> {
    pub headers: &'a mut HeaderMap,
    pub body: &'a mut Vec<u8>,
    pub target_path: &'a mut String,
    /// Core-owned, short-lived credential fields. Implementations must not retain them.
    pub credential: Option<ProviderEndpointCredential<'a>>,
}

/// A compiled provider-specific Endpoint implementation. Core retains routing,
/// persistence, and credential ownership; the Extension owns its wire behavior.
pub trait ProviderEndpoint: Send + Sync {
    fn extension_id(&self) -> &'static str;
    fn endpoint_type(&self) -> ProviderEndpointType;
    fn models(&self) -> &'static [&'static str];
    fn prepare_request(&self, request: ProviderEndpointRequest<'_>) -> Result<(), String>;
}

#[derive(Clone, Debug)]
pub struct SubscriptionCredential {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub account_id: String,
}

/// OAuth and credential lifecycle owned by a subscription Endpoint Extension.
/// The host supplies persistence and resource validation around these operations.
pub trait SubscriptionProvider: ProviderEndpoint {
    fn start_browser_authorization(&self) -> Result<BrowserAuthorization, String>;
    fn exchange_browser_authorization<'a>(
        &'a self,
        client: &'a reqwest::Client,
        code: &'a str,
        code_verifier: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<SubscriptionCredential, String>> + Send + 'a>,
    >;
    fn start_device_authorization<'a>(
        &'a self,
        client: &'a reqwest::Client,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<DeviceAuthorization, String>> + Send + 'a>,
    >;
    fn poll_device_authorization<'a>(
        &'a self,
        client: &'a reqwest::Client,
        device_auth_id: &'a str,
        user_code: &'a str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<SubscriptionCredential>, String>>
                + Send
                + 'a,
        >,
    >;
    fn refresh_credential<'a>(
        &'a self,
        client: &'a reqwest::Client,
        refresh_token: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<SubscriptionCredential, String>> + Send + 'a>,
    >;
}

#[derive(Clone, Debug)]
pub struct BrowserAuthorization {
    pub authorization_url: String,
    pub state: String,
    pub code_verifier: String,
    pub expires_in_seconds: u64,
}

#[derive(Clone, Debug)]
pub struct DeviceAuthorization {
    pub device_auth_id: String,
    pub user_code: String,
    pub verification_uri: &'static str,
    pub interval_seconds: u64,
    pub expires_in_seconds: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Extension {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub api_version: u32,
    pub description: &'static str,
    pub hooks: &'static [HookStage],
}

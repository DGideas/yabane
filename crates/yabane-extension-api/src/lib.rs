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
}

impl HookStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UpstreamRequest => "upstream_request",
            Self::UpstreamHeaders => "upstream_headers",
            Self::UpstreamExchange => "upstream_exchange",
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

#[derive(Clone, Copy, Debug)]
pub struct Extension {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub api_version: u32,
    pub description: &'static str,
    pub hooks: &'static [HookStage],
}

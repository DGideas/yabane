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
}

impl HookStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UpstreamRequest => "upstream_request",
            Self::UpstreamHeaders => "upstream_headers",
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

#[derive(Clone, Copy, Debug)]
pub struct Extension {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub api_version: u32,
    pub description: &'static str,
    pub hooks: &'static [HookStage],
}

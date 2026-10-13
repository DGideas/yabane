//! Tencent CodeBuddy (CN) Endpoint type.
//!
//! CodeBuddy's CN deployment serves an OpenAI Chat Completions surface under a
//! `/v2` root, so an Endpoint of this type keeps the administrator's own base
//! URL and adds the client-identification headers the service expects on a chat
//! request. Authentication is a pasted API key sent as `Authorization: Bearer`.
//!
//! Two service constraints shape this implementation and are stated here rather
//! than hidden behind a fallback:
//!
//! * Chat completions are streaming-only. A request without `stream: true` is
//!   answered with error `11101`, so the prepared body always asks for a stream.
//! * The service publishes no model catalog to an API-key client: `/v3/config`
//!   reports `models: null` and there is no `/models` operation. The catalog is
//!   therefore an explicit list maintained here rather than discovered over the
//!   wire or inferred from model names. Its model IDs are the ones this
//!   deployment actually serves to a key, which is why the CN catalog contains no
//!   overseas model: those answer `11102 ... only available for authorized users`.

use http::{HeaderMap, HeaderName, HeaderValue, header};
use yabane_extension_api::{
    EXTENSION_API_VERSION, Extension, HookStage, Protocol, ProviderCredentialKind,
    ProviderEndpoint, ProviderEndpointMaterial, ProviderEndpointRequest, ProviderEndpointType,
};

pub const ID: &str = "codebuddy";
pub const ENDPOINT_TYPE: &str = "codebuddy_cn";

/// The identity this Endpoint type owns: an API key the administrator pastes.
/// Core stores it generically and hands it back for this Endpoint to place on
/// the wire, so Core never assumes how a CodeBuddy key authenticates.
const CREDENTIAL_KINDS: &[ProviderCredentialKind] = &[ProviderCredentialKind {
    id: "codebuddy_api_key",
    label: "API key",
    flow: yabane_extension_api::CredentialFlow::Secret,
}];

/// The deployment this Endpoint type talks to. The base URL is not fixed here:
/// an administrator may point at a staging or self-hosted deployment, and the
/// version segment is preserved by Core's URL join, so `https://host/v2` is
/// called at `/v2/chat/completions` rather than `/v2/v1/chat/completions`.
const DEFAULT_BASE_URL_HINT: &str = "https://copilot.tencent.com/v2";

/// Models this deployment serves to a CodeBuddy API key.
///
/// There is no `/models` operation, and `/v3/config` answers an API-key client
/// with `models: null`, so this catalog is an explicit list maintained here
/// rather than discovered over the wire or read from the bundled product
/// configuration file, which is only an offline fallback that goes stale.
///
/// The entries below are the conversational models (`craft`, `ask`, and `plan`
/// all advertise the same set) confirmed against the live CN service. Auxiliary
/// models — completion, commit-message, image, and similar — are deliberately
/// absent because they do not serve a chat-completions request. The service
/// refuses overseas models for this credential kind, so none is offered.
const MODELS: &[&str] = &[
    "auto",
    "deepseek-v4-pro",
    "deepseek-v4.1-flash",
    "glm-5.1",
    "glm-5.2",
    "glm-5.3",
    "glm-5.3-flash",
    "glm-5v-turbo",
    "hy3",
    "hy3-x",
    "hy4-preview-f",
    "kimi-k2.6",
    "kimi-k2.7",
    "kimi-k3-1",
    "minimax-m3",
];

/// How this Endpoint identifies itself as a CodeBuddy client. The service reads
/// these as metadata; a request is accepted without them, but sending them keeps
/// Yabane's traffic indistinguishable from the official client's shape.
///
/// The official client resolves each of these from its client-info provider and
/// falls back to the product configuration: the IDE type and name default to the
/// product platform (`CLI`) and the version to the product version, which is the
/// package version when the product configuration names no other one. Only the
/// IDE version has a different last resort (`0.0.0`), which this Endpoint never
/// reaches because it states the version it ships as.
const IDE_TYPE: &str = "CLI";
const IDE_VERSION: &str = "2.156.0";
const AGENT_INTENT: &str = "craft";
const AGENT_TYPE: &str = "main";

/// Header names CodeBuddy clients use. Header names are case-insensitive on the
/// wire and `HeaderName::from_static` requires lowercase, so these are written
/// lowercase; the official clients spell the same names in title case.
const HEADER_IDE_TYPE: &str = "x-ide-type";
const HEADER_IDE_NAME: &str = "x-ide-name";
const HEADER_IDE_VERSION: &str = "x-ide-version";
const HEADER_AGENT_INTENT: &str = "x-agent-intent";
const HEADER_AGENT_TYPE: &str = "x-agent-type";
const HEADER_CONVERSATION_ID: &str = "x-conversation-id";
const HEADER_CONVERSATION_REQUEST_ID: &str = "x-conversation-request-id";
const HEADER_CONVERSATION_MESSAGE_ID: &str = "x-conversation-message-id";
const HEADER_REQUEST_ID: &str = "x-request-id";
const HEADER_USER_ID: &str = "x-user-id";

/// Headers this Endpoint owns because it derives them from the request and the
/// configured key, so an inbound copy is stale by definition. A caller's
/// conversation identifiers describe the caller's session, not this exchange,
/// and a caller's `X-User-Id` must never name an identity on this account.
const ENDPOINT_OWNED_HEADERS: &[&str] = &[
    "x-ide-type",
    "x-ide-name",
    "x-ide-version",
    "x-agent-intent",
    "x-agent-type",
    "x-conversation-id",
    "x-conversation-request-id",
    "x-conversation-message-id",
    "x-request-id",
    "x-user-id",
    "user-agent",
];

pub fn metadata() -> Extension {
    Extension {
        id: ID,
        name: "Tencent CodeBuddy",
        version: env!("CARGO_PKG_VERSION"),
        api_version: EXTENSION_API_VERSION,
        description: "Adds a Tencent CodeBuddy CN Endpoint type with an API key of your own and a configurable base URL.",
        hooks: &[HookStage::ProviderEndpoint],
    }
}

pub static ENDPOINT: CodeBuddyEndpoint = CodeBuddyEndpoint;

pub struct CodeBuddyEndpoint;

impl ProviderEndpoint for CodeBuddyEndpoint {
    fn extension_id(&self) -> &'static str {
        ID
    }

    fn endpoint_type(&self) -> ProviderEndpointType {
        ProviderEndpointType {
            id: ENDPOINT_TYPE,
            display_name: "CodeBuddy (CN)",
            description: "Tencent CodeBuddy CN chat completions with an API key",
            default_endpoint_id: "codebuddy",
            // The administrator supplies the base URL, so this Endpoint type
            // fixes no connection and the console leaves the field editable.
            fixed_base_url: None,
            upstream_protocol: Protocol::OpenAiChatCompletions,
            // The service answers a chat completion with SSE and refuses a
            // non-streaming request, so a successful response is an event
            // stream regardless of what the caller asked for.
            always_event_stream: true,
            surfaces: &[Protocol::OpenAiChatCompletions],
            credential_kinds: CREDENTIAL_KINDS,
            sign_in: None,
        }
    }

    fn models(&self) -> &'static [&'static str] {
        MODELS
    }

    fn prepare_request(&self, request: ProviderEndpointRequest<'_>) -> Result<(), String> {
        let credential = request
            .credential
            .ok_or_else(|| "This CodeBuddy Endpoint has no API key configured".to_owned())?;
        let ProviderEndpointMaterial::Secret { secret } = credential.material else {
            return Err("A CodeBuddy Endpoint needs a pasted API key".to_owned());
        };

        // Read the caller's conversation and correlation values before clearing
        // the headers this Endpoint owns. A caller that maintains a conversation
        // keeps its boundary; a caller that leaves any other owned header in
        // place still cannot override the value written below.
        let caller = CallerIdentifiers::from_headers(request.headers);

        strip_endpoint_owned_headers(request.headers);
        write_credentials(request.headers, secret)?;
        write_client_identity(request.headers, caller);
        write_streaming_body(request.body)?;

        Ok(())
    }
}

/// Removes headers this Endpoint is about to author, so a caller cannot inject
/// its own identity, conversation, or client-version values into the request
/// Yabane sends. Header names in a `HeaderMap` are already lowercase.
fn strip_endpoint_owned_headers(headers: &mut HeaderMap) {
    let stale: Vec<HeaderName> = headers
        .keys()
        .filter(|name| ENDPOINT_OWNED_HEADERS.contains(&name.as_str()))
        .cloned()
        .collect();
    for name in stale {
        headers.remove(name);
    }
}

/// Installs authentication and the identity derived from the configured key.
///
/// The key authenticates as itself; CodeBuddy does not exchange it for a token.
/// `X-User-Id` is metadata the official client sends as `anonymous_` plus the
/// key's last eight characters, which is a fragment of the key rather than a
/// separate secret and is derived here only so a request resembles the client
/// that owns the credential.
fn write_credentials(headers: &mut HeaderMap, secret: &str) -> Result<(), String> {
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {secret}")).map_err(|_| {
            "The configured CodeBuddy API key is not a valid header value".to_owned()
        })?,
    );
    headers.insert(
        HeaderName::from_static(HEADER_USER_ID),
        HeaderValue::from_str(&anonymous_user_id(secret))
            .map_err(|_| "Could not derive an identity from the configured API key".to_owned())?,
    );
    Ok(())
}

/// `anonymous_` followed by the key's last eight characters, as the official
/// client identifies an API-key session. A key shorter than eight characters
/// yields the whole key rather than a fabricated suffix.
fn anonymous_user_id(secret: &str) -> String {
    let suffix = if secret.len() > 8 {
        &secret[secret.len() - 8..]
    } else {
        secret
    };
    format!("anonymous_{suffix}")
}

/// Identifiers a caller already stated, read before this Endpoint replaces the
/// headers that carry them.
#[derive(Default)]
struct CallerIdentifiers {
    /// The caller's conversation boundary. CodeBuddy's own clients keep this
    /// stable across the exchanges of one conversation, so a caller that states
    /// one is understood as continuing that conversation rather than starting a
    /// new one on every request.
    conversation_id: Option<String>,
    /// The caller's per-exchange identifier. The service echoes it back as the
    /// response `id`, so preserving it keeps a caller's own tracing intact.
    request_id: Option<String>,
}

impl CallerIdentifiers {
    fn from_headers(headers: &HeaderMap) -> Self {
        let read = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Self {
            conversation_id: read(HEADER_CONVERSATION_ID),
            request_id: read(HEADER_REQUEST_ID),
        }
    }
}

/// States Yabane as the CodeBuddy client it proxies as, and answers with the
/// request identifiers the service echoes back.
///
/// `prepare_request` is given no conversation state, so a caller's own
/// conversation and request identifiers are the only boundary available: the
/// caller keeps the conversation it declared, and an exchange with no declared
/// conversation is its own. The message identifier is always fresh, because it
/// names this exchange and nothing else.
fn write_client_identity(headers: &mut HeaderMap, caller: CallerIdentifiers) {
    let message_id = generated_message_id();
    let conversation_id = caller.conversation_id.unwrap_or_else(|| message_id.clone());
    let request_id = caller.request_id.unwrap_or_else(|| message_id.clone());
    let header = |name: &'static str, value: &str| {
        (
            HeaderName::from_static(name),
            HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static("")),
        )
    };
    for (name, value) in [
        header(HEADER_REQUEST_ID, &request_id),
        header(HEADER_CONVERSATION_MESSAGE_ID, &message_id),
        header(HEADER_CONVERSATION_ID, &conversation_id),
        header(HEADER_CONVERSATION_REQUEST_ID, &message_id),
        header(HEADER_IDE_TYPE, IDE_TYPE),
        header(HEADER_IDE_NAME, IDE_TYPE),
        header(HEADER_IDE_VERSION, IDE_VERSION),
        header(HEADER_AGENT_INTENT, AGENT_INTENT),
        header(HEADER_AGENT_TYPE, AGENT_TYPE),
    ] {
        headers.insert(name, value);
    }
}

/// The message identifier for one exchange, in the shape the official client
/// sends: a UUIDv7 encoded as 32 lowercase hexadecimal characters with no
/// hyphens. The service validates that shape, and a UUIDv7 leads with a
/// millisecond timestamp so successive exchanges increase instead of colliding
/// the way a random identifier can.
fn generated_message_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

/// Forces the streaming request the service requires.
///
/// Chat completions are streaming-only here: a request without `stream: true`
/// is refused with error `11101`. Yabane's own streaming behavior already
/// handles an always-event-stream Endpoint, so the caller may ask for one
/// response and still receive a correct aggregate; this only makes the wire
/// request one the service accepts.
fn write_streaming_body(body: &mut Vec<u8>) -> Result<(), String> {
    let mut value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|error| format!("The request body is not valid JSON: {error}"))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "The request body must be a JSON object".to_owned())?;
    object.insert("stream".to_owned(), serde_json::Value::Bool(true));
    let encoded = serde_json::to_vec(&value)
        .map_err(|error| format!("Could not serialize the request: {error}"))?;
    *body = encoded;
    Ok(())
}

/// The base URL a new Endpoint of this type suggests. Exposed for the console
/// and tests; the administrator remains free to change it.
pub const SUGGESTED_BASE_URL: &str = DEFAULT_BASE_URL_HINT;

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue};
    use yabane_extension_api::{
        Protocol, ProviderEndpoint, ProviderEndpointMaterial, ProviderEndpointRequest,
    };

    use super::{
        AGENT_INTENT, AGENT_TYPE, CodeBuddyEndpoint, ENDPOINT_TYPE, IDE_TYPE, IDE_VERSION,
        anonymous_user_id,
    };

    fn request<'a>(
        headers: &'a mut HeaderMap,
        body: &'a mut Vec<u8>,
        secret: &'a str,
    ) -> ProviderEndpointRequest<'a> {
        ProviderEndpointRequest {
            headers,
            body,
            target_path: Box::leak(Box::new(String::from("/v1/chat/completions"))),
            credential: Some(yabane_extension_api::ProviderEndpointCredential {
                kind: "codebuddy_api_key",
                material: ProviderEndpointMaterial::Secret { secret },
            }),
        }
    }

    fn prepare(headers: &mut HeaderMap, body: &mut Vec<u8>, secret: &str) {
        CodeBuddyEndpoint
            .prepare_request(request(headers, body, secret))
            .expect("request preparation succeeds");
    }

    /// ENDPOINT-45: the declaration fixes no connection and serves Chat only, so
    /// the administrator supplies the deployment's base URL and keeps it editable.
    #[test]
    fn the_declaration_owns_no_connection_and_serves_chat_only() {
        let declaration = CodeBuddyEndpoint.endpoint_type();
        assert_eq!(declaration.id, ENDPOINT_TYPE);
        assert!(
            declaration.fixed_base_url.is_none(),
            "an administrator supplies the base URL, so the console must keep it editable"
        );
        assert_eq!(
            declaration.upstream_protocol,
            Protocol::OpenAiChatCompletions
        );
        assert_eq!(declaration.surfaces, &[Protocol::OpenAiChatCompletions]);
        assert!(declaration.always_event_stream);
        assert_eq!(declaration.credential_kinds.len(), 1);
        assert!(declaration.sign_in.is_none());
    }

    /// ENDPOINT-45: this Endpoint type serves one catalog and it contains only the
    /// conversational models this credential kind can call, so it offers no overseas
    /// model and no auxiliary model that cannot serve a chat-completions request.
    #[test]
    fn the_catalog_holds_only_models_this_credential_kind_may_call() {
        let models = CodeBuddyEndpoint.models();
        assert!(!models.is_empty());
        for overseas in ["gpt-5.5", "gpt-5.4", "gemini-3.1-pro", "gemini-3.0-flash"] {
            assert!(
                !models.contains(&overseas),
                "{overseas} is refused for an API key and must not be offered"
            );
        }
        for auxiliary in [
            "codewise-completions",
            "codewise-rewrite",
            "codewise-jump",
            "nes-gf",
            "hunyuan-image-alpha",
        ] {
            assert!(
                !models.contains(&auxiliary),
                "{auxiliary} does not serve chat completions"
            );
        }
        for model in models {
            assert!(!model.is_empty());
        }
    }

    /// PROXY-64: every request carries the configured key and the identity derived
    /// from it, plus the client-identification headers the service expects.
    #[test]
    fn every_request_carries_the_configured_key_and_a_derived_identity() {
        let mut headers = HeaderMap::new();
        let mut body = br#"{"model":"glm-5.0","messages":[]}"#.to_vec();
        prepare(&mut headers, &mut body, "ck_examplekey12345678");

        assert_eq!(headers["authorization"], "Bearer ck_examplekey12345678");
        assert_eq!(headers["x-user-id"], "anonymous_12345678");
        assert_eq!(headers["x-ide-type"], IDE_TYPE);
        assert_eq!(headers["x-ide-name"], IDE_TYPE);
        assert_eq!(headers["x-ide-version"], IDE_VERSION);
        assert_eq!(headers["x-agent-intent"], AGENT_INTENT);
        assert_eq!(headers["x-agent-type"], AGENT_TYPE);
    }

    /// A key shorter than the suffix this Endpoint reports is used whole rather
    /// than padded or truncated into a value that names no real key.
    #[test]
    fn a_short_key_yields_a_whole_key_identity() {
        assert_eq!(anonymous_user_id("short"), "anonymous_short");
        assert_eq!(anonymous_user_id("exactly8"), "anonymous_exactly8");
        assert_eq!(anonymous_user_id("longerthaneight"), "anonymous_haneight");
    }

    /// PROXY-64: a body without `stream` is refused by the service with `11101`, so
    /// the prepared body must always ask for a stream.
    #[test]
    fn a_non_streaming_body_is_prepared_as_streaming() {
        let mut headers = HeaderMap::new();
        let mut body = br#"{"model":"glm-5.0","messages":[]}"#.to_vec();
        prepare(&mut headers, &mut body, "ck_examplekey12345678");
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["stream"], serde_json::Value::Bool(true));
        assert_eq!(value["model"], "glm-5.0");
    }

    #[test]
    fn an_explicitly_false_stream_is_still_prepared_as_streaming() {
        let mut headers = HeaderMap::new();
        let mut body = br#"{"stream":false,"model":"glm-5.0"}"#.to_vec();
        prepare(&mut headers, &mut body, "ck_examplekey12345678");
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["stream"], serde_json::Value::Bool(true));
    }

    /// PROXY-64: a caller must not be able to name an identity or a client version
    /// on the request this Endpoint sends. A caller's declared conversation boundary
    /// is a different thing and is covered by its own test: it is the one owned
    /// header this Endpoint keeps.
    #[test]
    fn a_caller_cannot_state_an_identity_or_a_client_version() {
        let mut headers = HeaderMap::new();
        headers.insert("x-user-id", HeaderValue::from_static("someone-else"));
        headers.insert("x-ide-version", HeaderValue::from_static("9.9.9"));
        headers.insert("x-ide-type", HeaderValue::from_static("VSCode"));
        headers.insert("x-agent-type", HeaderValue::from_static("subagent"));
        headers.insert("x-agent-intent", HeaderValue::from_static("ask"));
        headers.insert("user-agent", HeaderValue::from_static("caller-agent"));
        // A header this Endpoint does not own is left for the caller.
        headers.insert("x-caller-note", HeaderValue::from_static("kept"));

        let mut body = br#"{"model":"glm-5.0","messages":[]}"#.to_vec();
        prepare(&mut headers, &mut body, "ck_examplekey12345678");

        assert_eq!(headers["x-user-id"], "anonymous_12345678");
        assert_eq!(headers["x-ide-version"], IDE_VERSION);
        assert_eq!(headers["x-ide-type"], IDE_TYPE);
        assert_eq!(headers["x-agent-type"], AGENT_TYPE);
        assert_eq!(headers["x-agent-intent"], AGENT_INTENT);
        assert!(!headers.contains_key("user-agent"));
        assert_eq!(headers["x-caller-note"], "kept");
    }

    /// The identifiers this Endpoint states are always present and never empty,
    /// because the service addresses a chat request by them.
    #[test]
    fn every_stated_identifier_is_present_and_non_empty() {
        let mut headers = HeaderMap::new();
        let mut body = br#"{"model":"glm-5.0"}"#.to_vec();
        prepare(&mut headers, &mut body, "ck_examplekey12345678");
        for name in [
            "x-request-id",
            "x-conversation-id",
            "x-conversation-request-id",
            "x-conversation-message-id",
        ] {
            let value = headers[name].to_str().unwrap();
            assert!(!value.is_empty(), "{name} was left empty");
        }
    }

    /// An Endpoint with no key cannot be prepared, and the request fails
    /// instead of being sent anonymously.
    #[test]
    fn a_missing_key_is_reported_rather_than_sent_anonymously() {
        let mut headers = HeaderMap::new();
        let mut body = br#"{"model":"glm-5.0"}"#.to_vec();
        let mut request = request(&mut headers, &mut body, "unused");
        request.credential = None;
        let error = CodeBuddyEndpoint.prepare_request(request).unwrap_err();
        assert!(error.contains("no API key"));
        assert!(!headers.contains_key("authorization"));
    }

    /// Only the declared kind is accepted, so a subscription-shaped material
    /// cannot be mistaken for this Endpoint's pasted key.
    #[test]
    fn subscription_material_is_rejected() {
        let mut headers = HeaderMap::new();
        let mut body = br#"{"model":"glm-5.0"}"#.to_vec();
        let mut request = request(&mut headers, &mut body, "unused");
        request.credential = Some(yabane_extension_api::ProviderEndpointCredential {
            kind: "codebuddy_api_key",
            material: ProviderEndpointMaterial::Subscription {
                access_token: "token",
                account_id: "account",
            },
        });
        let error = CodeBuddyEndpoint.prepare_request(request).unwrap_err();
        assert!(error.contains("pasted API key"));
    }

    /// A body that is not a JSON object fails visibly instead of being sent
    /// unchanged to a service that would reject it.
    #[test]
    fn a_body_that_is_not_a_json_object_is_reported() {
        let mut headers = HeaderMap::new();
        let mut body = b"not json".to_vec();
        let error = CodeBuddyEndpoint
            .prepare_request(request(&mut headers, &mut body, "ck_examplekey12345678"))
            .unwrap_err();
        assert!(error.contains("not valid JSON"), "{error}");
    }

    /// PROXY-64: a caller's own correlation value survives, because the service
    /// echoes `X-Request-Id` back as the response `id` and a caller tracing its own
    /// exchanges must be able to match them.
    #[test]
    fn a_caller_request_id_is_preserved_when_present() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("caller-trace"));
        let mut body = br#"{"model":"glm-5.0"}"#.to_vec();
        prepare(&mut headers, &mut body, "ck_examplekey12345678");
        assert_eq!(headers["x-request-id"], "caller-trace");
        // The message identifier still names this exchange alone.
        assert_ne!(headers["x-conversation-message-id"], "caller-trace");
        assert_ne!(headers["x-conversation-request-id"], "caller-trace");
    }

    /// PROXY-64: a conversation the caller declared is kept, so several exchanges of
    /// one conversation are not reported to the service as unrelated sessions.
    #[test]
    fn a_declared_conversation_is_continued() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-conversation-id",
            HeaderValue::from_static("0e37df36f0e23361ad6e1b6f1a9f11b9"),
        );
        let mut body = br#"{"model":"glm-5.0"}"#.to_vec();
        prepare(&mut headers, &mut body, "ck_examplekey12345678");
        assert_eq!(
            headers["x-conversation-id"],
            "0e37df36f0e23361ad6e1b6f1a9f11b9"
        );
    }

    /// PROXY-64: each exchange gets its own message identifier in the shape the
    /// service validates, and an exchange that declared no conversation is a
    /// conversation of its own.
    #[test]
    fn a_generated_identifier_is_a_fresh_uuid_v7() {
        let mut first = HeaderMap::new();
        let mut first_body = br#"{"model":"glm-5.0"}"#.to_vec();
        prepare(&mut first, &mut first_body, "ck_examplekey12345678");

        let mut second = HeaderMap::new();
        let mut second_body = br#"{"model":"glm-5.0"}"#.to_vec();
        prepare(&mut second, &mut second_body, "ck_examplekey12345678");

        let first_id = first["x-conversation-message-id"].to_str().unwrap();
        let second_id = second["x-conversation-message-id"].to_str().unwrap();
        assert_ne!(
            first_id, second_id,
            "each exchange names itself with its own identifier"
        );
        // 32 lowercase hex characters with no hyphens, as the official client
        // encodes them, with a version-7 UUID in the 13th digit and an RFC 4122
        // variant in the 17th, which is the shape the service validates.
        for value in [first_id, second_id] {
            assert_eq!(value.len(), 32, "{value}");
            assert!(
                value
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{value}"
            );
            assert_eq!(value.as_bytes()[12], b'7', "{value}");
            assert!(
                matches!(value.as_bytes()[16], b'8' | b'9' | b'a' | b'b'),
                "{value}"
            );
        }

        // The message identifier is also the request identifier, and an
        // exchange with no declared conversation is a conversation of its own.
        assert_eq!(first["x-request-id"], first_id);
        assert_eq!(first["x-conversation-request-id"], first_id);
        assert_eq!(first["x-conversation-id"], first_id);
    }
}

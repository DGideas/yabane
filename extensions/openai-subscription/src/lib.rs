use std::{sync::OnceLock, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http::{HeaderMap, HeaderName, HeaderValue, header};
use rand::RngCore as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use yabane_extension_api::{
    BrowserAuthorization, DeviceAuthorization, EXTENSION_API_VERSION, Extension, HookStage,
    Protocol, ProviderCredentialKind, ProviderEndpoint, ProviderEndpointMaterial,
    ProviderEndpointRequest, ProviderEndpointType, ProviderSignIn, SubscriptionCredential,
    SubscriptionProvider,
};

pub const ID: &str = "openai-subscription";
pub const ENDPOINT_TYPE: &str = "openai_codex";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_BASE_URL: &str = "https://auth.openai.com";
/// The fixed base URL this Endpoint type always talks to. Core stores it in
/// configuration and reads wire behavior from here instead of assuming one.
pub const BASE_URL: &str = "https://chatgpt.com/backend-api";
const BROWSER_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const BROWSER_SCOPE: &str = "openid profile email offline_access";
const BROWSER_TIMEOUT_SECONDS: u64 = 15 * 60;
const DEVICE_TIMEOUT_SECONDS: u64 = 15 * 60;
const OAUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";

/// The identity this Endpoint type owns: a ChatGPT account connected through
/// OpenAI sign-in. The identifier and label are declared here, not in Core.
const CREDENTIAL_KINDS: &[ProviderCredentialKind] = &[ProviderCredentialKind {
    id: "openai_subscription",
    label: "OAuth account",
    flow: yabane_extension_api::CredentialFlow::Subscription,
}];

// ChatGPT's Codex backend has no supported /models operation, so keep this catalog explicit
// and update it from pi-ai's OpenAI Codex provider catalog.
const MODELS: &[&str] = &[
    "gpt-5.3-codex-spark",
    "gpt-5.5",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-6-astra",
    "gpt-6-luna",
    "gpt-6-sol",
];

pub fn metadata() -> Extension {
    Extension {
        id: ID,
        name: "OpenAI Subscription",
        version: env!("CARGO_PKG_VERSION"),
        api_version: EXTENSION_API_VERSION,
        description: "Connects ChatGPT Plus or Pro subscriptions through OpenAI device authorization and the Codex Responses backend.",
        hooks: &[HookStage::ProviderEndpoint],
    }
}

pub static ENDPOINT: OpenAiSubscriptionEndpoint = OpenAiSubscriptionEndpoint;

pub struct OpenAiSubscriptionEndpoint;

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    user_code: String,
    interval: serde_json::Value,
}

#[derive(Deserialize)]
struct DeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct JwtClaims {
    #[serde(rename = "https://api.openai.com/auth")]
    auth: Option<JwtAuthClaims>,
}

#[derive(Deserialize)]
struct JwtAuthClaims {
    chatgpt_account_id: Option<String>,
}

/// Header name prefixes that only ever describe how a request travelled to Yabane.
/// A reverse proxy such as Cloudflare injects them, the Codex backend rejects requests
/// carrying them, and no caller can legitimately set them for this Endpoint.
const PROXY_METADATA_PREFIXES: &[&str] = &["cf-", "x-forwarded-"];

/// Reverse-proxy metadata headers that share no common prefix.
const PROXY_METADATA_NAMES: &[&str] = &["cdn-loop", "forwarded", "via"];

/// Headers this Endpoint derives from the request body and the connected subscription,
/// so every inbound value is stale by definition and is cleared before being reset.
const ENDPOINT_OWNED_HEADERS: &[&str] = &[
    "content-encoding",
    "user-agent",
    "session-id",
    "x-client-request-id",
];

/// Clears reverse-proxy metadata and Endpoint-owned headers before `prepare_request`
/// installs the Codex wire identity. Yabane Core stays a transparent proxy and never
/// inspects an Endpoint's API type to decide what a caller header means, so this
/// Endpoint owns the whole removal itself. Header names in a `HeaderMap` are already
/// lowercase, so the literal comparisons below need no further normalization.
fn strip_inbound_headers(headers: &mut HeaderMap) {
    let stale: Vec<HeaderName> = headers
        .keys()
        .filter(|name| {
            let name = name.as_str();
            ENDPOINT_OWNED_HEADERS.contains(&name)
                || PROXY_METADATA_NAMES.contains(&name)
                || PROXY_METADATA_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        })
        .cloned()
        .collect();
    for name in stale {
        headers.remove(name);
    }
}

impl ProviderEndpoint for OpenAiSubscriptionEndpoint {
    fn extension_id(&self) -> &'static str {
        ID
    }

    fn endpoint_type(&self) -> ProviderEndpointType {
        ProviderEndpointType {
            id: ENDPOINT_TYPE,
            display_name: "OpenAI subscription",
            description: "Responses API with ChatGPT Plus or Pro",
            default_endpoint_id: "chatgpt",
            fixed_base_url: Some(BASE_URL),
            upstream_protocol: Protocol::OpenAiResponses,
            always_event_stream: true,
            surfaces: &[Protocol::OpenAiResponses],
            credential_kinds: CREDENTIAL_KINDS,
            sign_in: Some(ProviderSignIn {
                device_code: true,
                browser: true,
            }),
        }
    }

    fn models(&self) -> &'static [&'static str] {
        MODELS
    }

    fn prepare_request(&self, request: ProviderEndpointRequest<'_>) -> Result<(), String> {
        let credential = request
            .credential
            .ok_or_else(|| "OpenAI subscription is not connected".to_owned())?;
        let ProviderEndpointMaterial::Subscription {
            access_token,
            account_id,
        } = credential.material
        else {
            return Err("OpenAI subscription needs a signed-in account".to_owned());
        };
        strip_inbound_headers(request.headers);
        request.headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        request.headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {access_token}"))
                .map_err(|_| "OpenAI subscription access token is invalid".to_owned())?,
        );
        request.headers.insert(
            HeaderName::from_static("chatgpt-account-id"),
            HeaderValue::from_str(account_id)
                .map_err(|_| "OpenAI subscription account ID is invalid".to_owned())?,
        );
        request.headers.insert(
            HeaderName::from_static("originator"),
            HeaderValue::from_static("pi"),
        );
        request
            .headers
            .insert(header::USER_AGENT, pi_user_agent().clone());
        request.headers.insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("responses=experimental"),
        );
        request.headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        request.headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );

        adapt_body(request.body);
        if let Some(session_id) = session_id(request.body) {
            request
                .headers
                .insert(HeaderName::from_static("session-id"), session_id.clone());
            request
                .headers
                .insert(HeaderName::from_static("x-client-request-id"), session_id);
        }
        *request.target_path = "/codex/responses".to_owned();
        Ok(())
    }
}

impl SubscriptionProvider for OpenAiSubscriptionEndpoint {
    fn start_browser_authorization(&self) -> Result<BrowserAuthorization, String> {
        let mut verifier_bytes = [0_u8; 32];
        rand::rng().fill_bytes(&mut verifier_bytes);
        let code_verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let mut state_bytes = [0_u8; 16];
        rand::rng().fill_bytes(&mut state_bytes);
        let state = state_bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let query = serde_urlencoded::to_string([
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", BROWSER_REDIRECT_URI),
            ("scope", BROWSER_SCOPE),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "pi"),
        ])
        .map_err(|_| "Could not construct OpenAI authorization URL".to_owned())?;
        Ok(BrowserAuthorization {
            authorization_url: format!("{AUTH_BASE_URL}/oauth/authorize?{query}"),
            state,
            code_verifier,
            expires_in_seconds: BROWSER_TIMEOUT_SECONDS,
        })
    }

    fn exchange_browser_authorization<'a>(
        &'a self,
        client: &'a reqwest::Client,
        code: &'a str,
        code_verifier: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<SubscriptionCredential, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            let response = client
                .post(format!("{AUTH_BASE_URL}/oauth/token"))
                .form(&[
                    ("grant_type", "authorization_code"),
                    ("client_id", CLIENT_ID),
                    ("code", code),
                    ("code_verifier", code_verifier),
                    ("redirect_uri", BROWSER_REDIRECT_URI),
                ])
                .timeout(OAUTH_REQUEST_TIMEOUT)
                .send()
                .await
                .map_err(|error| format!("OpenAI token exchange failed: {error}"))?;
            token_response(response, "exchange").await
        })
    }

    fn start_device_authorization<'a>(
        &'a self,
        client: &'a reqwest::Client,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<DeviceAuthorization, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            let response = client
                .post(format!("{AUTH_BASE_URL}/api/accounts/deviceauth/usercode"))
                .json(&serde_json::json!({"client_id": CLIENT_ID}))
                .timeout(OAUTH_REQUEST_TIMEOUT)
                .send()
                .await
                .map_err(|error| format!("Could not start OpenAI sign-in: {error}"))?;
            let status = response.status();
            let body = response
                .bytes()
                .await
                .map_err(|error| format!("Could not read OpenAI sign-in response: {error}"))?;
            if !status.is_success() {
                return Err(format!("OpenAI sign-in returned {status}"));
            }
            let device: DeviceCodeResponse = serde_json::from_slice(&body)
                .map_err(|_| "OpenAI returned an invalid device sign-in response".to_owned())?;
            let interval_seconds = match device.interval {
                serde_json::Value::Number(value) => value.as_u64(),
                serde_json::Value::String(value) => value.parse().ok(),
                _ => None,
            }
            .ok_or_else(|| "OpenAI returned an invalid polling interval".to_owned())?
            .max(1);
            Ok(DeviceAuthorization {
                device_auth_id: device.device_auth_id,
                user_code: device.user_code,
                verification_uri: VERIFICATION_URI,
                interval_seconds,
                expires_in_seconds: DEVICE_TIMEOUT_SECONDS,
            })
        })
    }

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
    > {
        Box::pin(async move {
            let response = client
                .post(format!("{AUTH_BASE_URL}/api/accounts/deviceauth/token"))
                .json(
                    &serde_json::json!({"device_auth_id": device_auth_id, "user_code": user_code}),
                )
                .timeout(OAUTH_REQUEST_TIMEOUT)
                .send()
                .await
                .map_err(|error| format!("OpenAI sign-in polling failed: {error}"))?;
            if matches!(
                response.status(),
                reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::NOT_FOUND
            ) {
                return Ok(None);
            }
            let status = response.status();
            let body = response
                .bytes()
                .await
                .map_err(|error| format!("Could not read OpenAI sign-in status: {error}"))?;
            if !status.is_success() {
                return Err(format!("OpenAI sign-in failed with {status}"));
            }
            let code: DeviceTokenResponse = serde_json::from_slice(&body)
                .map_err(|_| "OpenAI returned an invalid sign-in completion".to_owned())?;
            let response = client
                .post(format!("{AUTH_BASE_URL}/oauth/token"))
                .form(&[
                    ("grant_type", "authorization_code"),
                    ("client_id", CLIENT_ID),
                    ("code", code.authorization_code.as_str()),
                    ("code_verifier", code.code_verifier.as_str()),
                    (
                        "redirect_uri",
                        "https://auth.openai.com/deviceauth/callback",
                    ),
                ])
                .timeout(OAUTH_REQUEST_TIMEOUT)
                .send()
                .await
                .map_err(|error| format!("OpenAI token exchange failed: {error}"))?;
            token_response(response, "exchange").await.map(Some)
        })
    }

    fn refresh_credential<'a>(
        &'a self,
        client: &'a reqwest::Client,
        refresh_token: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<SubscriptionCredential, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            let response = client
                .post(format!("{AUTH_BASE_URL}/oauth/token"))
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token),
                    ("client_id", CLIENT_ID),
                ])
                .timeout(OAUTH_REQUEST_TIMEOUT)
                .send()
                .await
                .map_err(|error| format!("OpenAI token refresh failed: {error}"))?;
            token_response(response, "refresh").await
        })
    }
}

async fn token_response(
    response: reqwest::Response,
    operation: &str,
) -> Result<SubscriptionCredential, String> {
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("Could not read OpenAI token response: {error}"))?;
    if !status.is_success() {
        return Err(format!("OpenAI token {operation} failed with {status}"));
    }
    let token: TokenResponse = serde_json::from_slice(&body)
        .map_err(|_| format!("OpenAI token {operation} returned an invalid response"))?;
    let account_id = account_id(&token.access_token)?;
    HeaderValue::from_str(&account_id)
        .map_err(|_| "OpenAI access token contains an invalid ChatGPT account ID".to_owned())?;
    Ok(SubscriptionCredential {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: unix_now().saturating_add(token.expires_in),
        account_id,
    })
}

fn account_id(token: &str) -> Result<String, String> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| "OpenAI access token is not a JWT".to_owned())?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| "OpenAI access token has invalid JWT encoding".to_owned())?;
    let claims: JwtClaims = serde_json::from_slice(&bytes)
        .map_err(|_| "OpenAI access token has invalid JWT claims".to_owned())?;
    claims
        .auth
        .and_then(|auth| auth.chatgpt_account_id)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "OpenAI access token does not contain a ChatGPT account ID".to_owned())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Reads the text of a Responses input message, whether it uses the string
/// shorthand or the list of `input_text` parts.
fn input_message_text(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

pub fn adapt_body(body: &mut Vec<u8>) {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return;
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("store".to_owned(), serde_json::Value::Bool(false));
        object.insert("stream".to_owned(), serde_json::Value::Bool(true));
        // The Codex backend rejects the `system` role anywhere in `input`, so
        // every instruction message is folded into `instructions` the way
        // pi-ai replays its transcript, whatever content shape it arrives in.
        let mut system_instructions = Vec::new();
        if let Some(input) = object
            .get_mut("input")
            .and_then(serde_json::Value::as_array_mut)
        {
            input.retain(|item| {
                let Some(message) = item.as_object() else {
                    return true;
                };
                if !matches!(
                    message.get("role").and_then(serde_json::Value::as_str),
                    Some("developer" | "system")
                ) {
                    return true;
                }
                if let Some(text) = message
                    .get("content")
                    .and_then(input_message_text)
                    .filter(|text| !text.is_empty())
                {
                    system_instructions.push(text);
                }
                false
            });
        }
        if object
            .get("instructions")
            .and_then(serde_json::Value::as_str)
            .is_none()
        {
            let instructions = if system_instructions.is_empty() {
                "You are a helpful assistant.".to_owned()
            } else {
                system_instructions.join("\n\n")
            };
            object.insert(
                "instructions".to_owned(),
                serde_json::Value::String(instructions),
            );
        }
        if let Some(input) = object
            .get("input")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        {
            object.insert(
                "input".to_owned(),
                serde_json::json!([{"role": "user", "content": [{"type": "input_text", "text": input}]}]),
            );
        }
        // Tuning, cache, truncation, and caller-identifier fields the Codex
        // backend answers with `400 Unsupported parameter: <name>`. A field
        // whose absence would change the answer, such as
        // `previous_response_id`, stays in the body so the Provider's own
        // error reaches the caller instead of silently dropping behavior.
        for name in [
            "max_output_tokens",
            "temperature",
            "top_p",
            "truncation",
            "metadata",
            "service_tier",
            "user",
            "safety_identifier",
            "prompt_cache_options",
            "prompt_cache_retention",
        ] {
            object.remove(name);
        }
        object
            .entry("text")
            .or_insert_with(|| serde_json::json!({"verbosity": "low"}));
        object
            .entry("tool_choice")
            .or_insert_with(|| serde_json::Value::String("auto".to_owned()));
        object
            .entry("parallel_tool_calls")
            .or_insert(serde_json::Value::Bool(true));
        let include = object
            .entry("include")
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        if !include.is_array() {
            *include = serde_json::Value::Array(Vec::new());
        }
        let items = include.as_array_mut().expect("normalized include array");
        if !items
            .iter()
            .any(|item| item.as_str() == Some("reasoning.encrypted_content"))
        {
            items.push(serde_json::Value::String(
                "reasoning.encrypted_content".to_owned(),
            ));
        }
    }
    *body = serde_json::to_vec(&value).expect("serialize OpenAI subscription request");
}

pub fn session_id(body: &[u8]) -> Option<HeaderValue> {
    let value = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let key = value.get("prompt_cache_key")?.as_str()?;
    if key.is_empty() {
        return None;
    }
    HeaderValue::from_str(&key.chars().take(64).collect::<String>()).ok()
}

pub fn pi_user_agent() -> &'static HeaderValue {
    static USER_AGENT: OnceLock<HeaderValue> = OnceLock::new();
    USER_AGENT.get_or_init(|| {
        let platform = match std::env::consts::OS {
            "macos" => "darwin",
            platform => platform,
        };
        let architecture = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "x64",
            architecture => architecture,
        };
        let release = std::process::Command::new("uname")
            .arg("-r")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|release| release.trim().to_owned())
            .filter(|release| !release.is_empty());
        let value = match release {
            Some(release) => format!("pi ({platform} {release}; {architecture})"),
            None => format!("pi ({platform}; {architecture})"),
        };
        HeaderValue::from_str(&value).expect("pi user agent is a valid header value")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: &str) -> String {
        format!("header.{}.signature", URL_SAFE_NO_PAD.encode(claims))
    }

    /// DISCOVERY-06: this Endpoint type serves one catalog, and it mirrors pi-ai's
    /// `openai-codex` catalog because the Codex backend has no `/models` operation
    /// to keep it honest. GPT-5.4 and GPT-5.4 mini were removed because the backend
    /// stopped serving them, so an entry pi-ai drops must disappear here too;
    /// GPT-6 Sol and GPT-6 Luna must appear as soon as pi-ai adds them.
    #[test]
    fn the_catalog_mirrors_pi_ai_openai_codex_models() {
        assert_eq!(
            ENDPOINT.models(),
            [
                "gpt-5.3-codex-spark",
                "gpt-5.5",
                "gpt-5.6-luna",
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-6-astra",
                "gpt-6-luna",
                "gpt-6-sol",
            ]
        );
    }

    #[test]
    fn extracts_only_a_non_empty_chatgpt_account_id() {
        let token = jwt(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"account-123"}}"#);
        assert_eq!(account_id(&token).unwrap(), "account-123");
        assert!(account_id(&jwt(r#"{"sub":"user"}"#)).is_err());
        assert!(
            account_id(&jwt(
                r#"{"https://api.openai.com/auth":{"chatgpt_account_id":""}}"#
            ))
            .is_err()
        );
        assert!(account_id("not-a-jwt").is_err());
    }

    #[test]
    fn browser_authorization_matches_pi_ai_callback_and_pkce_shape() {
        let flow = ENDPOINT.start_browser_authorization().unwrap();
        let url = reqwest::Url::parse(&flow.authorization_url).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            url.as_str().split('?').next().unwrap(),
            "https://auth.openai.com/oauth/authorize"
        );
        assert_eq!(query.get("client_id").unwrap(), CLIENT_ID);
        assert_eq!(query.get("redirect_uri").unwrap(), BROWSER_REDIRECT_URI);
        assert_eq!(query.get("scope").unwrap(), BROWSER_SCOPE);
        assert_eq!(query.get("state").unwrap(), &flow.state);
        assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(query.get("originator").unwrap(), "pi");
        assert_eq!(flow.code_verifier.len(), 43);
        assert_eq!(flow.expires_in_seconds, 900);
    }

    #[test]
    fn prepares_the_fixed_codex_wire_request_without_trusting_existing_identity() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer caller"),
        );
        headers.insert(
            HeaderName::from_static("chatgpt-account-id"),
            HeaderValue::from_static("caller-account"),
        );
        let mut body = br#"{"model":"gpt-5.4","input":"hello","store":true,"stream":false,"max_output_tokens":100,"temperature":0.2,"top_p":0.9,"truncation":"disabled","metadata":{"trace":"caller"},"service_tier":"auto","user":"caller","safety_identifier":"caller-id","prompt_cache_options":{"mode":"implicit"},"prompt_cache_retention":"24h","previous_response_id":"resp_caller","prompt_cache_key":"session"}"#.to_vec();
        let mut target_path = "/caller/path".to_owned();

        ENDPOINT
            .prepare_request(ProviderEndpointRequest {
                headers: &mut headers,
                body: &mut body,
                target_path: &mut target_path,
                credential: Some(yabane_extension_api::ProviderEndpointCredential {
                    kind: "openai_subscription",
                    material: ProviderEndpointMaterial::Subscription {
                        access_token: "upstream-token",
                        account_id: "upstream-account",
                    },
                }),
            })
            .unwrap();

        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(target_path, "/codex/responses");
        assert_eq!(headers[header::AUTHORIZATION], "Bearer upstream-token");
        assert_eq!(headers["chatgpt-account-id"], "upstream-account");
        assert_eq!(headers["originator"], "pi");
        assert_eq!(headers["session-id"], "session");
        assert_eq!(headers["x-client-request-id"], "session");
        assert_eq!(value["store"], false);
        assert_eq!(value["stream"], true);
        for dropped in [
            "max_output_tokens",
            "temperature",
            "top_p",
            "truncation",
            "metadata",
            "service_tier",
            "user",
            "safety_identifier",
            "prompt_cache_options",
            "prompt_cache_retention",
        ] {
            assert!(
                value.get(dropped).is_none(),
                "unsupported field '{dropped}' must not reach Codex"
            );
        }
        // Dropping this one would silently discard the caller's conversation.
        assert_eq!(value["previous_response_id"], "resp_caller");
        assert_eq!(value["input"][0]["content"][0]["text"], "hello");
        assert!(
            value["include"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == "reasoning.encrypted_content")
        );
    }

    #[test]
    fn folds_every_system_or_developer_input_message_into_instructions() {
        let mut body = br#"{"model":"gpt-6-astra","input":[{"type":"message","role":"system","content":[{"type":"input_text","text":"Be terse."}]},{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]},{"role":"developer","content":"Late note."}]}"#.to_vec();

        adapt_body(&mut body);

        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["instructions"], "Be terse.\n\nLate note.");
        assert_eq!(value["input"].as_array().unwrap().len(), 1);
        assert_eq!(value["input"][0]["role"], "user");
    }

    #[test]
    fn caller_instructions_win_over_input_instruction_messages() {
        let mut body = br#"{"model":"gpt-6-astra","instructions":"Caller prompt","input":[{"role":"system","content":"Input prompt"},{"role":"user","content":"hello"}]}"#.to_vec();

        adapt_body(&mut body);

        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["instructions"], "Caller prompt");
        assert_eq!(value["input"].as_array().unwrap().len(), 1);
        assert_eq!(value["input"][0]["role"], "user");
    }

    #[test]
    fn request_preparation_requires_valid_extension_owned_credentials() {
        let mut headers = http::HeaderMap::new();
        let mut body = br#"{}"#.to_vec();
        let mut target_path = String::new();
        assert!(
            ENDPOINT
                .prepare_request(ProviderEndpointRequest {
                    headers: &mut headers,
                    body: &mut body,
                    target_path: &mut target_path,
                    credential: None,
                })
                .is_err()
        );

        assert!(
            ENDPOINT
                .prepare_request(ProviderEndpointRequest {
                    headers: &mut headers,
                    body: &mut body,
                    target_path: &mut target_path,
                    credential: Some(yabane_extension_api::ProviderEndpointCredential {
                        kind: "openai_subscription",
                        material: ProviderEndpointMaterial::Subscription {
                            access_token: "invalid\nvalue",
                            account_id: "account",
                        },
                    }),
                })
                .is_err()
        );
    }

    #[test]
    fn strips_reverse_proxy_metadata_but_keeps_unrelated_caller_headers() {
        let mut headers = http::HeaderMap::new();
        // A reverse proxy may use any casing; `HeaderMap` lowercases names on insert,
        // so the prefix match must hold for every spelling the proxy chose.
        headers.insert(
            HeaderName::from_static("cf-connecting-ip"),
            HeaderValue::from_static("203.0.113.7"),
        );
        headers.insert(
            HeaderName::from_bytes(b"CF-Visitor").expect("valid header name"),
            HeaderValue::from_static(r#"{"scheme":"https"}"#),
        );
        headers.insert(
            HeaderName::from_static("cf-warp-tag-id"),
            HeaderValue::from_static("warp-tag"),
        );
        headers.insert(
            HeaderName::from_static("cf-ray"),
            HeaderValue::from_static("ray-id"),
        );
        headers.insert(
            HeaderName::from_static("cf-ipcountry"),
            HeaderValue::from_static("SG"),
        );
        headers.insert(
            HeaderName::from_static("cdn-loop"),
            HeaderValue::from_static("cloudflare; loops=1"),
        );
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::from_static("203.0.113.7"),
        );
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static("https"),
        );
        headers.insert(
            HeaderName::from_static("forwarded"),
            HeaderValue::from_static("for=203.0.113.7"),
        );
        headers.insert(
            HeaderName::from_static("via"),
            HeaderValue::from_static("1.1 proxy"),
        );
        headers.insert(
            HeaderName::from_static("accept-language"),
            HeaderValue::from_static("*"),
        );
        headers.insert(
            HeaderName::from_static("x-stainless-lang"),
            HeaderValue::from_static("js"),
        );
        headers.insert(
            HeaderName::from_static("sec-fetch-mode"),
            HeaderValue::from_static("cors"),
        );
        let mut body = br#"{"model":"gpt-5.4","input":"hello"}"#.to_vec();
        let mut target_path = "/v1/responses".to_owned();

        ENDPOINT
            .prepare_request(ProviderEndpointRequest {
                headers: &mut headers,
                body: &mut body,
                target_path: &mut target_path,
                credential: Some(yabane_extension_api::ProviderEndpointCredential {
                    kind: "openai_subscription",
                    material: ProviderEndpointMaterial::Subscription {
                        access_token: "upstream-token",
                        account_id: "upstream-account",
                    },
                }),
            })
            .unwrap();

        for name in [
            "cf-connecting-ip",
            "cf-visitor",
            "cf-warp-tag-id",
            "cf-ray",
            "cf-ipcountry",
            "cdn-loop",
            "x-forwarded-for",
            "x-forwarded-proto",
            "forwarded",
            "via",
        ] {
            assert!(
                !headers.contains_key(name),
                "reverse-proxy metadata '{name}' must not reach Codex"
            );
        }
        // Caller headers that carry no proxy metadata are still forwarded untouched.
        assert_eq!(headers["accept-language"], "*");
        assert_eq!(headers["x-stainless-lang"], "js");
        assert_eq!(headers["sec-fetch-mode"], "cors");
        assert_eq!(headers[header::AUTHORIZATION], "Bearer upstream-token");
    }

    #[test]
    fn owns_content_encoding_and_affinity_headers_without_a_prompt_cache_key() {
        let mut headers = http::HeaderMap::new();
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("zstd"));
        headers.insert(header::USER_AGENT, HeaderValue::from_static("caller-agent"));
        headers.insert(
            HeaderName::from_static("session-id"),
            HeaderValue::from_static("caller-session"),
        );
        headers.insert(
            HeaderName::from_static("x-client-request-id"),
            HeaderValue::from_static("caller-request"),
        );
        // No `prompt_cache_key`, so the Endpoint has no affinity value to install and
        // must leave these headers absent rather than forward the inbound ones.
        let mut body = br#"{"model":"gpt-5.4","input":"hello"}"#.to_vec();
        let mut target_path = "/v1/responses".to_owned();

        ENDPOINT
            .prepare_request(ProviderEndpointRequest {
                headers: &mut headers,
                body: &mut body,
                target_path: &mut target_path,
                credential: Some(yabane_extension_api::ProviderEndpointCredential {
                    kind: "openai_subscription",
                    material: ProviderEndpointMaterial::Subscription {
                        access_token: "upstream-token",
                        account_id: "upstream-account",
                    },
                }),
            })
            .unwrap();

        assert!(!headers.contains_key(header::CONTENT_ENCODING));
        assert!(!headers.contains_key("session-id"));
        assert!(!headers.contains_key("x-client-request-id"));
        assert_eq!(headers[header::USER_AGENT], pi_user_agent());
        assert_ne!(headers[header::USER_AGENT], "caller-agent");
        assert_eq!(headers[header::ACCEPT_ENCODING], "identity");
    }
}

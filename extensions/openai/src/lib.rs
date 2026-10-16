use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http::{HeaderMap, HeaderName, HeaderValue, header};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use yabane_extension_api::{
    BrowserAuthorization, BrowserAuthorizationCallback, CredentialFlow, DeviceAuthorization,
    EXTENSION_API_VERSION, Extension, HookStage, Protocol, ProviderCredentialKind,
    ProviderEndpoint, ProviderEndpointMaterial, ProviderEndpointRequest, ProviderEndpointType,
    ProviderSignIn, SubscriptionCredential, SubscriptionProvider,
};

pub const ID: &str = "openai";
pub const ENDPOINT_TYPE: &str = "openai";

/// The fixed base URL this Endpoint type always talks to. Core stores it in
/// configuration and reads wire behavior from here instead of assuming one.
pub const BASE_URL: &str = "https://api.openai.com/v1";

/// First-time registration entrypoint. OpenAI issues the client identifier this
/// installation then saves and reuses, so this value is never the one a token
/// exchange or a refresh sends.
const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";
/// The application name OpenAI shows while the user approves the agent. It is
/// display metadata, and only a first-time registration sends it.
const AGENT_NAME_HINT: &str = "Yabane";
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
/// The resource this grant may be used against; sent on authorization,
/// exchange, and refresh alike.
const RESOURCE: &str = "https://api.openai.com/v1";
/// The callback address this Endpoint type registers. The hosted API needs the
/// loopback IP spelling, which is why it differs from the ChatGPT Codex
/// Endpoint's localhost address.
const REDIRECT_URI: &str = "http://127.0.0.1:1455/auth/callback";
const DIRECT_TOKEN_SCOPE: &str = "chatgpt.tokens.use.direct";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const BROWSER_TIMEOUT_SECONDS: u64 = 15 * 60;
/// A token that expires within this margin is treated as already expired, so a
/// request never starts on one that is about to lapse.
const EXPIRY_MARGIN_SECONDS: u64 = 3 * 60;
const OAUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const STATE_FILE: &str = "data/extensions/openai/state.json";

/// The identity this Endpoint type owns: a ChatGPT account connected through
/// Sign in with ChatGPT.
const CREDENTIAL_KINDS: &[ProviderCredentialKind] = &[ProviderCredentialKind {
    id: "openai_account",
    label: "ChatGPT account",
    flow: CredentialFlow::Subscription,
}];

/// The catalog this Endpoint type offers, mirroring pi-ai's openai provider
/// catalog: the models.dev catalog filtered to entries that accept tool calls,
/// minus the alias OpenAI's APIs do not accept.
const MODELS: &[&str] = &[
    "gpt-4",
    "gpt-4-turbo",
    "gpt-4.1",
    "gpt-4.1-mini",
    "gpt-4.1-nano",
    "gpt-4o",
    "gpt-4o-2024-05-13",
    "gpt-4o-2024-08-06",
    "gpt-4o-2024-11-20",
    "gpt-4o-mini",
    "gpt-5",
    "gpt-5-mini",
    "gpt-5-nano",
    "gpt-5-pro",
    "gpt-5.1",
    "gpt-5.2",
    "gpt-5.2-chat-latest",
    "gpt-5.2-pro",
    "gpt-5.3-chat-latest",
    "gpt-5.3-codex",
    "gpt-5.3-codex-spark",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.4-nano",
    "gpt-5.4-pro",
    "gpt-5.5",
    "gpt-5.5-pro",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-6-astra",
    "gpt-6-luna",
    "gpt-6-sol",
    "gpt-6.1-sol",
    "gpt-daybreak-blue-latest",
    "gpt-daybreak-red-latest",
    "gpt-realtime-2.1",
    "o1",
    "o1-pro",
    "o3",
    "o3-mini",
    "o3-pro",
    "o4-mini",
];

/// Request fields this route refuses. pi omits exactly these when the OpenAI
/// provider is authenticated through Sign in with ChatGPT, so an identical
/// caller body reaches the hosted API with an identical wire body. Fields the
/// documentation also lists as unsupported but pi does not remove stay in the
/// request: the hosted API answers them with its own visible error instead of
/// this Endpoint quietly rewriting a caller's request.
const UNSUPPORTED_FIELDS: &[&str] = &[
    "max_output_tokens",
    "temperature",
    "prompt_cache_retention",
    "prompt_cache_options",
];

/// The version of pi's OpenAI SDK client library, reported in the group of
/// X-Stainless-* headers that describe the client and the host it runs on. They
/// carry no protocol meaning, and this Endpoint reports the same values so its
/// requests are indistinguishable from pi's on the wire.
///
/// The runtime half of that group, `X-Stainless-Runtime` and
/// `X-Stainless-Runtime-Version`, is deliberately not sent: pi's SDK fills them
/// by detecting its own Node process, so sending them would claim this request
/// came from a Node runtime that is not involved. The host half
/// (`X-Stainless-OS`, `X-Stainless-Arch`) describes the machine that made the
/// request and stays.
const STAINLESS_PACKAGE_VERSION: &str = "7.19.0";

/// Header name prefixes that only ever describe how a request travelled to
/// Yabane, so no caller can legitimately set them for this Endpoint.
const PROXY_METADATA_PREFIXES: &[&str] = &["cf-", "x-forwarded-"];
/// Reverse-proxy metadata headers that share no common prefix.
const PROXY_METADATA_NAMES: &[&str] = &["cdn-loop", "forwarded", "via"];
/// Headers this Endpoint derives from the request body and the connected
/// account, so every inbound value is stale by definition. The X-Stainless-*
/// group is included because it is a client's own description of itself, which
/// this Endpoint speaks for itself: a caller-supplied copy would describe a
/// client library and host that did not make this request, including the two
/// runtime headers this Endpoint sends no value for at all.
const ENDPOINT_OWNED_HEADERS: &[&str] = &[
    "content-encoding",
    "user-agent",
    "accept",
    "accept-encoding",
    "session_id",
    "x-client-request-id",
    "openai-organization",
    "openai-project",
    "x-stainless-lang",
    "x-stainless-package-version",
    "x-stainless-os",
    "x-stainless-arch",
    "x-stainless-runtime",
    "x-stainless-runtime-version",
    "x-stainless-retry-count",
    "x-stainless-timeout",
    "x-stainless-helper-method",
];

pub static ENDPOINT: OpenAiEndpoint = OpenAiEndpoint;

pub struct OpenAiEndpoint;

pub fn metadata() -> Extension {
    Extension {
        id: ID,
        name: "OpenAI",
        version: env!("CARGO_PKG_VERSION"),
        api_version: EXTENSION_API_VERSION,
        description: "Connects a ChatGPT subscription through Sign in with ChatGPT and calls the OpenAI Responses API.",
        hooks: &[HookStage::ProviderEndpoint],
    }
}

/// The state this Endpoint type keeps for itself. Core's credential model has
/// no room for the client identifier OpenAI issues at registration or for this
/// installation's host identifier, and neither is a Core concept, so they live
/// beside the Endpoint type that owns them. The file holds no usable token: a
/// registration is keyed by a digest of the refresh token it arrived with.
#[derive(Default, Deserialize, Serialize)]
struct RegistrationState {
    /// This installation's stable host identifier, sent as ext_agent_host_id.
    #[serde(default)]
    host_id: String,
    /// Issued client identifiers keyed by the refresh token they arrived with.
    /// Refresh tokens rotate, so an entry moves with its token rather than
    /// being pinned to a value that changes.
    #[serde(default)]
    clients: HashMap<String, Registration>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Registration {
    client_id: String,
    account_id: String,
}

/// The key a registration is stored under.
fn registration_key(refresh_token: &str) -> String {
    Sha256::digest(refresh_token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn state_file() -> PathBuf {
    PathBuf::from(STATE_FILE)
}

fn state() -> &'static Mutex<RegistrationState> {
    static STATE: OnceLock<Mutex<RegistrationState>> = OnceLock::new();
    STATE.get_or_init(|| {
        let loaded = std::fs::read(state_file())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Mutex::new(loaded)
    })
}

fn lock_state() -> std::sync::MutexGuard<'static, RegistrationState> {
    state().lock().unwrap_or_else(|error| error.into_inner())
}

/// Writes the registration file atomically, so an interrupted write never
/// leaves partial JSON behind and a concurrent reader never sees one.
async fn persist_state() -> Result<(), String> {
    let value = {
        let state = lock_state();
        serde_json::to_vec_pretty(&*state)
            .map_err(|error| format!("serialize OpenAI registration: {error}"))?
    };
    let path = state_file();
    let parent = path
        .parent()
        .ok_or_else(|| "the OpenAI registration file has no directory".to_owned())?
        .to_owned();
    tokio::fs::create_dir_all(&parent)
        .await
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        std::process::id()
    ));
    let written = async {
        tokio::fs::write(&temporary, &value)
            .await
            .map_err(|error| format!("write {}: {error}", temporary.display()))?;
        tokio::fs::rename(&temporary, &path)
            .await
            .map_err(|error| format!("replace {}: {error}", path.display()))
    };
    if let Err(error) = written.await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    Ok(())
}

/// This installation's host identifier, created on first use and kept for the
/// life of the installation so OpenAI can tell hosts apart. The value is a
/// version 4 UUID, the form the authorization request validates.
fn host_id() -> String {
    static HOST: OnceLock<String> = OnceLock::new();
    HOST.get_or_init(|| {
        let mut state = lock_state();
        if !state.host_id.is_empty() {
            return state.host_id.clone();
        }
        let mut bytes = [0_u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let id = format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        );
        state.host_id = id.clone();
        id
    })
    .clone()
}

fn agent_host_id() -> String {
    format!("urn:uuid:{}", host_id())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs()
}

/// Reads one string claim out of a JWT payload without verifying it. The token
/// arrived over TLS from the issuer this Endpoint itself called, so the payload
/// is read for identification only.
fn jwt_claim(token: &str, claim: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    value
        .get(claim)?
        .as_str()
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
}

/// The account identity Core stores for deduplication and refresh identity. It
/// is never sent upstream: this route identifies the account through its bearer
/// token alone, unlike the ChatGPT Codex backend.
fn account_id(access_token: &str) -> Result<String, String> {
    jwt_claim(access_token, "sub")
        .ok_or_else(|| "The OpenAI access token does not contain a subject identifier".to_owned())
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

/// A field the token endpoint must have answered with usable text. An empty or
/// missing value is reported by name instead of being stored as a credential.
fn required(value: Option<&str>, field: &str) -> Result<String, String> {
    value
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("The OpenAI OAuth token response has no usable {field}"))
}

async fn request_token(
    client: &reqwest::Client,
    form: Vec<(&str, &str)>,
) -> Result<TokenResponse, String> {
    let response = client
        .post(TOKEN_URL)
        .form(&form)
        .timeout(OAUTH_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("OpenAI OAuth token request failed: {error}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("The OpenAI OAuth token response could not be read: {error}"))?;
    if !status.is_success() {
        // The status describes the failure without echoing whatever the token
        // endpoint wrote, which can quote the request that carried a code.
        return Err(format!("OpenAI OAuth token request failed ({status})"));
    }
    serde_json::from_slice(&body)
        .map_err(|error| format!("The OpenAI OAuth token response is not a token: {error}"))
}

/// Validates a token response and turns it into the credential Core stores.
/// The direct-use scope is required because without it the access token cannot
/// call the Responses API at all, and a credential that cannot is worse than a
/// visible failure here.
fn credential_from(token: &TokenResponse) -> Result<SubscriptionCredential, String> {
    let access_token = required(token.access_token.as_deref(), "access_token")?;
    let refresh_token = required(token.refresh_token.as_deref(), "refresh_token")?;
    let scope = required(token.scope.as_deref(), "scope")?;
    if !scope
        .split_whitespace()
        .any(|item| item == DIRECT_TOKEN_SCOPE)
    {
        return Err(format!(
            "The OpenAI OAuth grant did not include {DIRECT_TOKEN_SCOPE}"
        ));
    }
    let expires_in = token
        .expires_in
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| "The OpenAI OAuth token response has no usable lifetime".to_owned())?;
    let account_id = account_id(&access_token)?;
    Ok(SubscriptionCredential {
        access_token,
        refresh_token,
        // The stored expiry is the margin-adjusted one, so Core starts a
        // refresh before the token is actually about to lapse.
        expires_at: now()
            .saturating_add(expires_in)
            .saturating_sub(EXPIRY_MARGIN_SECONDS),
        account_id,
    })
}

/// Remembers the client identifier OpenAI issued for this account, keyed by the
/// refresh token it arrived with, and moves an existing entry when the token it
/// was keyed by is replaced.
async fn remember_registration(
    previous_refresh_token: Option<&str>,
    refresh_token: &str,
    client_id: &str,
    account_id: &str,
) -> Result<(), String> {
    {
        let mut state = lock_state();
        if let Some(previous) = previous_refresh_token
            && previous != refresh_token
        {
            state.clients.remove(&registration_key(previous));
        }
        state.clients.insert(
            registration_key(refresh_token),
            Registration {
                client_id: client_id.to_owned(),
                account_id: account_id.to_owned(),
            },
        );
    }
    persist_state().await
}

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

/// pi's User-Agent, formed from the platform, kernel release, and architecture
/// exactly as pi-ai's getPiUserAgent builds it.
fn pi_user_agent() -> &'static HeaderValue {
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

/// The value Stainless normalizes the operating system to.
fn stainless_os() -> String {
    let platform = std::env::consts::OS.to_ascii_lowercase();
    match platform.as_str() {
        "android" => "Android".to_owned(),
        "macos" => "MacOS".to_owned(),
        "windows" => "Windows".to_owned(),
        "freebsd" => "FreeBSD".to_owned(),
        "openbsd" => "OpenBSD".to_owned(),
        "linux" => "Linux".to_owned(),
        "" => "Unknown".to_owned(),
        other => format!("Other:{other}"),
    }
}

/// The value Stainless normalizes the architecture to.
fn stainless_arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x64".to_owned(),
        "aarch64" => "arm64".to_owned(),
        "arm" => "arm".to_owned(),
        other => format!("other:{other}"),
    }
}

fn insert(headers: &mut HeaderMap, name: &'static str, value: &str) -> Result<(), String> {
    headers.insert(
        HeaderName::from_static(name),
        HeaderValue::from_str(value).map_err(|_| format!("{name} is not a valid header value"))?,
    );
    Ok(())
}

/// Rewrites the caller's Responses body into the shape this route accepts:
/// streaming, not stored, and without the fields the route refuses. Everything
/// else the caller sent is left exactly as it arrived.
fn adapt_body(body: &mut Vec<u8>) -> Result<(), String> {
    let mut value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| "The request body is not a JSON object".to_owned())?;
    let Some(object) = value.as_object_mut() else {
        return Err("The request body is not a JSON object".to_owned());
    };
    object.insert("stream".to_owned(), serde_json::Value::Bool(true));
    object.insert("store".to_owned(), serde_json::Value::Bool(false));
    for field in UNSUPPORTED_FIELDS {
        object.remove(*field);
    }
    *body = serde_json::to_vec(&value)
        .map_err(|error| format!("The request body could not be serialized: {error}"))?;
    Ok(())
}

/// The session affinity pi derives from the request's prompt cache key: the
/// caller's key names its own conversation, so this Endpoint installs it as the
/// affinity headers pi's SDK would send rather than forwarding inbound ones.
fn session_id(body: &[u8]) -> Option<HeaderValue> {
    let value = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let key = value.get("prompt_cache_key")?.as_str()?;
    if key.is_empty() {
        return None;
    }
    HeaderValue::from_str(&key.chars().take(64).collect::<String>()).ok()
}

impl ProviderEndpoint for OpenAiEndpoint {
    fn extension_id(&self) -> &'static str {
        ID
    }

    fn endpoint_type(&self) -> ProviderEndpointType {
        ProviderEndpointType {
            id: ENDPOINT_TYPE,
            display_name: "OpenAI",
            description: "Sign in with ChatGPT against the OpenAI Responses API",
            default_endpoint_id: "openai",
            fixed_base_url: Some(BASE_URL),
            upstream_protocol: Protocol::OpenAiResponses,
            always_event_stream: true,
            surfaces: &[Protocol::OpenAiResponses],
            credential_kinds: CREDENTIAL_KINDS,
            sign_in: Some(ProviderSignIn {
                device_code: false,
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
            .ok_or_else(|| "This OpenAI Endpoint has no connected account".to_owned())?;
        let ProviderEndpointMaterial::Subscription { access_token, .. } = credential.material
        else {
            return Err("This OpenAI Endpoint needs a signed-in ChatGPT account".to_owned());
        };
        strip_inbound_headers(request.headers);
        request.headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {access_token}"))
                .map_err(|_| "The OpenAI access token is not a valid header value".to_owned())?,
        );
        request
            .headers
            .insert(header::USER_AGENT, pi_user_agent().clone());
        // The SDK pi uses takes JSON on this operation and selects streaming
        // through the body, so the request carries its own content type rather
        // than asking for an event stream in a header.
        request
            .headers
            .insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        request.headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        request.headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        insert(request.headers, "x-stainless-lang", "js")?;
        insert(
            request.headers,
            "x-stainless-package-version",
            STAINLESS_PACKAGE_VERSION,
        )?;
        insert(request.headers, "x-stainless-os", &stainless_os())?;
        insert(request.headers, "x-stainless-arch", &stainless_arch())?;
        insert(request.headers, "x-stainless-retry-count", "0")?;

        adapt_body(request.body)?;
        if let Some(session_id) = session_id(request.body) {
            request
                .headers
                .insert(HeaderName::from_static("session_id"), session_id.clone());
            request
                .headers
                .insert(HeaderName::from_static("x-client-request-id"), session_id);
        }
        *request.target_path = "/responses".to_owned();
        Ok(())
    }
}

impl SubscriptionProvider for OpenAiEndpoint {
    fn start_browser_authorization(&self) -> Result<BrowserAuthorization, String> {
        let mut verifier_bytes = [0_u8; 32];
        rand::rng().fill_bytes(&mut verifier_bytes);
        let code_verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let mut state_bytes = [0_u8; 32];
        rand::rng().fill_bytes(&mut state_bytes);
        let state = URL_SAFE_NO_PAD.encode(state_bytes);
        let mut nonce_bytes = [0_u8; 32];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = URL_SAFE_NO_PAD.encode(nonce_bytes);
        let query = serde_urlencoded::to_string([
            ("client_id", DYNAMIC_CLIENT_ID),
            ("agent_name_hint", AGENT_NAME_HINT),
            ("ext_agent_host_id", agent_host_id().as_str()),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT_URI),
            ("resource", RESOURCE),
            ("scope", SCOPE),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("nonce", nonce.as_str()),
        ])
        .map_err(|_| "Could not construct the OpenAI authorization URL".to_owned())?;
        Ok(BrowserAuthorization {
            authorization_url: format!("{AUTHORIZE_URL}?{query}"),
            redirect_uri: REDIRECT_URI.to_owned(),
            state,
            code_verifier,
            expires_in_seconds: BROWSER_TIMEOUT_SECONDS,
        })
    }

    fn exchange_browser_authorization<'a>(
        &'a self,
        client: &'a reqwest::Client,
        callback: &'a BrowserAuthorizationCallback,
        code_verifier: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<SubscriptionCredential, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            // Registration is what makes this flow work: the entrypoint client
            // identifier never exchanges a code, so the identifier OpenAI issued
            // at the callback is the one this exchange must present.
            let client_id = callback.parameter("client_id").ok_or_else(|| {
                "The OpenAI registration callback did not contain one issued client identifier"
                    .to_owned()
            })?;
            let token = request_token(
                client,
                vec![
                    ("grant_type", "authorization_code"),
                    ("client_id", client_id),
                    ("code", callback.code.as_str()),
                    ("code_verifier", code_verifier),
                    ("redirect_uri", REDIRECT_URI),
                    ("resource", RESOURCE),
                ],
            )
            .await?;
            // The presence of an ID token is part of the token-response
            // contract this flow relies on, even though nothing here reads it.
            if token
                .id_token
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                return Err(
                    "The OpenAI OAuth token response did not contain an ID token".to_owned(),
                );
            }
            let credential = credential_from(&token)?;
            remember_registration(
                None,
                &credential.refresh_token,
                client_id,
                &credential.account_id,
            )
            .await?;
            Ok(credential)
        })
    }

    fn start_device_authorization<'a>(
        &'a self,
        _client: &'a reqwest::Client,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<DeviceAuthorization, String>> + Send + 'a>,
    > {
        // This flow signs in through the browser only. Core asks every
        // subscription Endpoint for a device code, so the refusal is answered
        // here rather than by silently starting something else.
        Box::pin(async move {
            Err("OpenAI sign-in uses browser authorization, not a device code".to_owned())
        })
    }

    fn poll_device_authorization<'a>(
        &'a self,
        _client: &'a reqwest::Client,
        _device_auth_id: &'a str,
        _user_code: &'a str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<SubscriptionCredential>, String>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move { Err("OpenAI sign-in has no device code to poll".to_owned()) })
    }

    fn refresh_credential<'a>(
        &'a self,
        client: &'a reqwest::Client,
        refresh_token: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<SubscriptionCredential, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            // The refresh request must present the client identifier OpenAI
            // issued for this account, which this Endpoint type keeps beside the
            // connection because Core's credential shape has no room for it.
            let client_id = {
                let state = lock_state();
                state
                    .clients
                    .get(&registration_key(refresh_token))
                    .map(|registration| registration.client_id.clone())
            }
            .ok_or_else(|| {
                "This connection has no OpenAI registration state; connect the account again"
                    .to_owned()
            })?;
            let token = request_token(
                client,
                vec![
                    ("grant_type", "refresh_token"),
                    ("client_id", client_id.as_str()),
                    ("refresh_token", refresh_token),
                    ("resource", RESOURCE),
                ],
            )
            .await?;
            let credential = credential_from(&token)?;
            // A successful refresh replaces the refresh token, so the
            // registration moves to the token that now reaches this
            // installation.
            remember_registration(
                Some(refresh_token),
                &credential.refresh_token,
                &client_id,
                &credential.account_id,
            )
            .await?;
            Ok(credential)
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: &str) -> String {
        format!("header.{}.signature", URL_SAFE_NO_PAD.encode(claims))
    }

    fn token_response(access: &str, refresh: &str, scope: &str) -> TokenResponse {
        TokenResponse {
            access_token: Some(access.to_owned()),
            refresh_token: Some(refresh.to_owned()),
            expires_in: Some(3600),
            scope: Some(scope.to_owned()),
            id_token: Some("id-token".to_owned()),
        }
    }

    const FULL_SCOPE: &str =
        "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

    /// ENDPOINT-47: sign-in registers this installation from the dynamic
    /// entrypoint and identifies it with a stable host identifier.
    #[test]
    fn authorization_url_registers_a_new_client_for_this_host() {
        let flow = ENDPOINT.start_browser_authorization().unwrap();
        let url = reqwest::Url::parse(&flow.authorization_url).unwrap();
        assert_eq!(
            url.as_str().split('?').next().unwrap(),
            "https://auth.openai.com/api/accounts/authorize"
        );
        let query: HashMap<_, _> = url.query_pairs().collect();
        // First-time registration starts from the dynamic entrypoint, which is
        // never the client ID a token exchange presents.
        assert_eq!(query.get("client_id").unwrap(), DYNAMIC_CLIENT_ID);
        assert_eq!(query.get("agent_name_hint").unwrap(), AGENT_NAME_HINT);
        assert_eq!(query.get("redirect_uri").unwrap(), REDIRECT_URI);
        assert_eq!(flow.redirect_uri, REDIRECT_URI);
        // The address this Endpoint type registers is the loopback IP spelling,
        // so it never accepts the Codex Endpoint's localhost callback.
        assert_eq!(REDIRECT_URI, "http://127.0.0.1:1455/auth/callback");
        assert_eq!(query.get("resource").unwrap(), RESOURCE);
        assert_eq!(query.get("scope").unwrap(), SCOPE);
        assert_eq!(query.get("response_type").unwrap(), "code");
        assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(query.get("state").unwrap(), &flow.state);
        assert!(query.get("nonce").is_some_and(|value| !value.is_empty()));
        // PKCE and state are fresh per attempt, and the verifier is the 32-byte
        // base64url value pi generates.
        assert_eq!(flow.code_verifier.len(), 43);
        assert_eq!(query.get("code_challenge").unwrap().len(), 43);
        assert_ne!(query.get("code_challenge").unwrap(), &flow.code_verifier);
        assert_eq!(flow.expires_in_seconds, 900);
        // The host identifier is the URN form OpenAI validates and is stable
        // across attempts from this installation.
        let host = query.get("ext_agent_host_id").unwrap();
        assert_eq!(host.as_ref(), agent_host_id());
        let uuid = host.trim_start_matches("urn:uuid:");
        let groups: Vec<&str> = uuid.split('-').collect();
        assert_eq!(
            groups.iter().map(|group| group.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(
            groups
                .iter()
                .all(|group| group.chars().all(|c| c.is_ascii_hexdigit()))
        );
    }

    /// ENDPOINT-47: what OpenAI issues in the callback is what a later exchange
    /// presents, and neither the entrypoint nor a repeated value is accepted.
    #[test]
    fn the_entrypoint_client_never_exchanges_a_code() {
        // The registration callback must carry the issued client ID, because the
        // entrypoint value is not usable for token exchange.
        let callback = |extra: Vec<(&str, &str)>| BrowserAuthorizationCallback {
            redirect_uri: REDIRECT_URI.to_owned(),
            code: "code".to_owned(),
            extra_params: extra
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        };
        assert_eq!(
            callback(vec![("client_id", "issued-1")]).parameter("client_id"),
            Some("issued-1")
        );
        // A missing, empty, or repeated identifier is refused instead of guessed.
        assert_eq!(callback(vec![]).parameter("client_id"), None);
        assert_eq!(
            callback(vec![("client_id", "")]).parameter("client_id"),
            None
        );
        assert_eq!(
            callback(vec![("client_id", "a"), ("client_id", "b")]).parameter("client_id"),
            None
        );
    }

    /// ENDPOINT-47: an account that cannot call the Responses API is refused at
    /// sign-in instead of being stored as a working one.
    #[test]
    fn a_credential_requires_the_direct_use_scope() {
        // Without this scope the access token cannot call the Responses API, so
        // accepting the credential would only move the failure later.
        let missing = token_response(
            &jwt(r#"{"sub":"user-1"}"#),
            "refresh-1",
            "openid profile email offline_access resource.invoke",
        );
        assert!(credential_from(&missing).is_err());
        let present = token_response(&jwt(r#"{"sub":"user-1"}"#), "refresh-1", FULL_SCOPE);
        assert!(credential_from(&present).is_ok());
    }

    /// ENDPOINT-47: the stored account keeps Core's credential shape and expires
    /// with the margin the refresh needs.
    #[test]
    fn a_credential_carries_the_subject_and_a_margin_adjusted_expiry() {
        let token = token_response(&jwt(r#"{"sub":"user-1"}"#), "refresh-1", FULL_SCOPE);
        let before = now();
        let credential = credential_from(&token).unwrap();
        assert_eq!(credential.account_id, "user-1");
        assert_eq!(credential.refresh_token, "refresh-1");
        // The stored expiry keeps the three-minute margin, so Core refreshes
        // before the token is actually about to lapse.
        let earliest = before + 3600 - EXPIRY_MARGIN_SECONDS;
        let latest = now() + 3600 - EXPIRY_MARGIN_SECONDS;
        assert!(
            credential.expires_at >= earliest && credential.expires_at <= latest,
            "unexpected expiry {}",
            credential.expires_at
        );
        for broken in [
            TokenResponse {
                access_token: Some(String::new()),
                ..token_response(&jwt(r#"{"sub":"user-1"}"#), "refresh-1", FULL_SCOPE)
            },
            TokenResponse {
                refresh_token: None,
                ..token_response(&jwt(r#"{"sub":"user-1"}"#), "refresh-1", FULL_SCOPE)
            },
            TokenResponse {
                expires_in: Some(0),
                ..token_response(&jwt(r#"{"sub":"user-1"}"#), "refresh-1", FULL_SCOPE)
            },
        ] {
            assert!(credential_from(&broken).is_err());
        }
        // An account identity that cannot be read is not silently accepted.
        assert!(credential_from(&token_response("not-a-jwt", "refresh-1", FULL_SCOPE)).is_err());
    }

    /// ENDPOINT-48: an inference request reaches the Provider as pi's does, so
    /// the identity headers are absent, the agent and Stainless values are this
    /// Endpoint's own, and only the fields this route refuses are omitted.
    #[test]
    fn request_preparation_mirrors_pi_and_keeps_the_callers_body() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer caller"),
        );
        headers.insert(header::USER_AGENT, HeaderValue::from_static("caller-agent"));
        // A caller that describes its own client must not have that description
        // forwarded, and in particular cannot make this request claim a runtime.
        headers.insert(
            HeaderName::from_static("x-stainless-runtime"),
            HeaderValue::from_static("caller-runtime"),
        );
        headers.insert(
            HeaderName::from_static("x-stainless-lang"),
            HeaderValue::from_static("caller-lang"),
        );
        let mut body = br#"{"model":"gpt-6.1-sol","input":[{"role":"user","content":"hello"}],"stream":false,"store":true,"max_output_tokens":100,"temperature":0.2,"prompt_cache_retention":"24h","prompt_cache_options":{"mode":"implicit"},"top_p":0.9,"truncation":"disabled","metadata":{"trace":"caller"},"prompt_cache_key":"session"}"#.to_vec();
        let mut target_path = "/v1/responses".to_owned();

        ENDPOINT
            .prepare_request(ProviderEndpointRequest {
                headers: &mut headers,
                body: &mut body,
                target_path: &mut target_path,
                credential: Some(yabane_extension_api::ProviderEndpointCredential {
                    kind: "openai_account",
                    material: ProviderEndpointMaterial::Subscription {
                        access_token: "upstream-token",
                        account_id: "upstream-account",
                    },
                }),
            })
            .unwrap();

        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // The hosted API takes the operation below its own /v1 root, which the
        // fixed base URL already names.
        assert_eq!(target_path, "/responses");
        assert_eq!(headers[header::AUTHORIZATION], "Bearer upstream-token");
        // This route identifies the account through its bearer token alone, so
        // the ChatGPT Codex headers are never added to it. Core removes a
        // caller-supplied copy before an Endpoint prepares the request.
        assert!(!headers.contains_key("chatgpt-account-id"));
        assert!(!headers.contains_key("originator"));
        assert!(!headers.contains_key("openai-beta"));
        // An inbound agent value is replaced rather than forwarded.
        assert_eq!(headers[header::USER_AGENT], pi_user_agent());
        assert_ne!(headers[header::USER_AGENT], "caller-agent");
        assert_eq!(headers[header::ACCEPT], "application/json");
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        assert_eq!(headers["x-stainless-lang"], "js");
        assert_eq!(
            headers["x-stainless-package-version"],
            STAINLESS_PACKAGE_VERSION
        );
        // pi's SDK fills the runtime pair by detecting its own Node process, so
        // this Endpoint sends neither instead of claiming a runtime.
        assert!(!headers.contains_key("x-stainless-runtime"));
        assert!(!headers.contains_key("x-stainless-runtime-version"));
        assert_eq!(headers["x-stainless-retry-count"], "0");
        assert_eq!(headers["session_id"], "session");
        assert_eq!(headers["x-client-request-id"], "session");
        assert_eq!(value["stream"], true);
        assert_eq!(value["store"], false);
        for dropped in UNSUPPORTED_FIELDS {
            assert!(
                value.get(dropped).is_none(),
                "field '{dropped}' must not reach this route"
            );
        }
        // Fields pi does not remove stay in the request, so the hosted API
        // answers them instead of this Endpoint rewriting the caller's request.
        assert_eq!(value["top_p"], 0.9);
        assert_eq!(value["truncation"], "disabled");
        assert_eq!(value["metadata"]["trace"], "caller");
        assert_eq!(value["input"][0]["content"], "hello");
    }

    /// ENDPOINT-48: this route authenticates through the bearer token alone, so
    /// it serves nothing without a connected account.
    #[test]
    fn request_preparation_requires_a_connected_account() {
        let mut headers = HeaderMap::new();
        let mut body = br#"{"model":"gpt-6.1-sol"}"#.to_vec();
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
                        kind: "openai_account",
                        material: ProviderEndpointMaterial::Subscription {
                            access_token: "bad\nvalue",
                            account_id: "account",
                        },
                    }),
                })
                .is_err()
        );
    }

    /// ENDPOINT-47: a persisted registration is unusable on its own and follows
    /// the refresh token it belongs to.
    #[test]
    fn registrations_are_keyed_by_a_digest_of_their_refresh_token() {
        // The file is keyed by a digest, so a stored entry never holds a usable
        // refresh token.
        assert!(!registration_key("refresh-1").contains("refresh-1"));
        assert_eq!(registration_key("refresh-1").len(), 64);
        assert_eq!(registration_key("refresh-1"), registration_key("refresh-1"));
        assert_ne!(registration_key("refresh-1"), registration_key("refresh-2"));
    }

    /// ENDPOINT-46: the declaration fixes the connection, serves Responses only,
    /// and offers browser sign-in alone.
    #[test]
    fn the_declaration_fixes_the_connection_and_offers_browser_sign_in_alone() {
        let declaration = ENDPOINT.endpoint_type();
        assert_eq!(declaration.id, ENDPOINT_TYPE);
        assert_eq!(declaration.fixed_base_url, Some(BASE_URL));
        assert_eq!(declaration.upstream_protocol, Protocol::OpenAiResponses);
        assert_eq!(declaration.surfaces, &[Protocol::OpenAiResponses]);
        assert_eq!(declaration.default_endpoint_id, "openai");
        let sign_in = declaration.sign_in.unwrap();
        // This flow has no device code, so the console must not offer one.
        assert!(!sign_in.device_code);
        assert!(sign_in.browser);
        assert_eq!(
            declaration
                .credential_kinds
                .iter()
                .map(|kind| (kind.id, kind.flow))
                .collect::<Vec<_>>(),
            vec![("openai_account", CredentialFlow::Subscription)]
        );
        assert!(declaration.always_event_stream);
        assert!(ENDPOINT.models().contains(&"gpt-6.1-sol"));
    }
}

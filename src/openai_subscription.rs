use std::{collections::HashMap, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::config::{ApiEndpoint, ApiType, AppState, OpenAiSubscription, Provider, save_providers};

// Public client registration used by OpenAI's Codex device authorization flow.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_BASE_URL: &str = "https://auth.openai.com";
const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api";
const DEVICE_TIMEOUT_SECONDS: u64 = 15 * 60;
const OAUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
// ChatGPT's Codex backend has no supported /models operation, so keep this catalog explicit.
pub const MODELS: &[&str] = &[
    "gpt-5.3-codex-spark",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.5",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
];

#[derive(Clone, Default)]
pub struct OAuthState {
    flows: Arc<Mutex<HashMap<String, DeviceFlow>>>,
    refresh_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

#[derive(Clone)]
struct DeviceFlow {
    provider_id: String,
    provider_name: Option<String>,
    create_provider: bool,
    endpoint_id: String,
    socks5_proxy: Option<String>,
    client: reqwest::Client,
    device_auth_id: String,
    user_code: String,
    interval_seconds: u64,
    next_poll_at: u64,
    poll_lock: Arc<Mutex<()>>,
    expires_at: u64,
    status: FlowStatus,
}

#[derive(Clone)]
enum FlowStatus {
    Pending,
    Complete,
    Failed(String),
}

pub enum StartError {
    Invalid(String),
    Upstream(String),
}

#[derive(Deserialize)]
pub struct StartSubscription {
    pub provider_id: String,
    pub provider_name: Option<String>,
    pub endpoint_id: Option<String>,
    pub socks5_proxy: Option<String>,
}

#[derive(Serialize)]
pub struct DeviceFlowView {
    pub id: String,
    pub status: &'static str,
    pub user_code: String,
    pub verification_uri: &'static str,
    pub interval_seconds: u64,
    pub expires_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

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

pub async fn start(
    state: &AppState,
    input: StartSubscription,
) -> Result<DeviceFlowView, StartError> {
    if !valid_id(&input.provider_id) {
        return Err(StartError::Invalid(
            "Provider ID must be a URL slug".to_owned(),
        ));
    }
    let endpoint_id = input.endpoint_id.unwrap_or_else(|| "chatgpt".to_owned());
    if !valid_id(&endpoint_id) {
        return Err(StartError::Invalid(
            "Endpoint ID must be a URL slug".to_owned(),
        ));
    }
    let socks5_proxy =
        normalized_socks5_proxy(input.socks5_proxy.as_deref()).map_err(StartError::Invalid)?;
    let oauth_endpoint = ApiEndpoint {
        id: endpoint_id.clone(),
        socks5_proxy: socks5_proxy.clone(),
        ..ApiEndpoint::default()
    };
    let client = oauth_endpoint
        .client(&state.client)
        .map_err(StartError::Invalid)?;
    let create_provider = {
        let providers = state.providers.read().await;
        match providers.get(&input.provider_id) {
            Some(provider) => {
                if provider
                    .endpoints
                    .iter()
                    .any(|endpoint| endpoint.id == endpoint_id)
                {
                    return Err(StartError::Invalid("Endpoint ID already exists".to_owned()));
                }
                false
            }
            None if input
                .provider_name
                .as_deref()
                .is_none_or(|name| name.trim().is_empty()) =>
            {
                return Err(StartError::Invalid(
                    "Provider name is required when creating a Provider".to_owned(),
                ));
            }
            None => true,
        }
    };

    let response = client
        .post(format!("{AUTH_BASE_URL}/api/accounts/deviceauth/usercode"))
        .json(&serde_json::json!({"client_id": CLIENT_ID}))
        .timeout(OAUTH_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|err| StartError::Upstream(format!("Could not start OpenAI sign-in: {err}")))?;
    let status = response.status();
    let body = response.bytes().await.map_err(|err| {
        StartError::Upstream(format!("Could not read OpenAI sign-in response: {err}"))
    })?;
    if !status.is_success() {
        return Err(StartError::Upstream(format!(
            "OpenAI sign-in returned {status}"
        )));
    }
    let device: DeviceCodeResponse = serde_json::from_slice(&body).map_err(|_| {
        StartError::Upstream("OpenAI returned an invalid device sign-in response".to_owned())
    })?;
    let interval_seconds = match device.interval {
        serde_json::Value::Number(value) => value.as_u64(),
        serde_json::Value::String(value) => value.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| StartError::Upstream("OpenAI returned an invalid polling interval".to_owned()))?
    .max(1);
    let now = crate::auth::now();
    let id = random_id();
    let flow = DeviceFlow {
        provider_id: input.provider_id,
        provider_name: input.provider_name.map(|name| name.trim().to_owned()),
        create_provider,
        endpoint_id,
        socks5_proxy,
        client,
        device_auth_id: device.device_auth_id,
        user_code: device.user_code,
        interval_seconds,
        next_poll_at: now,
        poll_lock: Arc::new(Mutex::new(())),
        expires_at: now + DEVICE_TIMEOUT_SECONDS,
        status: FlowStatus::Pending,
    };
    let view = flow_view(&id, &flow);
    let mut flows = state.openai_oauth.flows.lock().await;
    flows.retain(|_, existing| existing.expires_at > now);
    flows.insert(id, flow);
    Ok(view)
}

pub async fn poll(state: &AppState, id: &str) -> Result<DeviceFlowView, String> {
    let now = crate::auth::now();
    let flow = {
        let mut flows = state.openai_oauth.flows.lock().await;
        let flow = flows
            .get_mut(id)
            .ok_or_else(|| "OpenAI sign-in flow not found".to_owned())?;
        match &flow.status {
            FlowStatus::Pending if flow.expires_at <= now => {
                flow.status = FlowStatus::Failed("OpenAI sign-in expired".to_owned());
                return Ok(flow_view(id, flow));
            }
            FlowStatus::Pending if flow.next_poll_at > now => return Ok(flow_view(id, flow)),
            FlowStatus::Pending => flow.next_poll_at = now.saturating_add(flow.interval_seconds),
            _ => return Ok(flow_view(id, flow)),
        }
        flow.clone()
    };
    let Ok(_poll_guard) = flow.poll_lock.clone().try_lock_owned() else {
        let flows = state.openai_oauth.flows.lock().await;
        return flows
            .get(id)
            .map(|current| flow_view(id, current))
            .ok_or_else(|| "OpenAI sign-in flow not found".to_owned());
    };

    match poll_openai(&flow.client, &flow, AUTH_BASE_URL).await {
        Ok(None) => {}
        Ok(Some(credential)) => {
            if let Err(error) = attach_subscription(state, &flow, credential).await {
                set_flow_status(state, id, FlowStatus::Failed(error)).await;
            } else {
                set_flow_status(state, id, FlowStatus::Complete).await;
            }
        }
        Err(error) => set_flow_status(state, id, FlowStatus::Failed(error)).await,
    }
    let flows = state.openai_oauth.flows.lock().await;
    flows
        .get(id)
        .map(|flow| flow_view(id, flow))
        .ok_or_else(|| "OpenAI sign-in flow not found".to_owned())
}

async fn poll_openai(
    client: &reqwest::Client,
    flow: &DeviceFlow,
    auth_base_url: &str,
) -> Result<Option<OpenAiSubscription>, String> {
    let response = client.post(format!("{auth_base_url}/api/accounts/deviceauth/token"))
        .json(&serde_json::json!({"device_auth_id": flow.device_auth_id, "user_code": flow.user_code}))
        .timeout(OAUTH_REQUEST_TIMEOUT)
        .send().await.map_err(|err| format!("OpenAI sign-in polling failed: {err}"))?;
    if response.status() == reqwest::StatusCode::FORBIDDEN
        || response.status() == reqwest::StatusCode::NOT_FOUND
    {
        return Ok(None);
    }
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|err| format!("Could not read OpenAI sign-in status: {err}"))?;
    if !status.is_success() {
        return Err(format!("OpenAI sign-in failed with {status}"));
    }
    let code: DeviceTokenResponse = serde_json::from_slice(&body)
        .map_err(|_| "OpenAI returned an invalid sign-in completion".to_owned())?;
    exchange_code(
        client,
        &code.authorization_code,
        &code.code_verifier,
        auth_base_url,
    )
    .await
    .map(Some)
}

async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    verifier: &str,
    auth_base_url: &str,
) -> Result<OpenAiSubscription, String> {
    let response = client
        .post(format!("{auth_base_url}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            (
                "redirect_uri",
                "https://auth.openai.com/deviceauth/callback",
            ),
        ])
        .timeout(OAUTH_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|err| format!("OpenAI token exchange failed: {err}"))?;
    token_response(response, "exchange").await
}

async fn refresh_token(
    client: &reqwest::Client,
    refresh: &str,
    auth_base_url: &str,
) -> Result<OpenAiSubscription, String> {
    let response = client
        .post(format!("{auth_base_url}/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", CLIENT_ID),
        ])
        .timeout(OAUTH_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|err| format!("OpenAI token refresh failed: {err}"))?;
    token_response(response, "refresh").await
}

async fn token_response(
    response: reqwest::Response,
    operation: &str,
) -> Result<OpenAiSubscription, String> {
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|err| format!("Could not read OpenAI token response: {err}"))?;
    if !status.is_success() {
        return Err(format!("OpenAI token {operation} failed with {status}"));
    }
    let token: TokenResponse = serde_json::from_slice(&body)
        .map_err(|_| format!("OpenAI token {operation} returned an invalid response"))?;
    let account_id = account_id(&token.access_token)?;
    reqwest::header::HeaderValue::from_str(&account_id)
        .map_err(|_| "OpenAI access token contains an invalid ChatGPT account ID".to_owned())?;
    Ok(OpenAiSubscription {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: crate::auth::now().saturating_add(token.expires_in),
        account_id,
    })
}

pub async fn refreshed_endpoint(
    state: &AppState,
    provider_id: &str,
    endpoint_id: &str,
) -> Result<ApiEndpoint, String> {
    let lock_key = format!("{provider_id}/{endpoint_id}");
    let refresh_lock = {
        let mut locks = state.openai_oauth.refresh_locks.lock().await;
        locks
            .entry(lock_key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _guard = refresh_lock.lock().await;
    let endpoint = state
        .providers
        .read()
        .await
        .get(provider_id)
        .and_then(|provider| {
            provider
                .endpoints
                .iter()
                .find(|endpoint| endpoint.id == endpoint_id)
        })
        .cloned()
        .ok_or_else(|| "OpenAI subscription Endpoint no longer exists".to_owned())?;
    let credential = endpoint
        .openai_subscription
        .as_ref()
        .ok_or_else(|| "OpenAI subscription is not connected".to_owned())?;
    if credential.expires_at > crate::auth::now().saturating_add(60) {
        return Ok(endpoint);
    }
    let client = endpoint.client(&state.client)?;
    let refreshed = refresh_token(&client, &credential.refresh_token, AUTH_BASE_URL).await?;
    let mut providers = state.providers.write().await;
    let mut updated = providers.clone();
    let stored = updated
        .get_mut(provider_id)
        .and_then(|provider| {
            provider
                .endpoints
                .iter_mut()
                .find(|endpoint| endpoint.id == endpoint_id)
        })
        .ok_or_else(|| "OpenAI subscription Endpoint no longer exists".to_owned())?;
    if stored.openai_subscription.is_none() {
        return Err("OpenAI subscription is not connected".to_owned());
    }
    if !replace_credential_if_current(stored, credential, refreshed) {
        return Ok(stored.clone());
    }
    save_providers(&updated)
        .await
        .map_err(|err| format!("Could not persist refreshed OpenAI subscription: {err}"))?;
    *providers = updated;
    providers
        .get(provider_id)
        .and_then(|provider| {
            provider
                .endpoints
                .iter()
                .find(|endpoint| endpoint.id == endpoint_id)
        })
        .cloned()
        .ok_or_else(|| "OpenAI subscription Endpoint no longer exists".to_owned())
}

async fn attach_subscription(
    state: &AppState,
    flow: &DeviceFlow,
    credential: OpenAiSubscription,
) -> Result<(), String> {
    let mut providers = state.providers.write().await;
    let mut updated = providers.clone();
    if flow.create_provider && updated.contains_key(&flow.provider_id) {
        return Err("Provider ID already exists".to_owned());
    }
    if !flow.create_provider && !updated.contains_key(&flow.provider_id) {
        return Err("Provider no longer exists".to_owned());
    }
    let provider = updated
        .entry(flow.provider_id.clone())
        .or_insert_with(|| Provider {
            id: flow.provider_id.clone(),
            name: flow
                .provider_name
                .clone()
                .unwrap_or_else(|| "OpenAI subscription".to_owned()),
            extra_headers: HashMap::new(),
            extra_body: serde_json::Map::new(),
            defaults_endpoint_ids: Vec::new(),
            endpoints: Vec::new(),
            discovered_models: Vec::new(),
            model_endpoints: HashMap::new(),
            model_endpoint_preferences: Vec::new(),
            models_discovered_at: None,
            model_discovery_error: None,
        });
    if provider
        .endpoints
        .iter()
        .any(|endpoint| endpoint.id == flow.endpoint_id)
    {
        return Err("Endpoint ID already exists".to_owned());
    }
    provider
        .endpoints
        .push(subscription_endpoint(flow, credential));
    provider
        .discovered_models
        .extend(MODELS.iter().map(|model| (*model).to_owned()));
    provider.discovered_models.sort();
    provider.discovered_models.dedup();
    for model in MODELS {
        provider
            .model_endpoints
            .entry((*model).to_owned())
            .or_default()
            .push(flow.endpoint_id.clone());
    }
    provider.models_discovered_at = Some(crate::auth::now());
    provider.model_discovery_error = None;
    save_providers(&updated)
        .await
        .map_err(|err| format!("Could not save OpenAI subscription: {err}"))?;
    *providers = updated;
    Ok(())
}

fn subscription_endpoint(flow: &DeviceFlow, credential: OpenAiSubscription) -> ApiEndpoint {
    ApiEndpoint {
        id: flow.endpoint_id.clone(),
        api_type: ApiType::OpenaiCodex,
        base_url: CHATGPT_BASE_URL.to_owned(),
        socks5_proxy: flow.socks5_proxy.clone(),
        extra_headers: HashMap::new(),
        extra_body: serde_json::Map::new(),
        requires_api_key: false,
        api_keys: Vec::new(),
        openai_subscription: Some(credential),
        ..ApiEndpoint::default()
    }
}

fn normalized_socks5_proxy(value: Option<&str>) -> Result<Option<String>, String> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    if value
        .is_some_and(|value| !value.starts_with("socks5://") && !value.starts_with("socks5h://"))
    {
        return Err("SOCKS5 proxy URL must start with socks5:// or socks5h://".to_owned());
    }
    Ok(value.map(str::to_owned))
}

async fn set_flow_status(state: &AppState, id: &str, status: FlowStatus) {
    if let Some(flow) = state.openai_oauth.flows.lock().await.get_mut(id) {
        flow.status = status;
    }
}

fn replace_credential_if_current(
    endpoint: &mut ApiEndpoint,
    expected: &OpenAiSubscription,
    refreshed: OpenAiSubscription,
) -> bool {
    let Some(current) = endpoint.openai_subscription.as_ref() else {
        return false;
    };
    if current.access_token != expected.access_token
        || current.refresh_token != expected.refresh_token
        || current.expires_at != expected.expires_at
        || current.account_id != expected.account_id
    {
        return false;
    }
    endpoint.openai_subscription = Some(refreshed);
    true
}

fn flow_view(id: &str, flow: &DeviceFlow) -> DeviceFlowView {
    let (status, error) = match &flow.status {
        FlowStatus::Pending => ("pending", None),
        FlowStatus::Complete => ("complete", None),
        FlowStatus::Failed(error) => ("failed", Some(error.clone())),
    };
    DeviceFlowView {
        id: id.to_owned(),
        status,
        user_code: flow.user_code.clone(),
        verification_uri: "https://auth.openai.com/codex/device",
        interval_seconds: flow.interval_seconds,
        expires_at: flow.expires_at,
        error,
    }
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

fn random_id() -> String {
    use rand::RngCore as _;
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use tokio::sync::Mutex;

    use crate::config::OpenAiSubscription;

    #[test]
    fn extracts_chatgpt_account_id_from_access_token() {
        let payload = URL_SAFE_NO_PAD
            .encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"account-123"}}"#);
        let token = format!("header.{payload}.signature");
        assert_eq!(super::account_id(&token).unwrap(), "account-123");
    }

    #[test]
    fn rejects_access_token_without_account_id() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"user"}"#);
        assert!(super::account_id(&format!("header.{payload}.signature")).is_err());
    }

    #[test]
    fn stale_refresh_cannot_overwrite_a_reconnected_endpoint() {
        use crate::config::{ApiEndpoint, OpenAiSubscription};

        let stale = OpenAiSubscription {
            access_token: "stale-access".to_owned(),
            refresh_token: "stale-refresh".to_owned(),
            expires_at: 1,
            account_id: "account".to_owned(),
        };
        let current = OpenAiSubscription {
            access_token: "current-access".to_owned(),
            refresh_token: "current-refresh".to_owned(),
            expires_at: 2,
            account_id: "account".to_owned(),
        };
        let refreshed = OpenAiSubscription {
            access_token: "refreshed-stale-access".to_owned(),
            refresh_token: "refreshed-stale-refresh".to_owned(),
            expires_at: 3,
            account_id: "account".to_owned(),
        };
        let mut endpoint = ApiEndpoint {
            openai_subscription: Some(current.clone()),
            ..ApiEndpoint::default()
        };

        assert!(!super::replace_credential_if_current(
            &mut endpoint,
            &stale,
            refreshed,
        ));
        assert_eq!(
            endpoint.openai_subscription.unwrap().access_token,
            current.access_token,
        );
    }

    #[test]
    fn validates_and_normalizes_subscription_proxy() {
        assert_eq!(
            super::normalized_socks5_proxy(Some(" socks5h://127.0.0.1:1080 ")).unwrap(),
            Some("socks5h://127.0.0.1:1080".to_owned()),
        );
        assert_eq!(super::normalized_socks5_proxy(Some("  ")).unwrap(), None);
        assert!(super::normalized_socks5_proxy(Some("http://127.0.0.1:1080")).is_err());
    }

    #[test]
    fn subscription_endpoint_keeps_flow_proxy() {
        let credential = OpenAiSubscription {
            access_token: "access".to_owned(),
            refresh_token: "refresh".to_owned(),
            expires_at: 1,
            account_id: "account".to_owned(),
        };
        let flow = super::DeviceFlow {
            provider_id: "openai".to_owned(),
            provider_name: None,
            create_provider: true,
            endpoint_id: "chatgpt".to_owned(),
            socks5_proxy: Some("socks5h://127.0.0.1:1080".to_owned()),
            client: reqwest::Client::new(),
            device_auth_id: "device".to_owned(),
            user_code: "CODE".to_owned(),
            interval_seconds: 1,
            next_poll_at: 0,
            poll_lock: Arc::new(Mutex::new(())),
            expires_at: u64::MAX,
            status: super::FlowStatus::Pending,
        };

        let endpoint = super::subscription_endpoint(&flow, credential);
        assert_eq!(
            endpoint.socks5_proxy.as_deref(),
            Some("socks5h://127.0.0.1:1080"),
        );
        assert_eq!(endpoint.api_type, crate::config::ApiType::OpenaiCodex);
    }

    #[tokio::test]
    async fn refreshes_subscription_credential() {
        use std::collections::HashMap;

        use axum::{Form, Json, Router, routing::post};

        let payload = URL_SAFE_NO_PAD.encode(
            br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"account-refreshed"}}"#,
        );
        let access_token = format!("header.{payload}.signature");
        let app = Router::new().route(
            "/oauth/token",
            post(move |Form(form): Form<HashMap<String, String>>| {
                let access_token = access_token.clone();
                async move {
                    assert_eq!(form.get("grant_type").map(String::as_str), Some("refresh_token"));
                    assert_eq!(form.get("refresh_token").map(String::as_str), Some("old-refresh"));
                    Json(serde_json::json!({"access_token":access_token, "refresh_token":"new-refresh", "expires_in":3600}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let credential = super::refresh_token(
            &reqwest::Client::new(),
            "old-refresh",
            &format!("http://{address}"),
        )
        .await
        .unwrap();
        assert_eq!(credential.account_id, "account-refreshed");
        assert_eq!(credential.refresh_token, "new-refresh");
    }

    #[tokio::test]
    async fn device_completion_exchanges_code_for_subscription_credential() {
        use axum::{Json, Router, routing::post};

        let payload = URL_SAFE_NO_PAD
            .encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"account-456"}}"#);
        let access_token = format!("header.{payload}.signature");
        let app = Router::new()
            .route("/api/accounts/deviceauth/token", post(|| async {
                Json(serde_json::json!({"authorization_code":"code", "code_verifier":"verifier"}))
            }))
            .route("/oauth/token", post(move || { let access_token = access_token.clone(); async move {
                Json(serde_json::json!({"access_token":access_token, "refresh_token":"refresh", "expires_in":3600}))
            }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let flow = super::DeviceFlow {
            provider_id: "openai".to_owned(),
            provider_name: None,
            create_provider: false,
            endpoint_id: "chatgpt".to_owned(),
            socks5_proxy: None,
            client: reqwest::Client::new(),
            device_auth_id: "device".to_owned(),
            user_code: "CODE".to_owned(),
            interval_seconds: 1,
            next_poll_at: 0,
            poll_lock: Arc::new(Mutex::new(())),
            expires_at: u64::MAX,
            status: super::FlowStatus::Pending,
        };

        let credential = super::poll_openai(&flow.client, &flow, &format!("http://{address}"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(credential.account_id, "account-456");
        assert_eq!(credential.refresh_token, "refresh");
        assert!(credential.expires_at > crate::auth::now());
    }
}

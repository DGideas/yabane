use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

use crate::config::{ApiEndpoint, ApiType, AppState, OpenAiSubscription, Provider, save_providers};

#[derive(Clone, Default)]
pub struct OAuthState {
    flows: Arc<Mutex<HashMap<String, DeviceFlow>>>,
    refresh_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

#[derive(Clone)]
struct DeviceFlow {
    kind: FlowKind,
    provider_id: String,
    provider_name: Option<String>,
    create_provider: bool,
    endpoint_id: String,
    socks5_proxy: Option<String>,
    client: reqwest::Client,
    base_url: String,
    device_auth_id: String,
    user_code: String,
    verification_uri: &'static str,
    interval_seconds: u64,
    next_poll_at: u64,
    poll_lock: Arc<Mutex<()>>,
    expires_at: u64,
    status: FlowStatus,
    browser_state: Option<String>,
    code_verifier: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum FlowKind {
    Device,
    Browser,
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

pub enum CompleteError {
    Invalid(String),
    Upstream(String),
    Internal(String),
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

#[derive(Serialize)]
pub struct BrowserFlowView {
    pub id: String,
    pub authorization_url: String,
    pub expires_at: u64,
}

#[derive(Deserialize)]
pub struct CompleteBrowserAuthorization {
    pub redirect_url: String,
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

    let implementation = state
        .extensions
        .subscription_provider("openai_codex")
        .ok_or_else(|| {
            StartError::Invalid("OpenAI Subscription Extension is not enabled".to_owned())
        })?;
    let endpoint_type = implementation.endpoint_type();
    let base_url = endpoint_type
        .fixed_base_url
        .ok_or_else(|| {
            StartError::Invalid("Subscription Extension must declare a fixed base URL".to_owned())
        })?
        .to_owned();
    let device = implementation
        .start_device_authorization(&client)
        .await
        .map_err(StartError::Upstream)?;
    let now = crate::auth::now();
    let id = random_id();
    let flow = DeviceFlow {
        kind: FlowKind::Device,
        provider_id: input.provider_id,
        provider_name: input.provider_name.map(|name| name.trim().to_owned()),
        create_provider,
        endpoint_id,
        socks5_proxy,
        client,
        base_url,
        device_auth_id: device.device_auth_id,
        user_code: device.user_code,
        verification_uri: device.verification_uri,
        interval_seconds: device.interval_seconds,
        next_poll_at: now,
        poll_lock: Arc::new(Mutex::new(())),
        expires_at: now.saturating_add(device.expires_in_seconds),
        status: FlowStatus::Pending,
        browser_state: None,
        code_verifier: None,
    };
    let view = flow_view(&id, &flow);
    let mut flows = state.openai_oauth.flows.lock().await;
    flows.retain(|_, existing| existing.expires_at > now);
    flows.insert(id, flow);
    Ok(view)
}

pub async fn start_browser(
    state: &AppState,
    input: StartSubscription,
) -> Result<BrowserFlowView, StartError> {
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
    let client = ApiEndpoint {
        id: endpoint_id.clone(),
        socks5_proxy: socks5_proxy.clone(),
        ..ApiEndpoint::default()
    }
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
    let implementation = state
        .extensions
        .subscription_provider("openai_codex")
        .ok_or_else(|| {
            StartError::Invalid("OpenAI Subscription Extension is not enabled".to_owned())
        })?;
    let base_url = implementation
        .endpoint_type()
        .fixed_base_url
        .ok_or_else(|| {
            StartError::Invalid("Subscription Extension must declare a fixed base URL".to_owned())
        })?
        .to_owned();
    let browser = implementation
        .start_browser_authorization()
        .map_err(StartError::Upstream)?;
    let now = crate::auth::now();
    let id = random_id();
    let flow = DeviceFlow {
        kind: FlowKind::Browser,
        provider_id: input.provider_id,
        provider_name: input.provider_name.map(|name| name.trim().to_owned()),
        create_provider,
        endpoint_id,
        socks5_proxy,
        client,
        base_url,
        device_auth_id: String::new(),
        user_code: String::new(),
        verification_uri: "",
        interval_seconds: 0,
        next_poll_at: 0,
        poll_lock: Arc::new(Mutex::new(())),
        expires_at: now.saturating_add(browser.expires_in_seconds),
        status: FlowStatus::Pending,
        browser_state: Some(browser.state),
        code_verifier: Some(browser.code_verifier),
    };
    let view = BrowserFlowView {
        id: id.clone(),
        authorization_url: browser.authorization_url,
        expires_at: flow.expires_at,
    };
    let mut flows = state.openai_oauth.flows.lock().await;
    flows.retain(|_, existing| existing.expires_at > now);
    flows.insert(id, flow);
    Ok(view)
}

pub async fn complete_browser(
    state: &AppState,
    id: &str,
    input: CompleteBrowserAuthorization,
) -> Result<(), CompleteError> {
    let flow = {
        let flows = state.openai_oauth.flows.lock().await;
        let flow = flows.get(id).ok_or_else(|| {
            CompleteError::Invalid("OpenAI browser sign-in flow not found".to_owned())
        })?;
        if flow.kind != FlowKind::Browser || !matches!(flow.status, FlowStatus::Pending) {
            return Err(CompleteError::Invalid(
                "OpenAI browser sign-in flow is no longer pending".to_owned(),
            ));
        }
        if flow.expires_at <= crate::auth::now() {
            return Err(CompleteError::Invalid(
                "OpenAI browser sign-in expired".to_owned(),
            ));
        }
        flow.clone()
    };
    let _guard = flow.poll_lock.clone().try_lock_owned().map_err(|_| {
        CompleteError::Invalid("OpenAI browser sign-in is already being completed".to_owned())
    })?;
    let (code, callback_state) =
        parse_browser_callback(&input.redirect_url).map_err(CompleteError::Invalid)?;
    let expected_state = flow.browser_state.as_deref().unwrap_or_default();
    if !crate::auth::constant_time_eq(&callback_state, expected_state) {
        return Err(CompleteError::Invalid(
            "OpenAI callback state does not match this sign-in".to_owned(),
        ));
    }
    let implementation = state
        .extensions
        .subscription_provider("openai_codex")
        .ok_or_else(|| {
            CompleteError::Invalid("OpenAI Subscription Extension is not enabled".to_owned())
        })?;
    let credential = implementation
        .exchange_browser_authorization(
            &flow.client,
            &code,
            flow.code_verifier.as_deref().unwrap_or_default(),
        )
        .await
        .map_err(CompleteError::Upstream)?;
    attach_subscription(state, &flow, credential.into())
        .await
        .map_err(CompleteError::Internal)?;
    state.openai_oauth.flows.lock().await.remove(id);
    Ok(())
}

pub async fn poll(state: &AppState, id: &str) -> Result<DeviceFlowView, String> {
    let now = crate::auth::now();
    let flow = {
        let mut flows = state.openai_oauth.flows.lock().await;
        let flow = flows
            .get_mut(id)
            .ok_or_else(|| "OpenAI sign-in flow not found".to_owned())?;
        if flow.kind != FlowKind::Device {
            return Err("OpenAI device sign-in flow not found".to_owned());
        }
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

    let implementation = state
        .extensions
        .subscription_provider("openai_codex")
        .ok_or_else(|| "OpenAI Subscription Extension is not enabled".to_owned())?;
    match implementation
        .poll_device_authorization(&flow.client, &flow.device_auth_id, &flow.user_code)
        .await
    {
        Ok(None) => {}
        Ok(Some(credential)) => {
            if let Err(error) = attach_subscription(state, &flow, credential.into()).await {
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
    let implementation = state
        .extensions
        .subscription_provider("openai_codex")
        .ok_or_else(|| "OpenAI Subscription Extension is not enabled".to_owned())?;
    let refreshed = implementation
        .refresh_credential(&client, &credential.refresh_token)
        .await?;
    let refreshed = refreshed.into();
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
    // Recheck after obtaining the Provider lock so a flow that completed its
    // upstream exchange before disablement cannot attach a new Endpoint afterward.
    let implementation = state
        .extensions
        .provider_endpoint("openai_codex")
        .ok_or_else(|| "OpenAI Subscription Extension is not enabled".to_owned())?;
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
    let models = implementation.models();
    provider
        .discovered_models
        .extend(models.iter().map(|model| (*model).to_owned()));
    provider.discovered_models.sort();
    provider.discovered_models.dedup();
    for model in models {
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
        // Persisted for backward compatibility; the Extension owns the target path and wire behavior.
        base_url: flow.base_url.clone(),
        socks5_proxy: flow.socks5_proxy.clone(),
        extra_headers: HashMap::new(),
        extra_body: serde_json::Map::new(),
        requires_api_key: false,
        api_keys: Vec::new(),
        openai_subscription: Some(credential),
        ..ApiEndpoint::default()
    }
}

fn parse_browser_callback(value: &str) -> Result<(String, String), String> {
    let redirect = reqwest::Url::parse(value.trim())
        .map_err(|_| "Paste the complete OpenAI localhost callback URL".to_owned())?;
    if redirect.scheme() != "http"
        || redirect.host_str() != Some("localhost")
        || redirect.port() != Some(1455)
        || redirect.path() != "/auth/callback"
        || !redirect.username().is_empty()
        || redirect.password().is_some()
        || redirect.fragment().is_some()
    {
        return Err("Callback URL must start with http://localhost:1455/auth/callback".to_owned());
    }
    let codes: Vec<_> = redirect
        .query_pairs()
        .filter(|(name, _)| name == "code")
        .map(|(_, value)| value.into_owned())
        .collect();
    let states: Vec<_> = redirect
        .query_pairs()
        .filter(|(name, _)| name == "state")
        .map(|(_, value)| value.into_owned())
        .collect();
    if codes.len() != 1 || states.len() != 1 || codes[0].is_empty() || states[0].is_empty() {
        return Err("Callback URL must contain exactly one non-empty code and state".to_owned());
    }
    Ok((codes[0].clone(), states[0].clone()))
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
        verification_uri: flow.verification_uri,
        interval_seconds: flow.interval_seconds,
        expires_at: flow.expires_at,
        error,
    }
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

    use tokio::sync::Mutex;

    use crate::config::OpenAiSubscription;

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
    fn browser_callback_accepts_only_the_registered_complete_url() {
        assert_eq!(
            super::parse_browser_callback(
                "http://localhost:1455/auth/callback?code=code%20123&state=state-456"
            )
            .unwrap(),
            ("code 123".to_owned(), "state-456".to_owned()),
        );
        for invalid in [
            "http://127.0.0.1:1455/auth/callback?code=x&state=y",
            "http://localhost:1456/auth/callback?code=x&state=y",
            "http://localhost:1455/other?code=x&state=y",
            "http://attacker@localhost:1455/auth/callback?code=x&state=y",
            "http://localhost:1455/auth/callback?code=x&code=z&state=y",
            "http://localhost:1455/auth/callback?code=&state=y",
            "http://localhost:1455/auth/callback?code=x&state=",
        ] {
            assert!(super::parse_browser_callback(invalid).is_err(), "{invalid}");
        }
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
            kind: super::FlowKind::Device,
            provider_id: "openai".to_owned(),
            provider_name: None,
            create_provider: true,
            endpoint_id: "chatgpt".to_owned(),
            socks5_proxy: Some("socks5h://127.0.0.1:1080".to_owned()),
            client: reqwest::Client::new(),
            base_url: "https://chatgpt.com/backend-api".to_owned(),
            device_auth_id: "device".to_owned(),
            user_code: "CODE".to_owned(),
            verification_uri: "https://auth.openai.com/codex/device",
            interval_seconds: 1,
            next_poll_at: 0,
            poll_lock: Arc::new(Mutex::new(())),
            expires_at: u64::MAX,
            status: super::FlowStatus::Pending,
            browser_state: None,
            code_verifier: None,
        };

        let endpoint = super::subscription_endpoint(&flow, credential);
        assert_eq!(
            endpoint.socks5_proxy.as_deref(),
            Some("socks5h://127.0.0.1:1080"),
        );
        assert_eq!(endpoint.api_type, crate::config::ApiType::OpenaiCodex);
    }
}

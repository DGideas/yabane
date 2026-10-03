use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

use crate::config::{
    ApiEndpoint, ApiType, AppState, Credential, CredentialMaterial, Provider, save_providers,
};

#[derive(Clone, Default)]
pub struct SignInState {
    flows: Arc<Mutex<HashMap<String, DeviceFlow>>>,
    refresh_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

/// Where a completed sign-in attaches its account: a new Provider, a new
/// Endpoint on an existing Provider, or the credential layer of an existing
/// subscription Endpoint.
#[derive(Clone, Copy, PartialEq)]
enum SubscriptionTarget {
    NewProvider,
    NewEndpoint,
    ExistingEndpoint,
}

#[derive(Clone)]
struct DeviceFlow {
    kind: FlowKind,
    /// The Endpoint type that owns this sign-in, as declared by its Extension.
    endpoint_type: &'static str,
    provider_id: String,
    provider_name: Option<String>,
    target: SubscriptionTarget,
    endpoint_id: String,
    socks5_proxy: Option<String>,
    client: reqwest::Client,
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
    endpoint_type: &str,
    input: StartSubscription,
) -> Result<DeviceFlowView, StartError> {
    if !valid_id(&input.provider_id) {
        return Err(StartError::Invalid(
            "Provider ID must be a URL slug".to_owned(),
        ));
    }
    let implementation = sign_in_provider(state, endpoint_type)?;
    let endpoint_id = input.endpoint_id.unwrap_or_else(|| {
        implementation
            .endpoint_type()
            .default_endpoint_id
            .to_owned()
    });
    if !valid_id(&endpoint_id) {
        return Err(StartError::Invalid(
            "Endpoint ID must be a URL slug".to_owned(),
        ));
    }
    let (socks5_proxy, client) = sign_in_client(
        state,
        &input.provider_id,
        &endpoint_id,
        input.socks5_proxy.as_deref(),
    )
    .await?;
    let target = {
        let providers = state.providers.read().await;
        subscription_target(
            &providers,
            endpoint_type,
            &input.provider_id,
            &endpoint_id,
            input.provider_name.as_deref(),
        )?
    };

    let device = implementation
        .start_device_authorization(&client)
        .await
        .map_err(StartError::Upstream)?;
    let now = crate::auth::now();
    let id = random_id();
    let flow = DeviceFlow {
        kind: FlowKind::Device,
        endpoint_type: implementation.endpoint_type().id,
        provider_id: input.provider_id,
        provider_name: input.provider_name.map(|name| name.trim().to_owned()),
        target,
        endpoint_id,
        socks5_proxy,
        client,
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
    let mut flows = state.sign_in.flows.lock().await;
    flows.retain(|_, existing| existing.expires_at > now);
    flows.insert(id, flow);
    Ok(view)
}

pub async fn start_browser(
    state: &AppState,
    endpoint_type: &str,
    input: StartSubscription,
) -> Result<BrowserFlowView, StartError> {
    if !valid_id(&input.provider_id) {
        return Err(StartError::Invalid(
            "Provider ID must be a URL slug".to_owned(),
        ));
    }
    let implementation = sign_in_provider(state, endpoint_type)?;
    if !implementation
        .endpoint_type()
        .sign_in
        .is_some_and(|sign_in| sign_in.browser)
    {
        return Err(StartError::Invalid(format!(
            "{} does not offer browser sign-in",
            implementation.endpoint_type().display_name
        )));
    }
    let endpoint_id = input.endpoint_id.unwrap_or_else(|| {
        implementation
            .endpoint_type()
            .default_endpoint_id
            .to_owned()
    });
    if !valid_id(&endpoint_id) {
        return Err(StartError::Invalid(
            "Endpoint ID must be a URL slug".to_owned(),
        ));
    }
    let (socks5_proxy, client) = sign_in_client(
        state,
        &input.provider_id,
        &endpoint_id,
        input.socks5_proxy.as_deref(),
    )
    .await?;
    let target = {
        let providers = state.providers.read().await;
        subscription_target(
            &providers,
            endpoint_type,
            &input.provider_id,
            &endpoint_id,
            input.provider_name.as_deref(),
        )?
    };
    let browser = implementation
        .start_browser_authorization()
        .map_err(StartError::Upstream)?;
    let now = crate::auth::now();
    let id = random_id();
    let flow = DeviceFlow {
        kind: FlowKind::Browser,
        endpoint_type: implementation.endpoint_type().id,
        provider_id: input.provider_id,
        provider_name: input.provider_name.map(|name| name.trim().to_owned()),
        target,
        endpoint_id,
        socks5_proxy,
        client,
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
    let mut flows = state.sign_in.flows.lock().await;
    flows.retain(|_, existing| existing.expires_at > now);
    flows.insert(id, flow);
    Ok(view)
}

pub async fn complete_browser(
    state: &AppState,
    endpoint_type: &str,
    id: &str,
    input: CompleteBrowserAuthorization,
) -> Result<(), CompleteError> {
    let flow = {
        let flows = state.sign_in.flows.lock().await;
        let flow = flows
            .get(id)
            .ok_or_else(|| CompleteError::Invalid("Browser sign-in flow not found".to_owned()))?;
        if flow.kind != FlowKind::Browser || !matches!(flow.status, FlowStatus::Pending) {
            return Err(CompleteError::Invalid(
                "Browser sign-in flow is no longer pending".to_owned(),
            ));
        }
        if flow.expires_at <= crate::auth::now() {
            return Err(CompleteError::Invalid("Browser sign-in expired".to_owned()));
        }
        flow.clone()
    };
    let _guard = flow.poll_lock.clone().try_lock_owned().map_err(|_| {
        CompleteError::Invalid("This browser sign-in is already being completed".to_owned())
    })?;
    let (code, callback_state) =
        parse_browser_callback(&input.redirect_url).map_err(CompleteError::Invalid)?;
    let expected_state = flow.browser_state.as_deref().unwrap_or_default();
    if !crate::auth::constant_time_eq(&callback_state, expected_state) {
        return Err(CompleteError::Invalid(
            "The callback state does not match this sign-in".to_owned(),
        ));
    }
    if flow.endpoint_type != endpoint_type {
        return Err(CompleteError::Invalid(
            "This sign-in belongs to another Endpoint type".to_owned(),
        ));
    }
    let implementation = state
        .extensions
        .subscription_provider(endpoint_type)
        .ok_or_else(|| {
            CompleteError::Invalid(format!(
                "Endpoint type '{endpoint_type}' is not available because its Extension is not enabled"
            ))
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
    state.sign_in.flows.lock().await.remove(id);
    Ok(())
}

pub async fn poll(state: &AppState, id: &str) -> Result<DeviceFlowView, String> {
    let now = crate::auth::now();
    let flow = {
        let mut flows = state.sign_in.flows.lock().await;
        let flow = flows
            .get_mut(id)
            .ok_or_else(|| "Sign-in flow not found".to_owned())?;
        if flow.kind != FlowKind::Device {
            return Err("Device sign-in flow not found".to_owned());
        }
        match &flow.status {
            FlowStatus::Pending if flow.expires_at <= now => {
                flow.status = FlowStatus::Failed("Sign-in expired".to_owned());
                return Ok(flow_view(id, flow));
            }
            FlowStatus::Pending if flow.next_poll_at > now => return Ok(flow_view(id, flow)),
            FlowStatus::Pending => flow.next_poll_at = now.saturating_add(flow.interval_seconds),
            _ => return Ok(flow_view(id, flow)),
        }
        flow.clone()
    };
    let Ok(_poll_guard) = flow.poll_lock.clone().try_lock_owned() else {
        let flows = state.sign_in.flows.lock().await;
        return flows
            .get(id)
            .map(|current| flow_view(id, current))
            .ok_or_else(|| "Sign-in flow not found".to_owned());
    };

    // The flow remembers the Endpoint type it belongs to, so Core never names a
    // vendor to find the implementation that owns it.
    let implementation = state
        .extensions
        .subscription_provider(flow.endpoint_type)
        .ok_or_else(|| {
            format!(
                "Endpoint type '{}' is not available because its Extension is not enabled",
                flow.endpoint_type
            )
        })?;
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
    let flows = state.sign_in.flows.lock().await;
    flows
        .get(id)
        .map(|flow| flow_view(id, flow))
        .ok_or_else(|| "Sign-in flow not found".to_owned())
}

/// Returns the subscription credential with a usable access token, renewing it
/// under the credential's own lock when it is about to expire.
pub async fn refreshed_credential(
    state: &AppState,
    provider_id: &str,
    endpoint_id: &str,
    credential_id: &str,
) -> Result<Credential, String> {
    let lock_key = format!("{provider_id}/{endpoint_id}/{credential_id}");
    let refresh_lock = {
        let mut locks = state.sign_in.refresh_locks.lock().await;
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
        .ok_or_else(|| "The Endpoint no longer exists".to_owned())?;
    let credential = endpoint
        .credentials
        .iter()
        .find(|credential| credential.id == credential_id)
        .cloned()
        .ok_or_else(|| "The Endpoint's account no longer exists".to_owned())?;
    let endpoint_type = endpoint.extension_endpoint_type().ok_or_else(|| {
        "This Endpoint type does not sign accounts in, so it has nothing to refresh".to_owned()
    })?;
    let subscription = credential.subscription().ok_or_else(|| {
        "This Endpoint type refreshes signed-in accounts, but the credential is not one".to_owned()
    })?;
    if subscription.expires_at > crate::auth::now().saturating_add(60) {
        return Ok(credential);
    }
    let client = endpoint.client(&state.client, state.upstream_timeouts)?;
    let implementation = state
        .extensions
        .subscription_provider(endpoint_type)
        .ok_or_else(|| {
            format!(
                "Endpoint type '{endpoint_type}' is not available because its Extension is not enabled"
            )
        })?;
    let refreshed = implementation
        .refresh_credential(&client, subscription.refresh_token)
        .await?;
    let refreshed: CredentialMaterial = refreshed.into();
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
        .and_then(|endpoint| {
            endpoint
                .credentials
                .iter_mut()
                .find(|credential| credential.id == credential_id)
        })
        .ok_or_else(|| "The Endpoint's account no longer exists".to_owned())?;
    if !replace_subscription_if_current(stored, &credential, refreshed) {
        return Ok(stored.clone());
    }
    save_providers(&updated)
        .await
        .map_err(|err| format!("Could not persist the refreshed account: {err}"))?;
    *providers = updated;
    providers
        .get(provider_id)
        .and_then(|provider| {
            provider
                .endpoints
                .iter()
                .find(|endpoint| endpoint.id == endpoint_id)
        })
        .and_then(|endpoint| {
            endpoint
                .credentials
                .iter()
                .find(|credential| credential.id == credential_id)
        })
        .cloned()
        .ok_or_else(|| "The connected account no longer exists".to_owned())
}

/// Builds the client this Endpoint type's sign-in, token exchange, and refresh use.
/// Connecting one more account to an existing Endpoint reuses that Endpoint's
/// SOCKS5 proxy so the new account is reached the same way as the accounts
/// already attached to it.
async fn sign_in_client(
    state: &AppState,
    provider_id: &str,
    endpoint_id: &str,
    requested_proxy: Option<&str>,
) -> Result<(Option<String>, reqwest::Client), StartError> {
    let requested = normalized_socks5_proxy(requested_proxy).map_err(StartError::Invalid)?;
    let stored = if requested.is_none() {
        state
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
            .and_then(|endpoint| endpoint.socks5_proxy.clone())
    } else {
        None
    };
    let socks5_proxy = requested.or(stored);
    let client = ApiEndpoint {
        id: endpoint_id.to_owned(),
        socks5_proxy: socks5_proxy.clone(),
        ..ApiEndpoint::default()
    }
    .client(&state.client, state.upstream_timeouts)
    .map_err(StartError::Invalid)?;
    Ok((socks5_proxy, client))
}

/// Decides where a new sign-in lands before the user starts it, so the console
/// can tell apart connecting a Provider, adding an Endpoint, and connecting one
/// more account to an Endpoint that already serves traffic.
fn subscription_target(
    providers: &HashMap<String, Provider>,
    endpoint_type: &str,
    provider_id: &str,
    endpoint_id: &str,
    provider_name: Option<&str>,
) -> Result<SubscriptionTarget, StartError> {
    match providers.get(provider_id) {
        Some(provider) => match provider
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == endpoint_id)
        {
            Some(endpoint) if endpoint.extension_endpoint_type() == Some(endpoint_type) => {
                Ok(SubscriptionTarget::ExistingEndpoint)
            }
            Some(_) => Err(StartError::Invalid(format!(
                "Endpoint '{endpoint_id}' is not an Endpoint of this type"
            ))),
            None => Ok(SubscriptionTarget::NewEndpoint),
        },
        None if provider_name.is_none_or(|name| name.trim().is_empty()) => Err(
            StartError::Invalid("Provider name is required when creating a Provider".to_owned()),
        ),
        None => Ok(SubscriptionTarget::NewProvider),
    }
}

async fn attach_subscription(
    state: &AppState,
    flow: &DeviceFlow,
    material: CredentialMaterial,
) -> Result<(), String> {
    let mut providers = state.providers.write().await;
    // Recheck after obtaining the Provider lock so a flow that completed its
    // upstream exchange before disablement cannot attach a new Endpoint afterward.
    let implementation = state
        .extensions
        .provider_endpoint(flow.endpoint_type)
        .ok_or_else(|| {
            format!(
                "Endpoint type '{}' is not available because its Extension is not enabled",
                flow.endpoint_type
            )
        })?;
    let declaration: &'static yabane_extension_api::ProviderEndpointType = state
        .extensions
        .endpoint_type_declaration(flow.endpoint_type)
        .ok_or_else(|| {
            format!(
                "Endpoint type '{}' is not available because its Extension is not enabled",
                flow.endpoint_type
            )
        })?;
    let mut updated = providers.clone();
    match flow.target {
        SubscriptionTarget::NewProvider => {
            if updated.contains_key(&flow.provider_id) {
                return Err("Provider ID already exists".to_owned());
            }
            let mut provider = Provider {
                id: flow.provider_id.clone(),
                name: flow
                    .provider_name
                    .clone()
                    .unwrap_or_else(|| declaration.display_name.to_owned()),
                extra_headers: HashMap::new(),
                extra_body: serde_json::Map::new(),
                pricing: None,
                defaults_endpoint_ids: Vec::new(),
                endpoints: Vec::new(),
                discovered_models: Vec::new(),
                model_endpoints: HashMap::new(),
                model_endpoint_preferences: Vec::new(),
                models_discovered_at: None,
                model_discovery_error: None,
            };
            provider
                .endpoints
                .push(signed_in_endpoint(flow, declaration, material)?);
            register_subscription_models(&mut provider, &flow.endpoint_id, implementation);
            updated.insert(flow.provider_id.clone(), provider);
        }
        SubscriptionTarget::NewEndpoint => {
            let Some(provider) = updated.get_mut(&flow.provider_id) else {
                return Err("Provider no longer exists".to_owned());
            };
            if provider
                .endpoints
                .iter()
                .any(|endpoint| endpoint.id == flow.endpoint_id)
            {
                return Err("Endpoint ID already exists".to_owned());
            }
            provider
                .endpoints
                .push(signed_in_endpoint(flow, declaration, material)?);
            register_subscription_models(provider, &flow.endpoint_id, implementation);
        }
        SubscriptionTarget::ExistingEndpoint => {
            let Some(provider) = updated.get_mut(&flow.provider_id) else {
                return Err("Provider no longer exists".to_owned());
            };
            let Some(endpoint) = provider
                .endpoints
                .iter_mut()
                .find(|endpoint| endpoint.id == flow.endpoint_id)
            else {
                return Err("Endpoint no longer exists".to_owned());
            };
            if endpoint.extension_endpoint_type() != Some(flow.endpoint_type) {
                return Err("The Endpoint is no longer of this type".to_owned());
            }
            attach_subscription_credential(endpoint, declaration, material)?;
        }
    }
    save_providers(&updated)
        .await
        .map_err(|err| format!("Could not save the connected account: {err}"))?;
    *providers = updated;
    Ok(())
}

/// The catalog an account-based Endpoint type exposes comes from the Extension,
/// not from an upstream `/models` call, so every account on an Endpoint shares it.
fn register_subscription_models(
    provider: &mut Provider,
    endpoint_id: &str,
    implementation: &dyn yabane_extension_api::ProviderEndpoint,
) {
    let models = implementation.models();
    provider
        .discovered_models
        .extend(models.iter().map(|model| (*model).to_owned()));
    provider.discovered_models.sort();
    provider.discovered_models.dedup();
    for model in models {
        let endpoint_ids = provider
            .model_endpoints
            .entry((*model).to_owned())
            .or_default();
        if !endpoint_ids.iter().any(|id| id == endpoint_id) {
            endpoint_ids.push(endpoint_id.to_owned());
        }
    }
    provider.models_discovered_at = Some(crate::auth::now());
    provider.model_discovery_error = None;
}

/// The identity kind a signed-in Endpoint owns: the first kind its Endpoint type
/// declares as something that expires.
fn signed_in_credential_kind(
    declaration: &yabane_extension_api::ProviderEndpointType,
) -> Result<&'static yabane_extension_api::ProviderCredentialKind, String> {
    declaration
        .credential_kinds
        .iter()
        .find(|kind| kind.flow == yabane_extension_api::CredentialFlow::Subscription)
        .ok_or_else(|| {
            format!(
                "Endpoint type '{}' offers sign-in but declares no account identity",
                declaration.id
            )
        })
}

fn signed_in_endpoint(
    flow: &DeviceFlow,
    declaration: &yabane_extension_api::ProviderEndpointType,
    material: CredentialMaterial,
) -> Result<ApiEndpoint, String> {
    let kind = signed_in_credential_kind(declaration)?.id.to_owned();
    let (id, name) = new_signed_in_identity(&[], kind_label(declaration));
    Ok(ApiEndpoint {
        id: flow.endpoint_id.clone(),
        api_type: ApiType::Extension(declaration.id),
        // The declaration owns the connection; this value keeps configuration
        // readable without Core relying on it.
        base_url: declaration.fixed_base_url.unwrap_or_default().to_owned(),
        socks5_proxy: flow.socks5_proxy.clone(),
        extra_headers: HashMap::new(),
        extra_body: serde_json::Map::new(),
        requires_credential: true,
        credentials: vec![Credential {
            id,
            name,
            weight: 100,
            enabled: true,
            priority: 1,
            kind,
            material,
        }],
        ..ApiEndpoint::default()
    })
}

fn kind_label(declaration: &yabane_extension_api::ProviderEndpointType) -> &'static str {
    declaration
        .credential_kinds
        .iter()
        .find(|kind| kind.flow == yabane_extension_api::CredentialFlow::Subscription)
        .map(|kind| kind.label)
        .unwrap_or("Account")
}

/// Subscription credentials are named generically because the account identity
/// is never exposed through the management API; the administrator can rename
/// them once several accounts exist.
/// Connects one more account to an Endpoint that already has an identity layer.
/// The Endpoint keeps one credential per signed-in account, so an account that is
/// already connected is rejected instead of appearing twice.
fn attach_subscription_credential(
    endpoint: &mut ApiEndpoint,
    declaration: &yabane_extension_api::ProviderEndpointType,
    material: CredentialMaterial,
) -> Result<(), String> {
    let account_id = material
        .subscription()
        .map(|subscription| subscription.account_id.to_owned())
        .ok_or_else(|| "Sign-in did not return an account".to_owned())?;
    if endpoint.credentials.iter().any(|credential| {
        credential
            .subscription()
            .is_some_and(|subscription| subscription.account_id == account_id)
    }) {
        return Err("This account is already connected to the Endpoint".to_owned());
    }
    let (id, name) = new_signed_in_identity(&endpoint.credentials, kind_label(declaration));
    endpoint.credentials.push(Credential {
        id,
        name,
        weight: 100,
        enabled: true,
        priority: 1,
        kind: signed_in_credential_kind(declaration)?.id.to_owned(),
        material,
    });
    Ok(())
}

/// Accounts are named generically because the account identity itself is never
/// exposed through the management API; the administrator can rename them once
/// several accounts exist.
fn new_signed_in_identity(credentials: &[Credential], label: &str) -> (String, String) {
    (1..)
        .map(|index| {
            let id = if index == 1 {
                "account".to_owned()
            } else {
                format!("account-{index}")
            };
            let name = if index == 1 {
                label.to_owned()
            } else {
                format!("{label} {index}")
            };
            (id, name)
        })
        .find(|(id, _)| !credentials.iter().any(|credential| credential.id == *id))
        .expect("finite credential ID space")
}

/// The signed-in counterpart of [`sign_in_flow`]: an Endpoint type a sign-in can
/// be started for.
fn sign_in_provider(
    state: &AppState,
    endpoint_type: &str,
) -> Result<&'static dyn yabane_extension_api::SubscriptionProvider, StartError> {
    let implementation = state
        .extensions
        .subscription_provider(endpoint_type)
        .ok_or_else(|| {
            StartError::Invalid(format!(
                "Endpoint type '{endpoint_type}' is not available because its Extension is not enabled"
            ))
        })?;
    if implementation.endpoint_type().sign_in.is_none() {
        return Err(StartError::Invalid(format!(
            "{} does not offer sign-in",
            implementation.endpoint_type().display_name
        )));
    }
    Ok(implementation)
}

fn parse_browser_callback(value: &str) -> Result<(String, String), String> {
    let redirect = reqwest::Url::parse(value.trim())
        .map_err(|_| "Paste the complete localhost callback URL".to_owned())?;
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
    if let Some(flow) = state.sign_in.flows.lock().await.get_mut(id) {
        flow.status = status;
    }
}

fn replace_subscription_if_current(
    stored: &mut Credential,
    expected: &Credential,
    refreshed: CredentialMaterial,
) -> bool {
    let Some(current) = stored.subscription() else {
        return false;
    };
    let Some(previous) = expected.subscription() else {
        return false;
    };
    // A sign-in that replaced this credential while the refresh was in flight
    // owns the newer account, so the stale result is dropped.
    if current.account_id != previous.account_id || current.refresh_token != previous.refresh_token
    {
        return false;
    }
    stored.material = refreshed;
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

    fn subscription_credential(
        id: &str,
        access_token: &str,
        refresh_token: &str,
    ) -> crate::config::Credential {
        crate::config::Credential {
            id: id.to_owned(),
            name: id.to_owned(),
            weight: 100,
            enabled: true,
            priority: 1,
            kind: "account".to_owned(),
            material: crate::config::CredentialMaterial::Subscription {
                access_token: access_token.to_owned(),
                refresh_token: refresh_token.to_owned(),
                expires_at: 1,
                account_id: "account".to_owned(),
            },
        }
    }

    fn provider(id: &str, endpoint: crate::config::ApiEndpoint) -> crate::config::Provider {
        crate::config::Provider {
            id: id.to_owned(),
            name: id.to_owned(),
            extra_headers: std::collections::HashMap::new(),
            extra_body: serde_json::Map::new(),
            pricing: None,
            defaults_endpoint_ids: Vec::new(),
            endpoints: vec![endpoint],
            discovered_models: Vec::new(),
            model_endpoints: std::collections::HashMap::new(),
            model_endpoint_preferences: Vec::new(),
            models_discovered_at: None,
            model_discovery_error: None,
        }
    }

    /// The console addresses sign-in by Endpoint type alongside the Provider and
    /// Endpoint it belongs to; the extra field must not turn a valid request into
    /// a rejected one.
    #[test]
    fn a_sign_in_request_carries_the_endpoint_type_the_console_sends() {
        let input: super::StartSubscription = serde_json::from_str(
            r#"{"endpoint_type":"openai_codex","provider_id":"plan","endpoint_id":"chatgpt"}"#,
        )
        .expect("the console body is accepted");
        assert_eq!(input.provider_id, "plan");
        assert_eq!(input.endpoint_id.as_deref(), Some("chatgpt"));
        assert!(input.provider_name.is_none() && input.socks5_proxy.is_none());
    }

    const DECLARATION_KINDS: &[yabane_extension_api::ProviderCredentialKind] =
        &[yabane_extension_api::ProviderCredentialKind {
            id: "account",
            label: "Account",
            flow: yabane_extension_api::CredentialFlow::Subscription,
        }];

    fn test_declaration() -> yabane_extension_api::ProviderEndpointType {
        // Core's identity lifecycle must be testable without a bundled Extension.
        yabane_extension_api::ProviderEndpointType {
            id: "openai_codex",
            display_name: "Test account Endpoint",
            description: "Test-only sign-in declaration",
            default_endpoint_id: "account",
            fixed_base_url: Some("https://accounts.example.test/api"),
            upstream_protocol: yabane_extension_api::Protocol::OpenAiResponses,
            always_event_stream: true,
            surfaces: &[yabane_extension_api::Protocol::OpenAiResponses],
            credential_kinds: DECLARATION_KINDS,
            sign_in: Some(yabane_extension_api::ProviderSignIn {
                device_code: true,
                browser: true,
            }),
        }
    }

    fn subscription_endpoint(id: &str) -> crate::config::ApiEndpoint {
        crate::config::ApiEndpoint {
            id: id.to_owned(),
            api_type: crate::config::ApiType::Extension("openai_codex"),
            ..crate::config::ApiEndpoint::default()
        }
    }

    #[test]
    fn a_new_account_takes_the_next_free_identity_on_its_endpoint() {
        let mut credentials = Vec::new();
        assert_eq!(
            super::new_signed_in_identity(&credentials, "Account"),
            ("account".to_owned(), "Account".to_owned()),
        );
        credentials.push(subscription_credential("account", "a", "r"));
        assert_eq!(
            super::new_signed_in_identity(&credentials, "Account"),
            ("account-2".to_owned(), "Account 2".to_owned()),
        );
        credentials.push(subscription_credential("account-2", "b", "r"));
        assert_eq!(
            super::new_signed_in_identity(&credentials, "Account"),
            ("account-3".to_owned(), "Account 3".to_owned()),
        );
    }

    #[test]
    fn one_connected_account_never_becomes_two_credentials() {
        let mut endpoint = subscription_endpoint("chatgpt");
        let declaration = test_declaration();
        let account = |account_id: &str, refresh_token: &str| {
            crate::config::CredentialMaterial::Subscription {
                access_token: "access".to_owned(),
                refresh_token: refresh_token.to_owned(),
                expires_at: 1,
                account_id: account_id.to_owned(),
            }
        };

        super::attach_subscription_credential(
            &mut endpoint,
            &declaration,
            account("account", "r1"),
        )
        .expect("first account connects");
        let error = super::attach_subscription_credential(
            &mut endpoint,
            &declaration,
            account("account", "r2"),
        )
        .expect_err("the same account is rejected");
        assert!(error.contains("already connected"));
        assert_eq!(endpoint.credentials.len(), 1);

        super::attach_subscription_credential(
            &mut endpoint,
            &declaration,
            account("account-2", "r3"),
        )
        .expect("another account connects");
        assert_eq!(
            endpoint
                .credentials
                .iter()
                .map(|credential| credential.id.as_str())
                .collect::<Vec<_>>(),
            ["account", "account-2"],
        );

        let error = super::attach_subscription_credential(
            &mut endpoint,
            &declaration,
            crate::config::CredentialMaterial::Secret {
                secret: "sk-test".to_owned(),
            },
        )
        .expect_err("a secret is not a signed-in account");
        assert!(error.contains("did not return an account"));
    }

    #[test]
    fn sign_in_lands_on_the_provider_endpoint_or_account_layer_it_names() {
        let providers = std::collections::HashMap::from([
            (
                "openai".to_owned(),
                provider("openai", subscription_endpoint("chatgpt")),
            ),
            (
                "plain".to_owned(),
                provider(
                    "plain",
                    crate::config::ApiEndpoint {
                        id: "openai".to_owned(),
                        ..crate::config::ApiEndpoint::default()
                    },
                ),
            ),
        ]);

        assert!(matches!(
            super::subscription_target(&providers, "openai_codex", "openai", "chatgpt", None),
            Ok(super::SubscriptionTarget::ExistingEndpoint),
        ));
        assert!(matches!(
            super::subscription_target(&providers, "openai_codex", "openai", "second", None),
            Ok(super::SubscriptionTarget::NewEndpoint),
        ));
        assert!(matches!(
            super::subscription_target(
                &providers,
                "openai_codex",
                "fresh",
                "chatgpt",
                Some("Fresh")
            ),
            Ok(super::SubscriptionTarget::NewProvider),
        ));
        // A Provider that must be created needs a name, and only a subscription
        // Endpoint can receive another connected account.
        assert!(
            super::subscription_target(&providers, "openai_codex", "fresh", "chatgpt", None)
                .is_err()
        );
        assert!(
            super::subscription_target(&providers, "openai_codex", "plain", "openai", None)
                .is_err()
        );
    }

    #[test]
    fn stale_refresh_cannot_overwrite_a_reconnected_credential() {
        let stale = subscription_credential("account", "stale-access", "stale-refresh");
        let current = subscription_credential("account", "current-access", "current-refresh");
        let refreshed = crate::config::CredentialMaterial::Subscription {
            access_token: "refreshed-stale-access".to_owned(),
            refresh_token: "refreshed-stale-refresh".to_owned(),
            expires_at: 3,
            account_id: "account".to_owned(),
        };
        let mut stored = current.clone();

        assert!(!super::replace_subscription_if_current(
            &mut stored,
            &stale,
            refreshed,
        ));
        assert_eq!(
            stored.subscription().unwrap().access_token,
            "current-access",
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
        let material = crate::config::CredentialMaterial::Subscription {
            access_token: "access".to_owned(),
            refresh_token: "refresh".to_owned(),
            expires_at: 1,
            account_id: "account".to_owned(),
        };
        let flow = super::DeviceFlow {
            kind: super::FlowKind::Device,
            endpoint_type: "openai_codex",
            provider_id: "openai".to_owned(),
            provider_name: None,
            target: super::SubscriptionTarget::NewProvider,
            endpoint_id: "chatgpt".to_owned(),
            socks5_proxy: Some("socks5h://127.0.0.1:1080".to_owned()),
            client: reqwest::Client::new(),
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

        let endpoint = super::signed_in_endpoint(&flow, &test_declaration(), material)
            .expect("a signed-in Endpoint is built from the declaration");
        assert_eq!(
            endpoint.socks5_proxy.as_deref(),
            Some("socks5h://127.0.0.1:1080"),
        );
        assert_eq!(
            endpoint.api_type,
            crate::config::ApiType::Extension("openai_codex")
        );
        assert_eq!(
            endpoint.base_url,
            test_declaration().fixed_base_url.unwrap()
        );
        assert!(endpoint.requires_credential);
        assert_eq!(endpoint.credentials.len(), 1);
        assert!(endpoint.credentials[0].subscription().is_some());
    }

    #[test]
    fn additional_accounts_get_distinct_credential_identities() {
        let mut credentials = vec![subscription_credential("account", "a", "r")];
        assert_eq!(
            super::new_signed_in_identity(&credentials, "Account"),
            ("account-2".to_owned(), "Account 2".to_owned())
        );
        credentials.push(subscription_credential("account-2", "b", "r"));
        assert_eq!(
            super::new_signed_in_identity(&credentials, "Account"),
            ("account-3".to_owned(), "Account 3".to_owned())
        );
    }
}

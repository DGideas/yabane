use std::{env, net::SocketAddr, sync::Arc};

use axum::{
    Router,
    extract::{Path, State},
    http::{StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpListener, sync::RwLock};
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

mod auth;
mod config;
mod gateway;
mod models;

use auth::{
    GatewayApiKey, GatewayApiKeyView, generate_secret, hash_secret, load_auth, now, save_auth,
};
use config::{
    ApiEndpoint, ApiKey, ApiType, AppState, ModelRoute, Provider, load_providers, save_providers,
};

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_CSS: &str = include_str!("../web/app.css");
const APP_JS: &str = include_str!("../web/app.js");
const UBUNTU_SANS_REGULAR: &[u8] = include_bytes!("../web/fonts/ubuntu-sans-regular.woff2");
const UBUNTU_SANS_MEDIUM: &[u8] = include_bytes!("../web/fonts/ubuntu-sans-medium.woff2");

#[derive(Deserialize)]
struct CreateProvider {
    id: String,
    name: String,
    endpoint: CreateEndpoint,
}

#[derive(Deserialize)]
struct CreateEndpoint {
    id: Option<String>,
    api_type: ApiType,
    base_url: String,
    requires_api_key: bool,
    api_key: Option<String>,
}

#[derive(Deserialize)]
struct CreateApiKey {
    endpoint_id: String,
    name: String,
    secret: String,
    weight: u32,
}

#[derive(Deserialize)]
struct UpdateApiKey {
    weight: Option<u32>,
    enabled: Option<bool>,
}

#[derive(Deserialize)]
struct CreateModelRoute {
    pattern: String,
    endpoint_id: String,
    api_key_id: String,
}

#[derive(Deserialize)]
struct UpdateAuthSettings {
    enabled: bool,
}

#[derive(Deserialize)]
struct CreateGatewayApiKey {
    #[serde(default)]
    note: String,
    expires_at: Option<u64>,
    #[serde(default)]
    provider_ids: Vec<String>,
}

#[derive(Serialize)]
struct AuthSettingsView {
    enabled: bool,
    api_keys: Vec<GatewayApiKeyView>,
}

#[derive(Serialize)]
struct CreatedGatewayApiKey {
    api_key: GatewayApiKeyView,
    secret: String,
}

#[derive(Serialize)]
struct ProviderView {
    id: String,
    name: String,
    endpoints: Vec<EndpointView>,
    discovered_models: Vec<String>,
    models_discovered_at: Option<u64>,
    model_discovery_error: Option<String>,
    model_routes: Vec<ModelRoute>,
}

#[derive(Serialize)]
struct EndpointView {
    id: String,
    api_type: ApiType,
    base_url: String,
    requires_api_key: bool,
    api_keys: Vec<ApiKeyView>,
}

#[derive(Serialize)]
struct ApiKeyView {
    id: String,
    name: String,
    weight: u32,
    enabled: bool,
}

#[derive(Serialize)]
struct ApiError {
    error: ApiErrorBody,
}

#[derive(Serialize)]
struct ApiErrorBody {
    message: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[tokio::main]
async fn main() {
    let log_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .with_env_var("YABANE_LOG")
        .from_env()
        .expect("YABANE_LOG must contain a valid tracing filter");
    tracing_subscriber::fmt().with_env_filter(log_filter).init();

    let providers = load_providers().await.expect("load provider configuration");
    let auth = load_auth()
        .await
        .expect("load authentication configuration");
    let state = AppState {
        client: reqwest::Client::builder()
            .pool_max_idle_per_host(64)
            .tcp_nodelay(true)
            .build()
            .expect("build HTTP client"),
        providers: Arc::new(RwLock::new(providers)),
        auth: Arc::new(RwLock::new(auth)),
    };

    let inference = Router::new()
        .route("/v1/models", get(models::list_models))
        .route("/v1/chat/completions", post(gateway::proxy_openai))
        .route("/v1/responses", post(gateway::proxy_openai))
        .route("/v1/messages", post(gateway::proxy_anthropic))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authorize,
        ));

    let app = Router::new()
        .route("/", get(index))
        .route("/app.css", get(css))
        .route("/app.js", get(js))
        .route("/fonts/ubuntu-sans-regular.woff2", get(ubuntu_sans_regular))
        .route("/fonts/ubuntu-sans-medium.woff2", get(ubuntu_sans_medium))
        .route("/healthz", get(health))
        .route(
            "/admin/auth",
            get(get_auth_settings).patch(update_auth_settings),
        )
        .route("/admin/auth/keys", post(create_gateway_api_key))
        .route("/admin/auth/keys/{id}", delete(delete_gateway_api_key))
        .route(
            "/admin/providers",
            get(list_providers).post(create_provider),
        )
        .route("/admin/providers/{id}", delete(delete_provider))
        .route(
            "/admin/providers/{id}/models/refresh",
            post(models::refresh_provider),
        )
        .route("/admin/providers/{id}/endpoints", post(create_endpoint))
        .route("/admin/providers/{id}/keys", post(create_api_key))
        .route(
            "/admin/providers/{provider_id}/keys/{key_id}",
            patch(update_api_key).delete(delete_api_key),
        )
        .route(
            "/admin/providers/{id}/model-routes",
            post(create_model_route),
        )
        .route(
            "/admin/providers/{provider_id}/model-routes/{pattern}",
            delete(delete_model_route),
        )
        .merge(inference)
        .with_state(state);

    let address: SocketAddr = env::var("YABANE_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse()
        .expect("YABANE_ADDR must be an address");
    let listener = TcpListener::bind(address).await.expect("bind server");
    let browser_host = match address.ip() {
        std::net::IpAddr::V4(ip) if ip.is_unspecified() => "127.0.0.1".to_owned(),
        std::net::IpAddr::V6(ip) if ip.is_unspecified() => "[::1]".to_owned(),
        ip => ip.to_string(),
    };
    let admin_url = format!("http://{browser_host}:{}/", address.port());
    info!(%address, %admin_url, "Yabane is ready");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("serve Yabane");
}

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
}

async fn css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS)
}

async fn js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
    )
}

async fn ubuntu_sans_regular() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "font/woff2")], UBUNTU_SANS_REGULAR)
}

async fn ubuntu_sans_medium() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "font/woff2")], UBUNTU_SANS_MEDIUM)
}

async fn health() -> &'static str {
    "ok"
}

async fn get_auth_settings(State(state): State<AppState>) -> impl IntoResponse {
    let auth = state.auth.read().await;
    let api_keys = auth.api_keys.iter().map(GatewayApiKeyView::from).collect();
    axum::Json(AuthSettingsView {
        enabled: auth.enabled,
        api_keys,
    })
}

async fn update_auth_settings(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<UpdateAuthSettings>,
) -> Response {
    let mut auth = state.auth.write().await;
    auth.enabled = input.enabled;
    persist_auth_or_error(&auth).await
}

async fn create_gateway_api_key(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<CreateGatewayApiKey>,
) -> Response {
    if input.expires_at.is_some_and(|expiry| expiry <= now()) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "API key expiry must be in the future",
        );
    }
    let providers = state.providers.read().await;
    if let Some(provider_id) = input
        .provider_ids
        .iter()
        .find(|id| !providers.contains_key(*id))
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            format!("Unknown provider '{provider_id}'"),
        );
    }
    drop(providers);
    let secret = generate_secret();
    let id = secret[3..15].to_owned();
    let key = GatewayApiKey {
        id,
        note: input.note.trim().to_owned(),
        secret_hash: hash_secret(&secret),
        prefix: format!("{}…{}", &secret[..10], &secret[secret.len() - 4..]),
        created_at: now(),
        expires_at: input.expires_at,
        provider_ids: input.provider_ids,
    };
    let view = GatewayApiKeyView::from(&key);
    let mut auth = state.auth.write().await;
    auth.api_keys.push(key);
    if let Err(err) = save_auth(&auth).await {
        error!(%err, "failed to persist authentication configuration");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save authentication configuration",
        );
    }
    (
        StatusCode::CREATED,
        axum::Json(CreatedGatewayApiKey {
            api_key: view,
            secret,
        }),
    )
        .into_response()
}

async fn delete_gateway_api_key(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let mut auth = state.auth.write().await;
    let count = auth.api_keys.len();
    auth.api_keys.retain(|key| key.id != id);
    if auth.api_keys.len() == count {
        return api_error(StatusCode::NOT_FOUND, "API key not found");
    }
    persist_auth_or_error(&auth).await
}

async fn persist_auth_or_error(auth: &auth::AuthConfig) -> Response {
    if let Err(err) = save_auth(auth).await {
        error!(%err, "failed to persist authentication configuration");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save authentication configuration",
        );
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn list_providers(State(state): State<AppState>) -> impl IntoResponse {
    let providers = state.providers.read().await;
    let mut providers: Vec<_> = providers.values().map(provider_view).collect();
    providers.sort_by(|a, b| a.name.cmp(&b.name));
    axum::Json(providers)
}

fn provider_view(provider: &Provider) -> ProviderView {
    ProviderView {
        id: provider.id.clone(),
        name: provider.name.clone(),
        endpoints: provider
            .endpoints
            .iter()
            .map(|endpoint| EndpointView {
                id: endpoint.id.clone(),
                api_type: endpoint.api_type,
                base_url: endpoint.base_url.clone(),
                requires_api_key: endpoint.requires_api_key,
                api_keys: endpoint
                    .api_keys
                    .iter()
                    .map(|key| ApiKeyView {
                        id: key.id.clone(),
                        name: key.name.clone(),
                        weight: key.weight,
                        enabled: key.enabled,
                    })
                    .collect(),
            })
            .collect(),
        discovered_models: provider.discovered_models.clone(),
        models_discovered_at: provider.models_discovered_at,
        model_discovery_error: provider.model_discovery_error.clone(),
        model_routes: provider.model_routes.clone(),
    }
}

async fn create_provider(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<CreateProvider>,
) -> Response {
    if input.id.trim().is_empty()
        || input.name.trim().is_empty()
        || input.endpoint.base_url.trim().is_empty()
    {
        return api_error(StatusCode::BAD_REQUEST, "Provider fields are required");
    }
    if !valid_id(&input.id) {
        return api_error(StatusCode::BAD_REQUEST, "Provider ID must be a URL slug");
    }
    if input.endpoint.requires_api_key
        && input.endpoint.api_key.as_deref().is_none_or(str::is_empty)
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "API key is required for this endpoint",
        );
    }

    let endpoint_id = input
        .endpoint
        .id
        .unwrap_or_else(|| input.endpoint.api_type.default_endpoint_id().to_owned());
    if !valid_id(&endpoint_id) {
        return api_error(StatusCode::BAD_REQUEST, "Endpoint ID must be a URL slug");
    }
    let api_keys = input
        .endpoint
        .api_key
        .filter(|secret| !secret.is_empty())
        .map(|secret| vec![new_api_key("default", "Default", secret, 100)])
        .unwrap_or_default();
    let provider = Provider {
        id: input.id.trim().to_owned(),
        name: input.name.trim().to_owned(),
        endpoints: vec![ApiEndpoint {
            id: endpoint_id,
            api_type: input.endpoint.api_type,
            base_url: input
                .endpoint
                .base_url
                .trim()
                .trim_end_matches('/')
                .to_owned(),
            requires_api_key: input.endpoint.requires_api_key,
            api_keys,
            ..ApiEndpoint::default()
        }],
        discovered_models: Vec::new(),
        models_discovered_at: None,
        model_discovery_error: None,
        model_routes: Vec::new(),
    };

    let provider_id = provider.id.clone();
    let mut providers = state.providers.write().await;
    if providers.contains_key(&provider.id) {
        return api_error(StatusCode::CONFLICT, "Provider ID already exists");
    }
    providers.insert(provider.id.clone(), provider);
    let response = persist_or_error(&providers).await;
    drop(providers);
    if response.status().is_success() {
        let refresh_state = state.clone();
        tokio::spawn(async move {
            let _ = models::refresh_provider(State(refresh_state), Path(provider_id)).await;
        });
    }
    response
}

async fn delete_provider(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let mut providers = state.providers.write().await;
    if providers.remove(&id).is_none() {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    }
    persist_or_error(&providers).await
}

async fn create_endpoint(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
    axum::Json(input): axum::Json<CreateEndpoint>,
) -> Response {
    if input.base_url.trim().is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "Endpoint base URL is required");
    }
    let endpoint_id = input
        .id
        .unwrap_or_else(|| input.api_type.default_endpoint_id().to_owned());
    if !valid_id(&endpoint_id) {
        return api_error(StatusCode::BAD_REQUEST, "Endpoint ID must be a URL slug");
    }
    if input.requires_api_key && input.api_key.as_deref().is_none_or(str::is_empty) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "API key is required for this endpoint",
        );
    }
    let mut providers = state.providers.write().await;
    let Some(provider) = providers.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    if provider
        .endpoints
        .iter()
        .any(|endpoint| endpoint.id == endpoint_id)
    {
        return api_error(StatusCode::CONFLICT, "Endpoint ID already exists");
    }
    let api_keys = input
        .api_key
        .filter(|secret| !secret.is_empty())
        .map(|secret| vec![new_api_key("default", "Default", secret, 100)])
        .unwrap_or_default();
    provider.endpoints.push(ApiEndpoint {
        id: endpoint_id,
        api_type: input.api_type,
        base_url: input.base_url.trim().trim_end_matches('/').to_owned(),
        requires_api_key: input.requires_api_key,
        api_keys,
        ..ApiEndpoint::default()
    });
    persist_or_error(&providers).await
}

async fn create_api_key(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
    axum::Json(input): axum::Json<CreateApiKey>,
) -> Response {
    if input.name.trim().is_empty() || input.secret.is_empty() || input.weight == 0 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Key name, secret and positive weight are required",
        );
    }
    let mut providers = state.providers.write().await;
    let Some(provider) = providers.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let Some(endpoint) = provider
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.id == input.endpoint_id)
    else {
        return api_error(StatusCode::NOT_FOUND, "API endpoint not found");
    };
    let id = unique_key_id(endpoint, &slugify(&input.name));
    endpoint.api_keys.push(new_api_key(
        &id,
        input.name.trim(),
        input.secret,
        input.weight,
    ));
    persist_or_error(&providers).await
}

async fn update_api_key(
    State(state): State<AppState>,
    Path((provider_id, key_id)): Path<(String, String)>,
    axum::Json(input): axum::Json<UpdateApiKey>,
) -> Response {
    if input.weight == Some(0) {
        return api_error(StatusCode::BAD_REQUEST, "API key weight must be positive");
    }
    let mut providers = state.providers.write().await;
    let Some(provider) = providers.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let Some(key) = provider
        .endpoints
        .iter_mut()
        .flat_map(|endpoint| &mut endpoint.api_keys)
        .find(|key| key.id == key_id)
    else {
        return api_error(StatusCode::NOT_FOUND, "API key not found");
    };
    if let Some(weight) = input.weight {
        key.weight = weight;
    }
    if let Some(enabled) = input.enabled {
        key.enabled = enabled;
    }
    persist_or_error(&providers).await
}

async fn delete_api_key(
    State(state): State<AppState>,
    Path((provider_id, key_id)): Path<(String, String)>,
) -> Response {
    let mut providers = state.providers.write().await;
    let Some(provider) = providers.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let original_count: usize = provider
        .endpoints
        .iter()
        .map(|endpoint| endpoint.api_keys.len())
        .sum();
    for endpoint in &mut provider.endpoints {
        endpoint.api_keys.retain(|key| key.id != key_id);
    }
    let current_count: usize = provider
        .endpoints
        .iter()
        .map(|endpoint| endpoint.api_keys.len())
        .sum();
    if current_count == original_count {
        return api_error(StatusCode::NOT_FOUND, "API key not found");
    }
    provider
        .model_routes
        .retain(|route| route.api_key_id != key_id);
    persist_or_error(&providers).await
}

async fn create_model_route(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
    axum::Json(input): axum::Json<CreateModelRoute>,
) -> Response {
    let pattern = input.pattern.trim();
    if !valid_model_pattern(pattern) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Model pattern must be an exact name or end with one '*'",
        );
    }
    let mut providers = state.providers.write().await;
    let Some(provider) = providers.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let Some((_, key)) = provider.endpoint_and_key(&input.endpoint_id, &input.api_key_id) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "API key does not belong to this endpoint",
        );
    };
    if !key.enabled {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Disabled API key cannot be assigned",
        );
    }
    if let Some(route) = provider
        .model_routes
        .iter_mut()
        .find(|route| route.pattern == pattern)
    {
        route.endpoint_id = input.endpoint_id;
        route.api_key_id = input.api_key_id;
    } else {
        provider.model_routes.push(ModelRoute {
            pattern: pattern.to_owned(),
            endpoint_id: input.endpoint_id,
            api_key_id: input.api_key_id,
        });
    }
    persist_or_error(&providers).await
}

async fn delete_model_route(
    State(state): State<AppState>,
    Path((provider_id, pattern)): Path<(String, String)>,
) -> Response {
    let mut providers = state.providers.write().await;
    let Some(provider) = providers.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let count = provider.model_routes.len();
    provider
        .model_routes
        .retain(|route| route.pattern != pattern);
    if provider.model_routes.len() == count {
        return api_error(StatusCode::NOT_FOUND, "Model route not found");
    }
    persist_or_error(&providers).await
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        })
}

fn slugify(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn new_api_key(id: &str, name: &str, secret: String, weight: u32) -> ApiKey {
    ApiKey {
        id: id.to_owned(),
        name: name.to_owned(),
        secret,
        weight,
        enabled: true,
    }
}

fn unique_key_id(endpoint: &ApiEndpoint, base: &str) -> String {
    if !endpoint.api_keys.iter().any(|key| key.id == base) {
        return base.to_owned();
    }
    (2..)
        .map(|suffix| format!("{base}-{suffix}"))
        .find(|candidate| !endpoint.api_keys.iter().any(|key| key.id == *candidate))
        .expect("finite key ID space")
}

fn valid_model_pattern(pattern: &str) -> bool {
    !pattern.is_empty()
        && (pattern.matches('*').count() == 0
            || (pattern.ends_with('*') && pattern.matches('*').count() == 1 && pattern.len() > 1))
}

async fn persist_or_error(providers: &std::collections::HashMap<String, Provider>) -> Response {
    if let Err(err) = save_providers(providers).await {
        error!(%err, "failed to persist providers");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save providers",
        );
    }
    StatusCode::NO_CONTENT.into_response()
}

fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        axum::Json(ApiError {
            error: ApiErrorBody {
                message: message.into(),
                kind: "yabane_error",
            },
        }),
    )
        .into_response()
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl-C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install terminate handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

/*
#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use tokio::sync::RwLock;

    use axum::http::StatusCode;

    use super::{AppState, Provider, ProviderKind, join_upstream_url, resolve_provider};

    fn state(provider: Provider) -> AppState {
        AppState {
            client: reqwest::Client::new(),
            providers: Arc::new(RwLock::new(HashMap::from([(
                provider.id.clone(),
                provider,
            )]))),
        }
    }

    #[test]
    fn joins_base_url_without_duplicating_v1() {
        assert_eq!(
            join_upstream_url("https://example.com/v1", "/v1/chat/completions"),
            "https://example.com/v1/chat/completions"
        );
    }

    #[test]
    fn preserves_base_path_and_query() {
        assert_eq!(
            join_upstream_url("https://example.com/openai/v1/", "/v1/responses?trace=true"),
            "https://example.com/openai/v1/responses?trace=true"
        );
    }

    #[test]
    fn appends_versioned_path_to_origin() {
        assert_eq!(
            join_upstream_url("https://api.anthropic.com", "/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[tokio::test]
    async fn routes_known_provider_and_preserves_model_namespace() {
        let state = state(Provider {
            id: "chutes".to_owned(),
            name: "Chutes".to_owned(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: "https://example.com/v1".to_owned(),
            api_key: "secret".to_owned(),
            models: Vec::new(),
        });
        let (_, body) = resolve_provider(
            &state,
            br#"{"model":"chutes/Qwen/Qwen3.8-27B-TEE","reasoning_effort":"xhigh"}"#,
            &ProviderKind::OpenaiCompatible,
        )
        .await
        .expect("route provider");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("parse body");

        assert_eq!(payload["model"], "Qwen/Qwen3.8-27B-TEE");
        assert_eq!(payload["reasoning_effort"], "xhigh");
    }

    #[tokio::test]
    async fn rejects_model_without_provider_prefix() {
        let state = state(Provider {
            id: "chutes".to_owned(),
            name: "Chutes".to_owned(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: "https://example.com/v1".to_owned(),
            api_key: "secret".to_owned(),
            models: Vec::new(),
        });
        let error = resolve_provider(
            &state,
            br#"{"model":"Qwen/Qwen3.8-27B-TEE"}"#,
            &ProviderKind::OpenaiCompatible,
        )
        .await
        .expect_err("reject unknown provider prefix");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.message, "Unknown provider 'Qwen' in model");
    }
}
*/

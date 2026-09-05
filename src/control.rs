use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::{
    admin_user,
    auth::{self, GatewayApiKey, GatewayApiKeyView, generate_secret, hash_secret, now, save_auth},
    config::{
        ApiEndpoint, ApiKey, ApiType, AppState, ModelEndpointPreference, Provider, save_providers,
    },
    error::api_error,
    models, routes,
};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
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
        .route(
            "/admin/routes",
            get(list_global_routes).post(create_global_route),
        )
        .route("/admin/routes/{pattern}", delete(delete_global_route))
        .route("/admin/activity/logs", get(activity_logs))
        .route("/admin/activity/stats", get(activity_stats))
        .route("/admin/activity/export", get(export_activity))
        .route(
            "/admin/activity/import",
            post(import_activity).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route(
            "/admin/providers/{id}",
            patch(update_provider_options).delete(delete_provider),
        )
        .route(
            "/admin/providers/{id}/model-endpoint-preferences",
            patch(update_model_endpoint_preferences),
        )
        .route(
            "/admin/providers/{id}/models/refresh",
            post(models::refresh_provider),
        )
        .route("/admin/providers/{id}/endpoints", post(create_endpoint))
        .route(
            "/admin/providers/{provider_id}/endpoints/{endpoint_id}",
            patch(update_endpoint).delete(delete_endpoint),
        )
        .route(
            "/admin/providers/{provider_id}/endpoints/{endpoint_id}/traffic",
            patch(update_endpoint_traffic),
        )
        .route("/admin/providers/{id}/keys", post(create_api_key))
        .route(
            "/admin/providers/{provider_id}/endpoints/{endpoint_id}/keys/{key_id}",
            patch(update_api_key).delete(delete_api_key),
        )
        .route_layer(middleware::from_fn_with_state(
            state,
            admin_user::require_admin,
        ))
}

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
    socks5_proxy: Option<String>,
    #[serde(default)]
    extra_headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    extra_body: serde_json::Map<String, serde_json::Value>,
    requires_api_key: bool,
    api_key: Option<String>,
}

#[derive(Deserialize)]
struct UpdateEndpoint {
    id: Option<String>,
    api_type: ApiType,
    base_url: String,
    socks5_proxy: Option<String>,
    requires_api_key: bool,
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
struct UpdateEndpointTraffic {
    weights: Vec<ApiKeyWeight>,
}

#[derive(Deserialize)]
struct ApiKeyWeight {
    key_id: String,
    weight: u32,
}

#[derive(Deserialize)]
struct CreateGlobalRoute {
    pattern: String,
    targets: Vec<routes::RouteTarget>,
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
    extra_headers: std::collections::HashMap<String, String>,
    extra_body: serde_json::Map<String, serde_json::Value>,
    defaults_endpoint_ids: Vec<String>,
    endpoints: Vec<EndpointView>,
    discovered_models: Vec<String>,
    model_endpoints: std::collections::HashMap<String, Vec<String>>,
    model_endpoint_preferences: Vec<ModelEndpointPreference>,
    models_discovered_at: Option<u64>,
    model_discovery_error: Option<String>,
}

#[derive(Serialize)]
struct EndpointView {
    id: String,
    api_type: ApiType,
    base_url: String,
    socks5_proxy: Option<String>,
    extra_headers: std::collections::HashMap<String, String>,
    extra_body: serde_json::Map<String, serde_json::Value>,
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
    let mut updated = auth.clone();
    updated.enabled = input.enabled;
    let response = persist_auth_or_error(&updated).await;
    if response.status().is_success() {
        *auth = updated;
    }
    response
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
        secret: secret.clone(),
        prefix: format!("{}…{}", &secret[..10], &secret[secret.len() - 4..]),
        created_at: now(),
        expires_at: input.expires_at,
        provider_ids: input.provider_ids,
    };
    let view = GatewayApiKeyView::from(&key);
    let mut auth = state.auth.write().await;
    let mut updated = auth.clone();
    updated.api_keys.push(key);
    if let Err(err) = save_auth(&updated).await {
        error!(%err, "failed to persist authentication configuration");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save authentication configuration",
        );
    }
    *auth = updated;
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
    let mut updated = auth.clone();
    let count = updated.api_keys.len();
    updated.api_keys.retain(|key| key.id != id);
    if updated.api_keys.len() == count {
        return api_error(StatusCode::NOT_FOUND, "API key not found");
    }
    let response = persist_auth_or_error(&updated).await;
    if response.status().is_success() {
        *auth = updated;
    }
    response
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

async fn list_global_routes(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(state.routes.0.read().await.clone())
}

async fn create_global_route(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<CreateGlobalRoute>,
) -> Response {
    if !valid_model_pattern(input.pattern.trim())
        || input.targets.is_empty()
        || input
            .targets
            .iter()
            .any(|target| target.weight == 0 || target.upstream_model.trim().is_empty())
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "A valid pattern and positive-weight route targets are required",
        );
    }
    let providers = state.providers.read().await;
    for target in &input.targets {
        let Some(provider) = providers.get(&target.provider_id) else {
            return api_error(
                StatusCode::BAD_REQUEST,
                format!("Unknown provider '{}'", target.provider_id),
            );
        };
        let Some((_, key)) = provider.endpoint_and_key(&target.endpoint_id, &target.api_key_id)
        else {
            return api_error(
                StatusCode::BAD_REQUEST,
                "Route target API key was not found",
            );
        };
        let upstream_model = target.upstream_model.trim();
        if upstream_model
            .strip_prefix(&format!("{}/", target.provider_id))
            .is_some_and(|model| {
                !model.is_empty()
                    && (provider
                        .discovered_models
                        .iter()
                        .any(|known| known == model)
                        || !provider
                            .discovered_models
                            .iter()
                            .any(|known| known == upstream_model))
            })
        {
            return api_error(
                StatusCode::BAD_REQUEST,
                format!(
                    "Upstream model ID must not include Yabane provider prefix '{}/'",
                    target.provider_id
                ),
            );
        }
        if !key.enabled {
            return api_error(StatusCode::BAD_REQUEST, "Route target API key is disabled");
        }
    }
    drop(providers);
    let mut routes = state.routes.0.write().await;
    let mut updated = routes.clone();
    if let Some(route) = updated
        .iter_mut()
        .find(|route| route.pattern == input.pattern)
    {
        route.targets = input.targets;
    } else {
        updated.push(routes::ModelRoute {
            pattern: input.pattern.trim().to_owned(),
            targets: input.targets,
            cursor: Default::default(),
        });
    }
    let response = persist_routes_or_error(&updated).await;
    if response.status().is_success() {
        *routes = updated;
    }
    response
}

async fn delete_global_route(
    State(state): State<AppState>,
    Path(pattern): Path<String>,
) -> Response {
    let mut routes = state.routes.0.write().await;
    let mut updated = routes.clone();
    let count = updated.len();
    updated.retain(|route| route.pattern != pattern);
    if updated.len() == count {
        return api_error(StatusCode::NOT_FOUND, "Model route not found");
    }
    let response = persist_routes_or_error(&updated).await;
    if response.status().is_success() {
        *routes = updated;
    }
    response
}

async fn persist_routes_or_error(routes: &[routes::ModelRoute]) -> Response {
    match routes::RouteStore::save_value(routes).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            error!(%err, "failed to persist routes");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not save model routes",
            )
        }
    }
}

#[derive(Deserialize)]
struct ActivityQuery {
    since: Option<u64>,
    limit: Option<usize>,
}
async fn activity_logs(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> impl IntoResponse {
    axum::Json(
        state
            .activity
            .logs(query.since.unwrap_or(0), query.limit.unwrap_or(100))
            .await,
    )
}
async fn activity_stats(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> impl IntoResponse {
    axum::Json(state.activity.stats(query.since.unwrap_or(0)).await)
}

async fn export_activity(State(state): State<AppState>) -> Response {
    let records = state.activity.export_records().await;
    let export = crate::activity::ActivityExport {
        format: "yabane-activity",
        version: 1,
        instance_id: state.activity.instance_id(),
        exported_at: now(),
        records: &records,
    };
    let body = serde_json::to_vec(&export).expect("serialize activity export");
    (
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"yabane-activity.json\"",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

async fn import_activity(State(state): State<AppState>, body: Bytes) -> Response {
    const MAX_IMPORT_SIZE: usize = 64 * 1024 * 1024;
    if body.len() > MAX_IMPORT_SIZE {
        return api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Activity import is too large",
        );
    }
    let import: crate::activity::ActivityImport = match serde_json::from_slice(&body) {
        Ok(import) => import,
        Err(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "Activity import must be a valid Yabane activity JSON file",
            );
        }
    };
    match state.activity.import(import).await {
        Ok(result) => axum::Json(result).into_response(),
        Err(message) => api_error(StatusCode::BAD_REQUEST, message),
    }
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
        extra_headers: provider.extra_headers.clone(),
        extra_body: provider.extra_body.clone(),
        defaults_endpoint_ids: provider.defaults_endpoint_ids.clone(),
        endpoints: provider
            .endpoints
            .iter()
            .map(|endpoint| EndpointView {
                id: endpoint.id.clone(),
                api_type: endpoint.api_type,
                base_url: endpoint.base_url.clone(),
                socks5_proxy: endpoint.socks5_proxy.clone(),
                extra_headers: endpoint.extra_headers.clone(),
                extra_body: endpoint.extra_body.clone(),
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
        model_endpoints: provider.model_endpoints.clone(),
        model_endpoint_preferences: provider.model_endpoint_preferences.clone(),
        models_discovered_at: provider.models_discovered_at,
        model_discovery_error: provider.model_discovery_error.clone(),
    }
}

#[derive(Deserialize)]
struct UpdateModelEndpointPreferences {
    preferences: Vec<ModelEndpointPreference>,
}

#[derive(Deserialize)]
struct ProviderOptions {
    #[serde(default)]
    extra_headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    extra_body: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    defaults_endpoint_ids: Vec<String>,
}

async fn update_model_endpoint_preferences(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::Json(input): axum::Json<UpdateModelEndpointPreferences>,
) -> Response {
    let mut providers = state.providers.write().await;
    let mut updated = providers.clone();
    let Some(provider) = updated.get_mut(&id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let mut unique = std::collections::HashSet::new();
    for preference in &input.preferences {
        if !unique.insert((preference.model.as_str(), preference.api_type)) {
            return api_error(
                StatusCode::BAD_REQUEST,
                "Each model and API type can have only one preferred endpoint",
            );
        }
        let available = provider
            .model_endpoints
            .get(&preference.model)
            .is_some_and(|endpoint_ids| endpoint_ids.contains(&preference.endpoint_id));
        let compatible = provider.endpoints.iter().any(|endpoint| {
            endpoint.id == preference.endpoint_id && endpoint.api_type == preference.api_type
        });
        if !available || !compatible {
            return api_error(
                StatusCode::BAD_REQUEST,
                format!(
                    "Endpoint '{}' does not expose model '{}' through the selected API type",
                    preference.endpoint_id, preference.model
                ),
            );
        }
    }
    provider.model_endpoint_preferences = input.preferences;
    let response = persist_or_error(&updated).await;
    if response.status().is_success() {
        *providers = updated;
    }
    response
}

async fn update_provider_options(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::Json(input): axum::Json<ProviderOptions>,
) -> Response {
    if let Err(message) = validate_extra_headers(&input.extra_headers) {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    let mut providers = state.providers.write().await;
    let mut updated = providers.clone();
    let Some(provider) = updated.get_mut(&id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let mut endpoint_ids = input.defaults_endpoint_ids;
    endpoint_ids.sort();
    endpoint_ids.dedup();
    if endpoint_ids
        .iter()
        .any(|id| !provider.endpoints.iter().any(|endpoint| &endpoint.id == id))
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Request defaults contain an unknown endpoint",
        );
    }
    if endpoint_ids.len() == provider.endpoints.len() {
        endpoint_ids.clear();
    }
    provider.extra_headers = input.extra_headers;
    provider.extra_body = input.extra_body;
    provider.defaults_endpoint_ids = endpoint_ids;
    let response = persist_or_error(&updated).await;
    if response.status().is_success() {
        *providers = updated;
    }
    response
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
    if let Err(message) = validate_socks5_proxy(input.endpoint.socks5_proxy.as_deref()) {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    if let Err(message) = validate_extra_headers(&input.endpoint.extra_headers) {
        return api_error(StatusCode::BAD_REQUEST, message);
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
        extra_headers: std::collections::HashMap::new(),
        extra_body: serde_json::Map::new(),
        defaults_endpoint_ids: Vec::new(),
        endpoints: vec![ApiEndpoint {
            id: endpoint_id,
            api_type: input.endpoint.api_type,
            base_url: input
                .endpoint
                .base_url
                .trim()
                .trim_end_matches('/')
                .to_owned(),
            socks5_proxy: normalized_socks5_proxy(input.endpoint.socks5_proxy.as_deref()),
            extra_headers: input.endpoint.extra_headers,
            extra_body: input.endpoint.extra_body,
            requires_api_key: input.endpoint.requires_api_key,
            api_keys,
            ..ApiEndpoint::default()
        }],
        discovered_models: Vec::new(),
        model_endpoints: std::collections::HashMap::new(),
        model_endpoint_preferences: Vec::new(),
        models_discovered_at: None,
        model_discovery_error: None,
    };

    let provider_id = provider.id.clone();
    let mut providers = state.providers.write().await;
    if providers.contains_key(&provider.id) {
        return api_error(StatusCode::CONFLICT, "Provider ID already exists");
    }
    let mut updated = providers.clone();
    updated.insert(provider.id.clone(), provider);
    let response = persist_or_error(&updated).await;
    if response.status().is_success() {
        *providers = updated;
    }
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
    let mut updated_providers = providers.clone();
    if updated_providers.remove(&id).is_none() {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    }

    let mut routes = state.routes.0.write().await;
    let mut updated_routes = routes.clone();
    for route in &mut updated_routes {
        route.targets.retain(|target| target.provider_id != id);
    }
    updated_routes.retain(|route| !route.targets.is_empty());
    if let Err(err) =
        save_provider_and_routes(&updated_providers, &updated_routes, &providers).await
    {
        error!(%err, "failed to persist provider deletion");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not delete Provider and its model routes",
        );
    }
    *providers = updated_providers;
    *routes = updated_routes;
    StatusCode::NO_CONTENT.into_response()
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
    if let Err(message) = validate_socks5_proxy(input.socks5_proxy.as_deref()) {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    if let Err(message) = validate_extra_headers(&input.extra_headers) {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    if input.requires_api_key && input.api_key.as_deref().is_none_or(str::is_empty) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "API key is required for this endpoint",
        );
    }
    let mut providers = state.providers.write().await;
    let mut updated = providers.clone();
    let Some(provider) = updated.get_mut(&provider_id) else {
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
        socks5_proxy: normalized_socks5_proxy(input.socks5_proxy.as_deref()),
        extra_headers: input.extra_headers,
        extra_body: input.extra_body,
        requires_api_key: input.requires_api_key,
        api_keys,
        ..ApiEndpoint::default()
    });
    let response = persist_or_error(&updated).await;
    if response.status().is_success() {
        *providers = updated;
    }
    drop(providers);
    if response.status().is_success() {
        spawn_provider_refresh(state, provider_id);
    }
    response
}

async fn update_endpoint(
    State(state): State<AppState>,
    Path((provider_id, endpoint_id)): Path<(String, String)>,
    axum::Json(input): axum::Json<UpdateEndpoint>,
) -> Response {
    if input.base_url.trim().is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "Endpoint base URL is required");
    }
    if input.id.as_deref().is_some_and(|id| id != endpoint_id) {
        return api_error(StatusCode::BAD_REQUEST, "Endpoint ID cannot be changed");
    }
    if let Err(message) = validate_socks5_proxy(input.socks5_proxy.as_deref()) {
        return api_error(StatusCode::BAD_REQUEST, message);
    }

    let mut providers = state.providers.write().await;
    let mut updated = providers.clone();
    let Some(provider) = updated.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let Some(endpoint) = provider
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.id == endpoint_id)
    else {
        return api_error(StatusCode::NOT_FOUND, "API endpoint not found");
    };
    endpoint.api_type = input.api_type;
    endpoint.base_url = input.base_url.trim().trim_end_matches('/').to_owned();
    endpoint.socks5_proxy = normalized_socks5_proxy(input.socks5_proxy.as_deref());
    endpoint.requires_api_key = input.requires_api_key;
    endpoint.proxy_client = Default::default();
    remove_endpoint_discovery(provider, &endpoint_id);
    let response = persist_or_error(&updated).await;
    if response.status().is_success() {
        *providers = updated;
    }
    drop(providers);
    if response.status().is_success() {
        spawn_provider_refresh(state, provider_id);
    }
    response
}

async fn delete_endpoint(
    State(state): State<AppState>,
    Path((provider_id, endpoint_id)): Path<(String, String)>,
) -> Response {
    let mut providers = state.providers.write().await;
    let mut updated_providers = providers.clone();
    let Some(provider) = updated_providers.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let original_count = provider.endpoints.len();
    provider
        .endpoints
        .retain(|endpoint| endpoint.id != endpoint_id);
    if provider.endpoints.len() == original_count {
        return api_error(StatusCode::NOT_FOUND, "API endpoint not found");
    }

    remove_endpoint_discovery(provider, &endpoint_id);
    if !provider.defaults_endpoint_ids.is_empty() {
        provider
            .defaults_endpoint_ids
            .retain(|id| id != &endpoint_id);
        if provider.defaults_endpoint_ids.is_empty() {
            provider.extra_headers.clear();
            provider.extra_body.clear();
        }
    }

    let mut routes = state.routes.0.write().await;
    let mut updated_routes = routes.clone();
    for route in &mut updated_routes {
        route.targets.retain(|target| {
            target.provider_id != provider_id || target.endpoint_id != endpoint_id
        });
    }
    updated_routes.retain(|route| !route.targets.is_empty());
    if let Err(err) =
        save_provider_and_routes(&updated_providers, &updated_routes, &providers).await
    {
        error!(%err, "failed to persist endpoint deletion");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not delete API endpoint",
        );
    }
    *providers = updated_providers;
    *routes = updated_routes;
    StatusCode::NO_CONTENT.into_response()
}

async fn update_endpoint_traffic(
    State(state): State<AppState>,
    Path((provider_id, endpoint_id)): Path<(String, String)>,
    axum::Json(input): axum::Json<UpdateEndpointTraffic>,
) -> Response {
    if input.weights.is_empty()
        || input.weights.iter().any(|item| item.weight == 0)
        || input
            .weights
            .iter()
            .map(|item| u64::from(item.weight))
            .sum::<u64>()
            != 100
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Enabled API key traffic percentages must total 100",
        );
    }
    let mut seen = std::collections::HashSet::new();
    if input
        .weights
        .iter()
        .any(|item| !seen.insert(item.key_id.as_str()))
    {
        return api_error(StatusCode::BAD_REQUEST, "API key IDs must be unique");
    }

    let mut providers = state.providers.write().await;
    let mut updated = providers.clone();
    let Some(provider) = updated.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let Some(endpoint) = provider
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.id == endpoint_id)
    else {
        return api_error(StatusCode::NOT_FOUND, "API endpoint not found");
    };
    let enabled_key_ids: std::collections::HashSet<_> = endpoint
        .api_keys
        .iter()
        .filter(|key| key.enabled)
        .map(|key| key.id.as_str())
        .collect();
    if seen != enabled_key_ids {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Traffic distribution must include every enabled API key exactly once",
        );
    }
    for item in &input.weights {
        endpoint
            .api_keys
            .iter_mut()
            .find(|key| key.id == item.key_id)
            .expect("validated API key")
            .weight = item.weight;
    }
    let response = persist_or_error(&updated).await;
    if response.status().is_success() {
        *providers = updated;
    }
    response
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
    let mut updated = providers.clone();
    let Some(provider) = updated.get_mut(&provider_id) else {
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
    let response = persist_or_error(&updated).await;
    if response.status().is_success() {
        *providers = updated;
    }
    response
}

async fn update_api_key(
    State(state): State<AppState>,
    Path((provider_id, endpoint_id, key_id)): Path<(String, String, String)>,
    axum::Json(input): axum::Json<UpdateApiKey>,
) -> Response {
    if input.weight == Some(0) {
        return api_error(StatusCode::BAD_REQUEST, "API key weight must be positive");
    }
    let mut providers = state.providers.write().await;
    let mut updated = providers.clone();
    let Some(provider) = updated.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let Some(endpoint) = provider
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.id == endpoint_id)
    else {
        return api_error(StatusCode::NOT_FOUND, "API endpoint not found");
    };
    let Some(key) = endpoint.api_keys.iter_mut().find(|key| key.id == key_id) else {
        return api_error(StatusCode::NOT_FOUND, "API key not found");
    };
    if let Some(weight) = input.weight {
        key.weight = weight;
    }
    if let Some(enabled) = input.enabled {
        key.enabled = enabled;
    }
    let response = persist_or_error(&updated).await;
    if response.status().is_success() {
        *providers = updated;
    }
    response
}

async fn delete_api_key(
    State(state): State<AppState>,
    Path((provider_id, endpoint_id, key_id)): Path<(String, String, String)>,
) -> Response {
    let mut providers = state.providers.write().await;
    let mut updated_providers = providers.clone();
    let Some(provider) = updated_providers.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let Some(endpoint) = provider
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.id == endpoint_id)
    else {
        return api_error(StatusCode::NOT_FOUND, "API endpoint not found");
    };
    let original_count = endpoint.api_keys.len();
    endpoint.api_keys.retain(|key| key.id != key_id);
    if endpoint.api_keys.len() == original_count {
        return api_error(StatusCode::NOT_FOUND, "API key not found");
    }
    let mut routes = state.routes.0.write().await;
    let mut updated = routes.clone();
    for route in &mut updated {
        route.targets.retain(|target| {
            target.provider_id != provider_id
                || target.endpoint_id != endpoint_id
                || target.api_key_id != key_id
        });
    }
    updated.retain(|route| !route.targets.is_empty());
    if let Err(err) = save_provider_and_routes(&updated_providers, &updated, &providers).await {
        error!(%err, "failed to persist provider and route mutation");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save provider and model routes",
        );
    }
    *providers = updated_providers;
    *routes = updated;
    StatusCode::NO_CONTENT.into_response()
}

fn remove_endpoint_discovery(provider: &mut Provider, endpoint_id: &str) {
    provider.model_endpoints.retain(|_, endpoint_ids| {
        endpoint_ids.retain(|id| id != endpoint_id);
        !endpoint_ids.is_empty()
    });
    provider
        .model_endpoint_preferences
        .retain(|preference| preference.endpoint_id != endpoint_id);
    provider
        .discovered_models
        .retain(|model| provider.model_endpoints.contains_key(model));
}

fn spawn_provider_refresh(state: AppState, provider_id: String) {
    tokio::spawn(async move {
        let _ = models::refresh_provider(State(state), Path(provider_id)).await;
    });
}

fn validate_extra_headers(
    headers: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let mut normalized_names = std::collections::HashSet::new();
    for (name, value) in headers {
        if !normalized_names.insert(name.to_ascii_lowercase()) {
            return Err(format!("Duplicate extra header name '{name}'"));
        }
        let header_name = axum::http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("Invalid extra header name '{name}'"))?;
        axum::http::HeaderValue::from_str(value)
            .map_err(|_| format!("Invalid value for extra header '{name}'"))?;
        if matches!(
            header_name.as_str(),
            "host"
                | "authorization"
                | "x-api-key"
                | "content-length"
                | "connection"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        ) {
            return Err(format!(
                "Extra header '{name}' is managed by Yabane and cannot be overridden"
            ));
        }
    }
    Ok(())
}

fn validate_socks5_proxy(value: Option<&str>) -> Result<(), &'static str> {
    if value.map(str::trim).is_some_and(|value| {
        !value.is_empty() && !value.starts_with("socks5://") && !value.starts_with("socks5h://")
    }) {
        return Err("SOCKS5 proxy URL must start with socks5:// or socks5h://");
    }
    Ok(())
}

fn normalized_socks5_proxy(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
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

async fn save_provider_and_routes(
    providers: &std::collections::HashMap<String, Provider>,
    routes: &[routes::ModelRoute],
    previous_providers: &std::collections::HashMap<String, Provider>,
) -> Result<(), String> {
    save_providers(providers)
        .await
        .map_err(|err| format!("save providers: {err}"))?;
    if let Err(err) = routes::RouteStore::save_value(routes).await {
        if let Err(rollback_err) = save_providers(previous_providers).await {
            return Err(format!(
                "save routes: {err}; provider rollback also failed: {rollback_err}"
            ));
        }
        return Err(format!("save routes: {err}"));
    }
    Ok(())
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

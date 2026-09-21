use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, patch, post},
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
    models, openai_subscription, routes,
    storage::{AtomicWrite, CONFIG_TRANSACTION_FILE, write_transaction},
};

pub fn router(state: AppState) -> Router<AppState> {
    let router = Router::new()
        .route(
            "/admin/auth",
            get(get_auth_settings).patch(update_auth_settings),
        )
        .route("/admin/auth/keys", post(create_gateway_api_key))
        .route(
            "/admin/auth/keys/{id}",
            patch(update_gateway_api_key).delete(delete_gateway_api_key),
        )
        .route(
            "/admin/providers",
            get(list_providers).post(create_provider),
        )
        .route(
            "/admin/pricing",
            get(get_global_pricing).patch(update_global_pricing),
        )
        .route(
            "/admin/pricing/providers/{provider_id}",
            patch(update_provider_pricing),
        )
        .route(
            "/admin/pricing/providers/{provider_id}/endpoints/{endpoint_id}",
            patch(update_endpoint_pricing),
        )
        .route(
            "/admin/openai-subscriptions/device-code",
            post(start_openai_subscription),
        )
        .route(
            "/admin/openai-subscriptions/device-code/{id}",
            get(poll_openai_subscription),
        )
        .route(
            "/admin/openai-subscriptions/oauth",
            post(start_openai_subscription_oauth),
        )
        .route(
            "/admin/openai-subscriptions/oauth/{id}/complete",
            post(complete_openai_subscription_oauth),
        )
        .route("/admin/extensions", get(list_extensions))
        .route("/admin/extensions/{id}", patch(update_extension));
    #[cfg(feature = "extension-traffic-capture")]
    let router = router
        .route(
            "/admin/extensions/traffic-capture/status",
            get(traffic_capture_status).patch(configure_traffic_capture),
        )
        .route(
            "/admin/extensions/traffic-capture/stop",
            post(stop_traffic_capture),
        )
        .route(
            "/admin/extensions/traffic-capture/captures",
            get(list_traffic_captures).delete(delete_all_traffic_captures),
        )
        .route(
            "/admin/extensions/traffic-capture/captures/{request_id}",
            get(get_traffic_capture).delete(delete_traffic_capture),
        );
    router
        .route(
            "/admin/routes",
            get(list_global_routes).post(create_global_route),
        )
        .route(
            "/admin/routes/{pattern}",
            patch(update_global_route).delete(delete_global_route),
        )
        .route("/admin/activity/logs", get(activity_logs))
        .route("/admin/activity/logs/page", get(activity_log_page))
        .route("/admin/activity/stats", get(activity_stats))
        .route(
            "/admin/activity/recalculate-costs",
            post(recalculate_activity_costs),
        )
        .route("/admin/activity/export", get(export_activity))
        .route(
            "/admin/activity/export/preview",
            get(preview_activity_export),
        )
        .route(
            "/admin/activity/import",
            post(import_activity).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route(
            "/admin/activity/import/preview",
            post(preview_activity_import).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route(
            "/admin/activity/settings",
            get(activity_settings).patch(update_activity_settings),
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
    pricing: Option<crate::pricing::PricingTable>,
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
    pricing: Option<crate::pricing::PricingTable>,
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

#[derive(Deserialize)]
struct UpdateGatewayApiKey {
    note: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_expiry")]
    expires_at: Option<Option<u64>>,
    provider_ids: Option<Vec<String>>,
}

fn deserialize_optional_expiry<'de, D>(deserializer: D) -> Result<Option<Option<u64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<u64>::deserialize(deserializer).map(Some)
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
    pricing: Option<crate::pricing::PricingTable>,
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
    pricing: Option<crate::pricing::PricingTable>,
    requires_api_key: bool,
    api_keys: Vec<ApiKeyView>,
    subscription_connected: bool,
    subscription_expires_at: Option<u64>,
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
    // Keep the Provider read lock through the authentication commit so a
    // concurrently deleted Provider cannot leave a newly created dangling scope.
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

async fn update_gateway_api_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::Json(input): axum::Json<UpdateGatewayApiKey>,
) -> Response {
    if input
        .expires_at
        .flatten()
        .is_some_and(|expiry| expiry <= now())
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "API key expiry must be in the future",
        );
    }
    // Provider deletion takes the same locks in Provider → authentication
    // order. Retain this guard until persistence completes to make validation
    // and the scoped-key update one serialized operation.
    let providers = state.providers.read().await;
    if let Some(provider_ids) = &input.provider_ids
        && let Some(provider_id) = provider_ids
            .iter()
            .find(|provider_id| !providers.contains_key(*provider_id))
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            format!("Unknown provider '{provider_id}'"),
        );
    }

    let mut auth = state.auth.write().await;
    let mut updated = auth.clone();
    let Some(key) = updated.api_keys.iter_mut().find(|key| key.id == id) else {
        return api_error(StatusCode::NOT_FOUND, "API key not found");
    };
    if let Some(note) = input.note {
        key.note = note.trim().to_owned();
    }
    if let Some(expires_at) = input.expires_at {
        key.expires_at = expires_at;
    }
    if let Some(provider_ids) = input.provider_ids {
        key.provider_ids = provider_ids;
    }
    let response = persist_auth_or_error(&updated).await;
    if response.status().is_success() {
        *auth = updated;
    }
    response
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
    save_global_route(state, None, input).await
}

async fn update_global_route(
    State(state): State<AppState>,
    Path(original_pattern): Path<String>,
    axum::Json(input): axum::Json<CreateGlobalRoute>,
) -> Response {
    save_global_route(state, Some(original_pattern), input).await
}

async fn save_global_route(
    state: AppState,
    original_pattern: Option<String>,
    mut input: CreateGlobalRoute,
) -> Response {
    for target in &mut input.targets {
        if !target.enabled || target.weight == 0 {
            target.enabled = false;
            target.weight = 0;
        }
    }
    let traffic_total: u64 = input
        .targets
        .iter()
        .map(|target| u64::from(target.weight))
        .sum();
    if !routes::valid_model_pattern(input.pattern.trim())
        || input.targets.is_empty()
        || input
            .targets
            .iter()
            .any(|target| target.upstream_model.trim().is_empty() || target.weight > 100)
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "A valid pattern and traffic shares from 0 to 100 are required; 0 disables a target",
        );
    }
    if traffic_total != 100 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Enabled route target traffic percentages must total 100",
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
        let Some(endpoint) = provider
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == target.endpoint_id)
        else {
            return api_error(
                StatusCode::BAD_REQUEST,
                "Route target Endpoint was not found",
            );
        };
        let key = if endpoint.api_type == ApiType::OpenaiCodex {
            if state.extensions.provider_endpoint("openai_codex").is_none() {
                return api_error(
                    StatusCode::CONFLICT,
                    "Enable the OpenAI Subscription Extension before routing to this Endpoint",
                );
            }
            if !target.api_key_id.is_empty() {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "OpenAI subscription route targets must not specify an API key",
                );
            }
            if endpoint.openai_subscription.is_none() {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "Route target OpenAI subscription is not connected",
                );
            }
            None
        } else if !endpoint.requires_api_key {
            if !target.api_key_id.is_empty() {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "Route targets for Endpoints without authentication must not specify an API key",
                );
            }
            None
        } else {
            let Some(key) = endpoint
                .api_keys
                .iter()
                .find(|key| key.id == target.api_key_id)
            else {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "Route target API key was not found",
                );
            };
            Some(key)
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
        if key.is_some_and(|key| !key.enabled) {
            return api_error(StatusCode::BAD_REQUEST, "Route target API key is disabled");
        }
    }
    // Keep the Provider snapshot locked until the route is persisted. Provider
    // and Endpoint mutations use the same Provider → route lock order, so a
    // target cannot become dangling between validation and commit.
    let mut routes = state.routes.0.write().await;
    let mut updated = routes.clone();
    let pattern = input.pattern.trim().to_owned();
    if let Some(original) = original_pattern {
        let Some(index) = updated.iter().position(|route| route.pattern == original) else {
            return api_error(StatusCode::NOT_FOUND, "Model route not found");
        };
        if pattern != original && updated.iter().any(|route| route.pattern == pattern) {
            return api_error(
                StatusCode::CONFLICT,
                "A model route with that pattern already exists",
            );
        }
        updated[index] = routes::ModelRoute {
            pattern,
            targets: input.targets,
            cursor: Default::default(),
        };
    } else if let Some(route) = updated.iter_mut().find(|route| route.pattern == pattern) {
        route.targets = input.targets;
    } else {
        updated.push(routes::ModelRoute {
            pattern,
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
    offset: Option<usize>,
    query: Option<String>,
    status: Option<String>,
    provider: Option<String>,
    providers: Option<String>,
    models: Option<String>,
    api_keys: Option<String>,
    buckets: Option<usize>,
    until: Option<u64>,
}

struct OwnedActivityFilters {
    providers: Vec<String>,
    models: Vec<String>,
    api_keys: Vec<String>,
}

impl OwnedActivityFilters {
    fn from_query(query: &ActivityQuery) -> Self {
        let mut providers = split_activity_filter(query.providers.as_deref());
        if let Some(provider) = query.provider.as_deref().filter(|value| !value.is_empty())
            && !providers.iter().any(|value| value == provider)
        {
            providers.push(provider.to_owned());
        }
        Self {
            providers,
            models: split_activity_filter(query.models.as_deref()),
            api_keys: split_activity_filter(query.api_keys.as_deref()),
        }
    }

    fn borrowed(&self) -> crate::activity::ActivityFilters<'_> {
        crate::activity::ActivityFilters {
            providers: &self.providers,
            models: &self.models,
            api_keys: &self.api_keys,
        }
    }
}

fn split_activity_filter(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

#[derive(Serialize)]
struct ActivityLogPage {
    data: Vec<crate::activity::RequestLog>,
    total: usize,
    offset: usize,
    limit: usize,
}
async fn activity_logs(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> impl IntoResponse {
    let filters = OwnedActivityFilters::from_query(&query);
    axum::Json(
        state
            .activity
            .logs(
                query.since.unwrap_or(0),
                query.until.unwrap_or_else(now),
                filters.borrowed(),
                query.limit.unwrap_or(100),
            )
            .await,
    )
}
async fn activity_log_page(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> impl IntoResponse {
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 100);
    let filters = OwnedActivityFilters::from_query(&query);
    let (data, total) = state
        .activity
        .query_logs(crate::activity::ActivityLogQuery {
            since: query.since.unwrap_or(0),
            until: query.until.unwrap_or_else(now),
            filters: filters.borrowed(),
            text: query.query.as_deref(),
            status: query.status.as_deref(),
            offset,
            limit,
        })
        .await;
    axum::Json(ActivityLogPage {
        data,
        total,
        offset,
        limit,
    })
}
async fn activity_stats(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> impl IntoResponse {
    let filters = OwnedActivityFilters::from_query(&query);
    axum::Json(
        state
            .activity
            .stats(
                query.since.unwrap_or(0),
                filters.borrowed(),
                query.buckets.unwrap_or(24),
                query.until.unwrap_or_else(now),
            )
            .await,
    )
}

#[derive(Default, Deserialize)]
struct ActivityCostRecalculationRequest {
    request_id: Option<String>,
    source_instance_id: Option<String>,
}

async fn recalculate_activity_costs(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<ActivityCostRecalculationRequest>,
) -> Response {
    let record = match (
        input.request_id.as_deref(),
        input.source_instance_id.as_deref(),
    ) {
        (None, None) => None,
        (Some(""), _) => {
            return api_error(StatusCode::BAD_REQUEST, "request_id cannot be empty");
        }
        (Some(request_id), source_instance_id) => Some((request_id, source_instance_id)),
        (None, Some(_)) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "source_instance_id requires request_id",
            );
        }
    };
    let global = state.pricing.read().await.clone();
    let providers = state.providers.read().await.clone();
    let result = state
        .activity
        .recalculate_non_reported_costs(record, |log| {
            let Some(upstream_model) = log.upstream_model.as_deref() else {
                return crate::activity::CostRecalculationResolution::MissingRoute;
            };
            let Some(provider) = providers.get(&log.provider) else {
                return crate::activity::CostRecalculationResolution::MissingRoute;
            };
            let Some(endpoint) = provider
                .endpoints
                .iter()
                .find(|endpoint| endpoint.id == log.endpoint)
            else {
                return crate::activity::CostRecalculationResolution::MissingRoute;
            };
            match crate::pricing::effective_pricing(&global, provider, endpoint, upstream_model) {
                Some(pricing) => crate::activity::CostRecalculationResolution::Available(pricing),
                None => crate::activity::CostRecalculationResolution::MissingPricing,
            }
        })
        .await;
    match result {
        Ok(result) => axum::Json(result).into_response(),
        Err(error) => {
            error!(%error, "failed to recalculate Activity costs");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not recalculate Activity costs",
            )
        }
    }
}

async fn preview_activity_export(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> impl IntoResponse {
    axum::Json(state.activity.summary(query.since.unwrap_or(0)).await)
}

async fn activity_settings(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(crate::activity::ActivitySettings {
        retention_days: state.activity.retention_days(),
    })
}

async fn update_activity_settings(
    State(state): State<AppState>,
    axum::Json(settings): axum::Json<crate::activity::ActivitySettings>,
) -> Response {
    if !(1..=3650).contains(&settings.retention_days) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Activity retention must be between 1 and 3650 days",
        );
    }
    match state
        .activity
        .set_retention_days(settings.retention_days)
        .await
    {
        Ok(()) => axum::Json(settings).into_response(),
        Err(err) => {
            error!(%err, "failed to persist activity retention");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to save activity retention",
            )
        }
    }
}

async fn export_activity(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> Response {
    let records = state
        .activity
        .export_records(query.since.unwrap_or(0))
        .await;
    let exported_at = now();
    let export = crate::activity::ActivityExport {
        format: "yabane-activity",
        version: 1,
        instance_id: state.activity.instance_id(),
        exported_at,
        records: &records,
    };
    let body = serde_json::to_vec(&export).expect("serialize activity export");
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_str(&format!(
            "attachment; filename=\"yabane-activity-{exported_at}.json\""
        ))
        .expect("valid activity export filename"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

fn parse_activity_import(
    body: &Bytes,
) -> Result<crate::activity::ActivityImport, (StatusCode, &'static str)> {
    if body.len() > 64 * 1024 * 1024 {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "Activity import is too large",
        ));
    }
    serde_json::from_slice(body).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "Activity import must be a valid Yabane activity JSON file",
        )
    })
}

async fn preview_activity_import(State(state): State<AppState>, body: Bytes) -> Response {
    let import = match parse_activity_import(&body) {
        Ok(import) => import,
        Err((status, message)) => return api_error(status, message),
    };
    match state.activity.preview_import(&import).await {
        Ok(result) => axum::Json(result).into_response(),
        Err(message) => api_error(StatusCode::BAD_REQUEST, message),
    }
}

async fn import_activity(State(state): State<AppState>, body: Bytes) -> Response {
    let import: crate::activity::ActivityImport = match parse_activity_import(&body) {
        Ok(import) => import,
        Err((status, message)) => return api_error(status, message),
    };
    match state.activity.import(import).await {
        Ok(result) => axum::Json(result).into_response(),
        Err(error) => activity_import_error(error),
    }
}

fn activity_import_error(error: crate::activity::ActivityImportError) -> Response {
    match error {
        crate::activity::ActivityImportError::Invalid(message) => {
            api_error(StatusCode::BAD_REQUEST, message)
        }
        crate::activity::ActivityImportError::Persist(error) => {
            error!(%error, "failed to persist imported activity");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist imported Activity",
            )
        }
    }
}

async fn list_extensions(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(state.extensions.views())
}

#[derive(Deserialize)]
struct ExtensionUpdate {
    enabled: bool,
}

async fn update_extension(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::Json(input): axum::Json<ExtensionUpdate>,
) -> Response {
    #[cfg(feature = "extension-openai-subscription")]
    if id == yabane_extension_openai_subscription::ID && !input.enabled {
        // OAuth completion takes the same lock and rechecks the Extension before
        // attaching an Endpoint. Holding it through disablement makes the switch
        // a clean boundary without coupling enabled state to saved resources.
        let _providers = state.providers.write().await;
        return set_extension_enabled(&state, &id, false).await;
    }
    #[cfg(feature = "extension-traffic-capture")]
    if id == yabane_extension_traffic_capture::ID && !input.enabled {
        let capture = state.traffic_capture.status().await.config;
        if capture.active {
            return api_error(
                StatusCode::CONFLICT,
                "Stop Traffic Capture before disabling the extension",
            );
        }
    }
    set_extension_enabled(&state, &id, input.enabled).await
}

async fn set_extension_enabled(state: &AppState, id: &str, enabled: bool) -> Response {
    match state.extensions.set_enabled(id, enabled).await {
        Ok(extension) => axum::Json(extension).into_response(),
        Err(error) => extension_update_error(id, error),
    }
}

fn extension_update_error(id: &str, error: crate::extensions::UpdateError) -> Response {
    match error {
        crate::extensions::UpdateError::NotFound => {
            api_error(StatusCode::NOT_FOUND, "Extension not found")
        }
        crate::extensions::UpdateError::DisabledByCli => api_error(
            StatusCode::CONFLICT,
            "Extensions are disabled for this process by --no-extensions",
        ),
        crate::extensions::UpdateError::Persist(error) => {
            error!(%error, extension = %id, "persist extension settings");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not persist extension settings",
            )
        }
    }
}

#[cfg(feature = "extension-traffic-capture")]
async fn traffic_capture_status(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(state.traffic_capture.status().await)
}

#[cfg(feature = "extension-traffic-capture")]
async fn configure_traffic_capture(
    State(state): State<AppState>,
    axum::Json(config): axum::Json<yabane_extension_traffic_capture::CaptureConfig>,
) -> Response {
    if config.active && !state.extensions.is_enabled("traffic-capture") {
        return api_error(
            StatusCode::CONFLICT,
            "Enable the Traffic Capture extension before starting capture",
        );
    }
    if config.provider_id.is_empty() != config.endpoint_id.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Capture Provider and Endpoint must be specified together",
        );
    }
    // Retain this guard through Capture persistence. Provider and Endpoint
    // deletion lock Providers first and recheck active Capture state, preventing
    // a validated scope from becoming orphaned before it is activated.
    let providers = state.providers.read().await;
    if !config.provider_id.is_empty() {
        let Some(provider) = providers.get(&config.provider_id) else {
            return api_error(StatusCode::BAD_REQUEST, "Capture Provider was not found");
        };
        if !provider
            .endpoints
            .iter()
            .any(|endpoint| endpoint.id == config.endpoint_id)
        {
            return api_error(StatusCode::BAD_REQUEST, "Capture Endpoint was not found");
        }
    }
    match state.traffic_capture.configure(config).await {
        Ok(status) => axum::Json(status).into_response(),
        Err(message) => api_error(StatusCode::BAD_REQUEST, message),
    }
}

#[cfg(feature = "extension-traffic-capture")]
async fn stop_traffic_capture(State(state): State<AppState>) -> Response {
    match state.traffic_capture.stop().await {
        Ok(status) => axum::Json(status).into_response(),
        Err(message) => api_error(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

#[cfg(feature = "extension-traffic-capture")]
async fn list_traffic_captures(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(state.traffic_capture.list().await)
}

#[cfg(feature = "extension-traffic-capture")]
async fn get_traffic_capture(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
) -> Response {
    match state.traffic_capture.get(&request_id).await {
        Some(capture) => axum::Json(capture).into_response(),
        None => api_error(StatusCode::NOT_FOUND, "Traffic capture not found"),
    }
}

#[cfg(feature = "extension-traffic-capture")]
async fn delete_traffic_capture(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
) -> Response {
    match state.traffic_capture.delete(&request_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => api_error(StatusCode::NOT_FOUND, "Traffic capture not found"),
        Err(message) => api_error(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

#[cfg(feature = "extension-traffic-capture")]
async fn delete_all_traffic_captures(State(state): State<AppState>) -> Response {
    match state.traffic_capture.delete_all().await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) => api_error(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

async fn get_global_pricing(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(state.pricing.read().await.clone())
}

async fn update_global_pricing(
    State(state): State<AppState>,
    axum::Json(mut pricing): axum::Json<crate::pricing::PricingTable>,
) -> Response {
    if let Err(message) = crate::pricing::validate_table(&pricing, "Global pricing") {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    pricing.updated_at = if pricing.models.is_empty() { 0 } else { now() };
    let mut current = state.pricing.write().await;
    if let Err(error) = crate::pricing::save(&pricing).await {
        error!(%error, "failed to persist global pricing");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save global pricing",
        );
    }
    *current = pricing;
    StatusCode::NO_CONTENT.into_response()
}

async fn update_provider_pricing(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
    axum::Json(pricing): axum::Json<crate::pricing::PricingTable>,
) -> Response {
    if let Err(message) =
        crate::pricing::validate_table(&pricing, &format!("Provider '{provider_id}'"))
    {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    let mut providers = state.providers.write().await;
    let mut updated = providers.clone();
    let Some(provider) = updated.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    provider.pricing = normalize_pricing(pricing);
    let response = persist_or_error(&updated).await;
    if response.status().is_success() {
        *providers = updated;
    }
    response
}

async fn update_endpoint_pricing(
    State(state): State<AppState>,
    Path((provider_id, endpoint_id)): Path<(String, String)>,
    axum::Json(pricing): axum::Json<crate::pricing::PricingTable>,
) -> Response {
    if let Err(message) =
        crate::pricing::validate_table(&pricing, &format!("Endpoint '{provider_id}/{endpoint_id}'"))
    {
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
    endpoint.pricing = normalize_pricing(pricing);
    let response = persist_or_error(&updated).await;
    if response.status().is_success() {
        *providers = updated;
    }
    response
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
        pricing: provider.pricing.clone(),
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
                pricing: endpoint.pricing.clone(),
                requires_api_key: endpoint.requires_api_key,
                subscription_connected: endpoint.openai_subscription.is_some(),
                subscription_expires_at: endpoint
                    .openai_subscription
                    .as_ref()
                    .map(|credential| credential.expires_at),
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
    id: Option<String>,
    name: Option<String>,
    extra_headers: Option<std::collections::HashMap<String, String>>,
    extra_body: Option<serde_json::Map<String, serde_json::Value>>,
    pricing: Option<crate::pricing::PricingTable>,
    defaults_endpoint_ids: Option<Vec<String>>,
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
    if input.id.as_deref().is_some_and(|new_id| new_id != id) {
        return api_error(StatusCode::BAD_REQUEST, "Provider ID cannot be changed");
    }
    if let Some(headers) = &input.extra_headers
        && let Err(message) = validate_extra_headers(headers)
    {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    if let Some(pricing) = &input.pricing
        && let Err(message) = crate::pricing::validate_table(pricing, &format!("Provider '{id}'"))
    {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    let mut providers = state.providers.write().await;
    let mut updated = providers.clone();
    let Some(provider) = updated.get_mut(&id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    if let Some(name) = input.name {
        let name = name.trim();
        if name.is_empty() {
            return api_error(StatusCode::BAD_REQUEST, "Provider display name is required");
        }
        provider.name = name.to_owned();
    }
    if let Some(mut endpoint_ids) = input.defaults_endpoint_ids {
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
        provider.defaults_endpoint_ids = endpoint_ids;
    }
    if let Some(headers) = input.extra_headers {
        provider.extra_headers = headers;
    }
    if let Some(body) = input.extra_body {
        provider.extra_body = body;
    }
    if let Some(pricing) = input.pricing {
        provider.pricing = normalize_pricing(pricing);
    }
    let response = persist_or_error(&updated).await;
    if response.status().is_success() {
        *providers = updated;
    }
    response
}

fn normalize_pricing(
    mut pricing: crate::pricing::PricingTable,
) -> Option<crate::pricing::PricingTable> {
    if pricing.models.is_empty() {
        None
    } else {
        pricing.updated_at = now();
        Some(pricing)
    }
}

async fn start_openai_subscription(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<openai_subscription::StartSubscription>,
) -> Response {
    if state.extensions.provider_endpoint("openai_codex").is_none() {
        return api_error(
            StatusCode::CONFLICT,
            "OpenAI Subscription Extension is not enabled",
        );
    }
    match openai_subscription::start(&state, input).await {
        Ok(flow) => (StatusCode::CREATED, axum::Json(flow)).into_response(),
        Err(openai_subscription::StartError::Invalid(message)) => {
            api_error(StatusCode::BAD_REQUEST, message)
        }
        Err(openai_subscription::StartError::Upstream(message)) => {
            api_error(StatusCode::BAD_GATEWAY, message)
        }
    }
}

async fn start_openai_subscription_oauth(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<openai_subscription::StartSubscription>,
) -> Response {
    if state.extensions.provider_endpoint("openai_codex").is_none() {
        return api_error(
            StatusCode::CONFLICT,
            "OpenAI Subscription Extension is not enabled",
        );
    }
    match openai_subscription::start_browser(&state, input).await {
        Ok(flow) => (StatusCode::CREATED, axum::Json(flow)).into_response(),
        Err(openai_subscription::StartError::Invalid(message)) => {
            api_error(StatusCode::BAD_REQUEST, message)
        }
        Err(openai_subscription::StartError::Upstream(message)) => {
            api_error(StatusCode::BAD_GATEWAY, message)
        }
    }
}

async fn complete_openai_subscription_oauth(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::Json(input): axum::Json<openai_subscription::CompleteBrowserAuthorization>,
) -> Response {
    if state.extensions.provider_endpoint("openai_codex").is_none() {
        return api_error(
            StatusCode::CONFLICT,
            "OpenAI Subscription Extension is not enabled",
        );
    }
    match openai_subscription::complete_browser(&state, &id, input).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(openai_subscription::CompleteError::Invalid(message)) => {
            api_error(StatusCode::BAD_REQUEST, message)
        }
        Err(openai_subscription::CompleteError::Upstream(message)) => {
            api_error(StatusCode::BAD_GATEWAY, message)
        }
        Err(openai_subscription::CompleteError::Internal(message)) => {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, message)
        }
    }
}

async fn poll_openai_subscription(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    if state.extensions.provider_endpoint("openai_codex").is_none() {
        return api_error(
            StatusCode::CONFLICT,
            "OpenAI Subscription Extension is not enabled",
        );
    }
    match openai_subscription::poll(&state, &id).await {
        Ok(flow) => axum::Json(flow).into_response(),
        Err(message) => api_error(StatusCode::NOT_FOUND, message),
    }
}

async fn create_provider(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<CreateProvider>,
) -> Response {
    if input.endpoint.api_type == ApiType::OpenaiCodex {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Connect an OpenAI subscription with the device sign-in endpoint",
        );
    }
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
    if let Err(message) = validate_endpoint_base_url(&input.endpoint.base_url) {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    if let Err(message) = validate_extra_headers(&input.endpoint.extra_headers) {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    if let Some(pricing) = &input.endpoint.pricing
        && let Err(message) = crate::pricing::validate_table(pricing, "Endpoint")
    {
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
        pricing: None,
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
            pricing: input.endpoint.pricing.and_then(normalize_pricing),
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
    #[cfg(feature = "extension-traffic-capture")]
    let capture_update = {
        // Capture configuration keeps a Provider read lock through activation,
        // so checking after taking the write lock closes the validation/delete race.
        let capture = state.traffic_capture.status().await.config;
        if capture.active && capture.provider_id == id {
            return api_error(
                StatusCode::CONFLICT,
                "Stop Traffic Capture before deleting its scoped Provider",
            );
        }
        if capture.provider_id == id {
            let mut cleared = capture.clone();
            cleared.provider_id.clear();
            cleared.endpoint_id.clear();
            cleared.model.clear();
            Some((capture, cleared))
        } else {
            None
        }
    };
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

    let mut auth = state.auth.write().await;
    let mut updated_auth = auth.clone();
    updated_auth.api_keys.retain_mut(|key| {
        if !key
            .provider_ids
            .iter()
            .any(|provider_id| provider_id == &id)
        {
            return true;
        }
        key.provider_ids.retain(|provider_id| provider_id != &id);
        // An empty allowlist means unrestricted access, so revoke a key whose
        // only scope was the deleted Provider rather than broadening it.
        !key.provider_ids.is_empty()
    });

    #[cfg(feature = "extension-traffic-capture")]
    if let Some((_, config)) = &capture_update
        && let Err(err) = state.traffic_capture.configure(config.clone()).await
    {
        error!(%err, "failed to clear Traffic Capture scope for Provider deletion");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not clear Traffic Capture for the Provider deletion",
        );
    }

    if let Err(err) =
        save_auth_provider_and_routes(&updated_auth, &updated_providers, &updated_routes).await
    {
        #[cfg(feature = "extension-traffic-capture")]
        if let Some((previous, _)) = capture_update
            && let Err(rollback_err) = state.traffic_capture.configure(previous).await
        {
            error!(%rollback_err, "failed to roll back Traffic Capture after Provider deletion failure");
        }
        error!(%err, "failed to persist provider deletion");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not delete Provider and its dependent configuration",
        );
    }
    *auth = updated_auth;
    *providers = updated_providers;
    *routes = updated_routes;
    StatusCode::NO_CONTENT.into_response()
}

async fn create_endpoint(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
    axum::Json(input): axum::Json<CreateEndpoint>,
) -> Response {
    if input.api_type == ApiType::OpenaiCodex {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Connect an OpenAI subscription with the device sign-in endpoint",
        );
    }
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
    if let Err(message) = validate_endpoint_base_url(&input.base_url) {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    if let Err(message) = validate_extra_headers(&input.extra_headers) {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    if let Some(pricing) = &input.pricing
        && let Err(message) = crate::pricing::validate_table(pricing, "Endpoint")
    {
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
        pricing: input.pricing.and_then(normalize_pricing),
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
    let new_endpoint_id = input.id.as_deref().unwrap_or(&endpoint_id).to_owned();
    if !valid_id(&new_endpoint_id) {
        return api_error(StatusCode::BAD_REQUEST, "Endpoint ID must be a URL slug");
    }
    if let Err(message) = validate_socks5_proxy(input.socks5_proxy.as_deref()) {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    if let Err(message) = validate_endpoint_base_url(&input.base_url) {
        return api_error(StatusCode::BAD_REQUEST, message);
    }
    if let Some(pricing) = &input.pricing
        && let Err(message) = crate::pricing::validate_table(
            pricing,
            &format!("Endpoint '{}/{}'", provider_id, endpoint_id),
        )
    {
        return api_error(StatusCode::BAD_REQUEST, message);
    }

    let mut providers = state.providers.write().await;
    let mut updated = providers.clone();
    let Some(provider) = updated.get_mut(&provider_id) else {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    };
    let Some(endpoint_index) = provider
        .endpoints
        .iter()
        .position(|endpoint| endpoint.id == endpoint_id)
    else {
        return api_error(StatusCode::NOT_FOUND, "API endpoint not found");
    };
    if new_endpoint_id != endpoint_id
        && provider
            .endpoints
            .iter()
            .any(|endpoint| endpoint.id == new_endpoint_id)
    {
        return api_error(StatusCode::CONFLICT, "Endpoint ID already exists");
    }

    let endpoint = &provider.endpoints[endpoint_index];
    let subscription_endpoint = endpoint.api_type == ApiType::OpenaiCodex;
    let normalized_base_url = input.base_url.trim().trim_end_matches('/').to_owned();
    let normalized_proxy = normalized_socks5_proxy(input.socks5_proxy.as_deref());
    if subscription_endpoint {
        if input.api_type != ApiType::OpenaiCodex
            || normalized_base_url != endpoint.base_url
            || input.requires_api_key
        {
            return api_error(
                StatusCode::BAD_REQUEST,
                "OpenAI subscription Endpoints allow ID and SOCKS5 proxy changes only; delete and reconnect to change other settings",
            );
        }
    } else if input.api_type == ApiType::OpenaiCodex {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Connect a new OpenAI subscription instead of converting an existing Endpoint",
        );
    }
    let stopped_requiring_api_key = endpoint.requires_api_key && !input.requires_api_key;
    let started_requiring_api_key = !endpoint.requires_api_key && input.requires_api_key;
    let connection_changed = endpoint.api_type != input.api_type
        || endpoint.base_url != normalized_base_url
        || endpoint.socks5_proxy != normalized_proxy
        || endpoint.requires_api_key != input.requires_api_key;
    let renamed = new_endpoint_id != endpoint_id;

    let mut routes = state.routes.0.write().await;
    if started_requiring_api_key
        && routes.iter().any(|route| {
            route.targets.iter().any(|target| {
                target.provider_id == provider_id && target.endpoint_id == endpoint_id
            })
        })
    {
        return api_error(
            StatusCode::CONFLICT,
            "Update or delete model routes targeting this Endpoint before requiring an API key",
        );
    }

    #[cfg(feature = "extension-traffic-capture")]
    let capture_update = if renamed {
        let status = state.traffic_capture.status().await;
        if status.config.provider_id == provider_id && status.config.endpoint_id == endpoint_id {
            if status.config.active {
                return api_error(
                    StatusCode::CONFLICT,
                    "Stop Traffic Capture before changing its scoped Endpoint ID",
                );
            }
            let previous = status.config;
            let mut updated = previous.clone();
            updated.endpoint_id = new_endpoint_id.clone();
            Some((previous, updated))
        } else {
            None
        }
    } else {
        None
    };

    {
        let endpoint = &mut provider.endpoints[endpoint_index];
        endpoint.id = new_endpoint_id.clone();
        endpoint.api_type = input.api_type;
        endpoint.base_url = normalized_base_url;
        endpoint.socks5_proxy = normalized_proxy;
        endpoint.requires_api_key = input.requires_api_key;
        if let Some(pricing) = input.pricing {
            endpoint.pricing = normalize_pricing(pricing);
        }
        endpoint.proxy_client = Default::default();
    }

    if connection_changed && !subscription_endpoint {
        remove_endpoint_discovery(provider, &endpoint_id);
    } else if renamed {
        rename_endpoint_references(provider, &endpoint_id, &new_endpoint_id);
    }
    if renamed {
        for configured_id in &mut provider.defaults_endpoint_ids {
            if configured_id == &endpoint_id {
                *configured_id = new_endpoint_id.clone();
            }
        }
    }

    let mut updated_routes = routes.clone();
    for route in &mut updated_routes {
        for target in &mut route.targets {
            if target.provider_id == provider_id && target.endpoint_id == endpoint_id {
                if renamed {
                    target.endpoint_id = new_endpoint_id.clone();
                }
                if stopped_requiring_api_key {
                    target.api_key_id.clear();
                }
            }
        }
    }

    #[cfg(feature = "extension-traffic-capture")]
    if let Some((_, config)) = &capture_update
        && let Err(err) = state.traffic_capture.configure(config.clone()).await
    {
        error!(%err, "failed to update Traffic Capture scope for Endpoint rename");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not update Traffic Capture for the Endpoint ID change",
        );
    }

    let response = match save_provider_and_routes(&updated, &updated_routes).await {
        Ok(()) => {
            *providers = updated;
            *routes = updated_routes;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => {
            #[cfg(feature = "extension-traffic-capture")]
            if let Some((previous, _)) = capture_update
                && let Err(rollback_err) = state.traffic_capture.configure(previous).await
            {
                error!(%rollback_err, "failed to roll back Traffic Capture after Endpoint update failure");
            }
            error!(%err, "failed to persist Endpoint update");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not update API endpoint and its references",
            )
        }
    };
    drop(routes);
    drop(providers);
    if response.status().is_success() && connection_changed && !subscription_endpoint {
        spawn_provider_refresh(state, provider_id);
    }
    response
}

async fn delete_endpoint(
    State(state): State<AppState>,
    Path((provider_id, endpoint_id)): Path<(String, String)>,
) -> Response {
    let mut providers = state.providers.write().await;
    #[cfg(feature = "extension-traffic-capture")]
    let capture_update = {
        // See configure_traffic_capture: both operations serialize on Providers
        // before observing or changing Capture scope.
        let capture = state.traffic_capture.status().await.config;
        let scoped = capture.provider_id == provider_id && capture.endpoint_id == endpoint_id;
        if capture.active && scoped {
            return api_error(
                StatusCode::CONFLICT,
                "Stop Traffic Capture before deleting its scoped Endpoint",
            );
        }
        if scoped {
            let mut cleared = capture.clone();
            cleared.provider_id.clear();
            cleared.endpoint_id.clear();
            cleared.model.clear();
            Some((capture, cleared))
        } else {
            None
        }
    };
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
    #[cfg(feature = "extension-traffic-capture")]
    if let Some((_, config)) = &capture_update
        && let Err(err) = state.traffic_capture.configure(config.clone()).await
    {
        error!(%err, "failed to clear Traffic Capture scope for Endpoint deletion");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not clear Traffic Capture for the Endpoint deletion",
        );
    }

    if let Err(err) = save_provider_and_routes(&updated_providers, &updated_routes).await {
        #[cfg(feature = "extension-traffic-capture")]
        if let Some((previous, _)) = capture_update
            && let Err(rollback_err) = state.traffic_capture.configure(previous).await
        {
            error!(%rollback_err, "failed to roll back Traffic Capture after Endpoint deletion failure");
        }
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
    if endpoint.api_type == ApiType::OpenaiCodex {
        return api_error(
            StatusCode::BAD_REQUEST,
            "OpenAI subscription Endpoints use OAuth instead of API keys",
        );
    }
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
    if key.enabled && input.enabled == Some(false) {
        let routes = state.routes.0.read().await;
        if routes.iter().any(|route| {
            route.targets.iter().any(|target| {
                target.provider_id == provider_id
                    && target.endpoint_id == endpoint_id
                    && target.api_key_id == key_id
            })
        }) {
            return api_error(
                StatusCode::CONFLICT,
                "Update or delete model routes targeting this API key before disabling it",
            );
        }
    }
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
    if let Err(err) = save_provider_and_routes(&updated_providers, &updated).await {
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

fn rename_endpoint_references(provider: &mut Provider, old_id: &str, new_id: &str) {
    for endpoint_ids in provider.model_endpoints.values_mut() {
        for endpoint_id in endpoint_ids {
            if endpoint_id == old_id {
                *endpoint_id = new_id.to_owned();
            }
        }
    }
    for preference in &mut provider.model_endpoint_preferences {
        if preference.endpoint_id == old_id {
            preference.endpoint_id = new_id.to_owned();
        }
    }
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
                | "cookie"
                | "set-cookie"
                | "chatgpt-account-id"
                | "originator"
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

fn validate_endpoint_base_url(value: &str) -> Result<(), &'static str> {
    let path = value
        .split_once("://")
        .map(|(_, rest)| rest.split_once('/').map_or("", |(_, path)| path))
        .unwrap_or(value)
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    if ["/chat/completions", "/responses", "/messages", "/models"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
    {
        return Err("Endpoint base URL must be the shared API root, not a specific operation URL");
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

async fn save_auth_provider_and_routes(
    auth: &auth::AuthConfig,
    providers: &std::collections::HashMap<String, Provider>,
    routes: &[routes::ModelRoute],
) -> Result<(), String> {
    let writes = [
        AtomicWrite::json(auth::AUTH_FILE, auth)
            .map_err(|err| format!("serialize authentication: {err}"))?,
        providers_atomic_write(providers)?,
        AtomicWrite::json(routes::ROUTES_FILE, &routes)
            .map_err(|err| format!("serialize routes: {err}"))?,
    ];
    write_transaction(CONFIG_TRANSACTION_FILE, &writes)
        .await
        .map_err(|err| format!("save authentication, providers, and routes: {err}"))
}

async fn save_provider_and_routes(
    providers: &std::collections::HashMap<String, Provider>,
    routes: &[routes::ModelRoute],
) -> Result<(), String> {
    let writes = [
        providers_atomic_write(providers)?,
        AtomicWrite::json(routes::ROUTES_FILE, &routes)
            .map_err(|err| format!("serialize routes: {err}"))?,
    ];
    write_transaction(CONFIG_TRANSACTION_FILE, &writes)
        .await
        .map_err(|err| format!("save providers and routes: {err}"))
}

fn providers_atomic_write(
    providers: &std::collections::HashMap<String, Provider>,
) -> Result<AtomicWrite, String> {
    let mut values: Vec<_> = providers.values().cloned().collect();
    values.sort_by(|left, right| left.id.cmp(&right.id));
    AtomicWrite::json(crate::config::PROVIDERS_FILE, &values)
        .map_err(|err| format!("serialize providers: {err}"))
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::config::{ApiEndpoint, ApiType, OpenAiSubscription, Provider};

    #[tokio::test]
    async fn activity_import_persistence_error_is_server_side_and_redacted() {
        let response = super::activity_import_error(crate::activity::ActivityImportError::Persist(
            std::io::Error::other("private/data/activity.jsonl: disk failure"),
        ));

        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("Failed to persist imported Activity"));
        assert!(!body.contains("private/data/activity.jsonl"));
        assert!(!body.contains("disk failure"));
    }

    #[test]
    fn pricing_normalization_uses_server_timestamp_and_empty_clears() {
        let table = crate::pricing::PricingTable {
            updated_at: 1,
            models: HashMap::from([(
                "model".to_owned(),
                crate::pricing::ModelPricing {
                    input_per_million: Some(1.0),
                    output_per_million: Some(2.0),
                    ..Default::default()
                },
            )]),
        };
        let normalized = super::normalize_pricing(table).unwrap();
        assert!(normalized.updated_at > 1);
        assert!(
            super::normalize_pricing(crate::pricing::PricingTable {
                updated_at: 99,
                models: HashMap::new(),
            })
            .is_none()
        );
    }

    #[test]
    fn provider_view_redacts_subscription_tokens_and_account_id() {
        let provider = Provider {
            id: "openai".to_owned(),
            name: "OpenAI".to_owned(),
            extra_headers: HashMap::new(),
            extra_body: serde_json::Map::new(),
            pricing: None,
            defaults_endpoint_ids: Vec::new(),
            endpoints: vec![ApiEndpoint {
                id: "chatgpt".to_owned(),
                api_type: ApiType::OpenaiCodex,
                base_url: "https://chatgpt.com/backend-api".to_owned(),
                requires_api_key: false,
                openai_subscription: Some(OpenAiSubscription {
                    access_token: "private-access".to_owned(),
                    refresh_token: "private-refresh".to_owned(),
                    expires_at: 123,
                    account_id: "private-account".to_owned(),
                }),
                ..ApiEndpoint::default()
            }],
            discovered_models: Vec::new(),
            model_endpoints: HashMap::new(),
            model_endpoint_preferences: Vec::new(),
            models_discovered_at: None,
            model_discovery_error: None,
        };

        let json = serde_json::to_string(&super::provider_view(&provider)).unwrap();
        assert!(!json.contains("private-access"));
        assert!(!json.contains("private-refresh"));
        assert!(!json.contains("private-account"));
        assert!(json.contains("\"subscription_connected\":true"));
        assert!(json.contains("\"subscription_expires_at\":123"));
    }
}

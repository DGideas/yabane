use std::{
    collections::{HashMap, HashSet},
    env,
    net::SocketAddr,
    sync::Arc,
};

use axum::{
    Router,
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::{net::TcpListener, sync::RwLock};
use tracing::{error, info};

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_CSS: &str = include_str!("../web/app.css");
const APP_JS: &str = include_str!("../web/app.js");
const PROVIDERS_FILE: &str = "data/providers.json";

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    providers: Arc<RwLock<HashMap<String, Provider>>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProviderKind {
    OpenaiCompatible,
    Anthropic,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Provider {
    id: String,
    name: String,
    kind: ProviderKind,
    base_url: String,
    api_key: String,
    #[serde(default)]
    models: Vec<String>,
}

#[derive(Serialize)]
struct ProviderView {
    id: String,
    name: String,
    kind: ProviderKind,
    base_url: String,
    models: Vec<String>,
}

#[derive(Deserialize)]
struct CreateProvider {
    id: String,
    name: String,
    kind: ProviderKind,
    base_url: String,
    api_key: String,
    #[serde(default)]
    models: Vec<String>,
}

#[derive(Serialize)]
struct ApiError<'a> {
    error: ApiErrorBody<'a>,
}

#[derive(Serialize)]
struct ApiErrorBody<'a> {
    message: &'a str,
    #[serde(rename = "type")]
    kind: &'a str,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let providers = load_providers().await;
    let state = AppState {
        client: reqwest::Client::builder()
            .pool_max_idle_per_host(64)
            .tcp_nodelay(true)
            .build()
            .expect("build HTTP client"),
        providers: Arc::new(RwLock::new(providers)),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/app.css", get(css))
        .route("/app.js", get(js))
        .route("/healthz", get(health))
        .route(
            "/admin/providers",
            get(list_providers).post(create_provider),
        )
        .route(
            "/admin/providers/{id}",
            axum::routing::delete(delete_provider),
        )
        .route("/v1/chat/completions", any(proxy_openai))
        .route("/v1/responses", any(proxy_openai))
        .route("/v1/messages", any(proxy_anthropic))
        .route("/providers/{provider_id}/{*path}", any(proxy_explicit))
        .with_state(state);

    let address: SocketAddr = env::var("YABANE_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse()
        .expect("YABANE_ADDR must be an address");
    let listener = TcpListener::bind(address).await.expect("bind server");
    info!(%address, "Yabane is listening");
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

async fn health() -> &'static str {
    "ok"
}

async fn list_providers(State(state): State<AppState>) -> impl IntoResponse {
    let providers = state.providers.read().await;
    let mut views: Vec<_> = providers
        .values()
        .map(|provider| ProviderView {
            id: provider.id.clone(),
            name: provider.name.clone(),
            kind: provider.kind.clone(),
            base_url: provider.base_url.clone(),
            models: provider.models.clone(),
        })
        .collect();
    views.sort_by(|a, b| a.name.cmp(&b.name));
    axum::Json(views)
}

async fn create_provider(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<CreateProvider>,
) -> Response {
    if input.id.trim().is_empty()
        || input.name.trim().is_empty()
        || input.base_url.trim().is_empty()
        || input.api_key.trim().is_empty()
    {
        return api_error(StatusCode::BAD_REQUEST, "All provider fields are required");
    }
    if !input
        .id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Provider ID may contain letters, numbers, '-' and '_'",
        );
    }

    let provider = Provider {
        id: input.id.trim().to_owned(),
        name: input.name.trim().to_owned(),
        kind: input.kind,
        base_url: input.base_url.trim().trim_end_matches('/').to_owned(),
        api_key: input.api_key.trim().to_owned(),
        models: input
            .models
            .into_iter()
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty())
            .collect(),
    };

    let mut providers = state.providers.write().await;
    if providers.contains_key(&provider.id) {
        return api_error(StatusCode::CONFLICT, "Provider ID already exists");
    }
    providers.insert(provider.id.clone(), provider);
    if let Err(err) = save_providers(&providers).await {
        error!(%err, "failed to persist providers");
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not save provider");
    }
    StatusCode::CREATED.into_response()
}

async fn delete_provider(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let mut providers = state.providers.write().await;
    if providers.remove(&id).is_none() {
        return api_error(StatusCode::NOT_FOUND, "Provider not found");
    }
    if let Err(err) = save_providers(&providers).await {
        error!(%err, "failed to persist providers");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save providers",
        );
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn proxy_openai(State(state): State<AppState>, request: Request) -> Response {
    proxy_by_kind(state, request, ProviderKind::OpenaiCompatible).await
}

async fn proxy_anthropic(State(state): State<AppState>, request: Request) -> Response {
    proxy_by_kind(state, request, ProviderKind::Anthropic).await
}

async fn proxy_by_kind(state: AppState, request: Request, kind: ProviderKind) -> Response {
    let provider_id = request
        .headers()
        .get("x-yabane-provider")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    let providers = state.providers.read().await;
    let provider = match provider_id {
        Some(id) => providers.get(&id).cloned(),
        None => providers
            .values()
            .find(|provider| {
                std::mem::discriminant(&provider.kind) == std::mem::discriminant(&kind)
            })
            .cloned(),
    };
    drop(providers);

    match provider {
        Some(provider)
            if std::mem::discriminant(&provider.kind) == std::mem::discriminant(&kind) =>
        {
            forward(state.client, provider, request, None).await
        }
        Some(_) => api_error(
            StatusCode::BAD_REQUEST,
            "Provider uses a different API type",
        ),
        None => api_error(StatusCode::BAD_GATEWAY, "No matching provider configured"),
    }
}

async fn proxy_explicit(
    State(state): State<AppState>,
    Path((provider_id, path)): Path<(String, String)>,
    mut request: Request,
) -> Response {
    let provider = state.providers.read().await.get(&provider_id).cloned();
    match provider {
        Some(provider) => {
            let query = request
                .uri()
                .query()
                .map(|value| format!("?{value}"))
                .unwrap_or_default();
            let rewritten = format!("/{path}{query}");
            *request.uri_mut() = rewritten.parse().expect("valid rewritten URI");
            forward(state.client, provider, request, Some(rewritten)).await
        }
        None => api_error(StatusCode::NOT_FOUND, "Provider not found"),
    }
}

async fn forward(
    client: reqwest::Client,
    provider: Provider,
    request: Request,
    explicit_path: Option<String>,
) -> Response {
    let (parts, body) = request.into_parts();
    let request_headers = sanitize_request_headers(parts.headers, &provider);
    let path_and_query = explicit_path.unwrap_or_else(|| {
        parts
            .uri
            .path_and_query()
            .map(|value| value.as_str().to_owned())
            .unwrap_or_else(|| "/".to_owned())
    });
    let target = join_upstream_url(&provider.base_url, &path_and_query);

    let mut upstream = client.request(to_reqwest_method(&parts.method), target);
    upstream = upstream.headers(request_headers);
    let stream = body
        .into_data_stream()
        .map(|chunk| chunk.map_err(|err| std::io::Error::other(err.to_string())));
    upstream = upstream.body(reqwest::Body::wrap_stream(stream));

    let upstream_response = match upstream.send().await {
        Ok(response) => response,
        Err(err) => {
            error!(provider = %provider.id, %err, "upstream request failed");
            return api_error(StatusCode::BAD_GATEWAY, "Upstream request failed");
        }
    };

    let status = upstream_response.status();
    let response_headers = upstream_response.headers().clone();
    let stream = upstream_response
        .bytes_stream()
        .map(|chunk| chunk.map_err(|err| std::io::Error::other(err.to_string())));
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    copy_response_headers(response.headers_mut(), &response_headers);
    response
}

fn join_upstream_url(base_url: &str, path_and_query: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let path = if base.ends_with("/v1") && path_and_query.starts_with("/v1/") {
        &path_and_query[3..]
    } else {
        path_and_query
    };
    format!(
        "{base}{}",
        if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        }
    )
}

fn sanitize_request_headers(mut headers: HeaderMap, provider: &Provider) -> HeaderMap {
    for name in [
        header::HOST.as_str(),
        header::AUTHORIZATION.as_str(),
        "x-api-key",
        header::CONTENT_LENGTH.as_str(),
        "x-yabane-provider",
    ] {
        headers.remove(name);
    }
    let value = match provider.kind {
        ProviderKind::OpenaiCompatible => {
            HeaderValue::from_str(&format!("Bearer {}", provider.api_key))
        }
        ProviderKind::Anthropic => HeaderValue::from_str(&provider.api_key),
    };
    if let Ok(value) = value {
        let name = match provider.kind {
            ProviderKind::OpenaiCompatible => header::AUTHORIZATION,
            ProviderKind::Anthropic => HeaderName::from_static("x-api-key"),
        };
        headers.insert(name, value);
    }
    headers
}

fn copy_response_headers(target: &mut HeaderMap, source: &reqwest::header::HeaderMap) {
    static SKIP: [&str; 4] = [
        "content-length",
        "transfer-encoding",
        "connection",
        "keep-alive",
    ];
    let skip: HashSet<&str> = SKIP.into_iter().collect();
    for (name, value) in source {
        if skip.contains(name.as_str()) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            target.append(name, value);
        }
    }
}

fn to_reqwest_method(method: &Method) -> reqwest::Method {
    reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST)
}

fn api_error(status: StatusCode, message: &'static str) -> Response {
    (
        status,
        axum::Json(ApiError {
            error: ApiErrorBody {
                message,
                kind: "yabane_error",
            },
        }),
    )
        .into_response()
}

async fn load_providers() -> HashMap<String, Provider> {
    match tokio::fs::read(PROVIDERS_FILE).await {
        Ok(contents) => serde_json::from_slice::<Vec<Provider>>(&contents)
            .unwrap_or_default()
            .into_iter()
            .map(|provider| (provider.id.clone(), provider))
            .collect(),
        Err(_) => HashMap::new(),
    }
}

async fn save_providers(providers: &HashMap<String, Provider>) -> Result<(), std::io::Error> {
    tokio::fs::create_dir_all("data").await?;
    let mut values: Vec<_> = providers.values().cloned().collect();
    values.sort_by(|a, b| a.id.cmp(&b.id));
    let contents = serde_json::to_vec_pretty(&values).expect("serialize providers");
    tokio::fs::write(PROVIDERS_FILE, contents).await
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

#[cfg(test)]
mod tests {
    use super::join_upstream_url;

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
}

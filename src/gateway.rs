use std::collections::HashSet;

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    response::Response,
};
use futures_util::StreamExt;
use tracing::error;

use crate::{
    api_error, auth,
    config::{ApiEndpoint, ApiKey, ApiType, AppState},
};

const MAX_REQUEST_BODY_SIZE: usize = 32 * 1024 * 1024;

#[derive(Debug)]
struct RoutingError {
    status: StatusCode,
    message: String,
}

pub async fn proxy_openai(State(state): State<AppState>, request: Request) -> Response {
    route_request(state, request, ApiType::OpenaiCompatible).await
}

pub async fn proxy_anthropic(State(state): State<AppState>, request: Request) -> Response {
    route_request(state, request, ApiType::Anthropic).await
}

async fn route_request(state: AppState, request: Request, expected_type: ApiType) -> Response {
    let allowed_providers = auth::authorized_provider_ids(&state, request.headers()).await;
    let (parts, body) = request.into_parts();
    let body = match axum::body::to_bytes(body, MAX_REQUEST_BODY_SIZE).await {
        Ok(body) => body,
        Err(_) => return api_error(StatusCode::PAYLOAD_TOO_LARGE, "Request body is too large"),
    };
    let (provider_id, endpoint, api_key, body) =
        match resolve_provider(&state, &body, expected_type, allowed_providers.as_deref()).await {
            Ok(resolved) => resolved,
            Err(err) => return api_error(err.status, err.message),
        };

    forward(state.client, provider_id, endpoint, api_key, parts, body).await
}

async fn resolve_provider(
    state: &AppState,
    body: &[u8],
    expected_type: ApiType,
    allowed_providers: Option<&[String]>,
) -> Result<(String, ApiEndpoint, Option<ApiKey>, Vec<u8>), RoutingError> {
    let mut payload: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| RoutingError {
            status: StatusCode::BAD_REQUEST,
            message: "Request body must be valid JSON".to_owned(),
        })?;
    let model = payload
        .get("model")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| RoutingError {
            status: StatusCode::BAD_REQUEST,
            message: "Request body must contain a model".to_owned(),
        })?;
    let (provider_id, upstream_model) = model.split_once('/').ok_or_else(|| RoutingError {
        status: StatusCode::BAD_REQUEST,
        message: "Model must use the provider/model format".to_owned(),
    })?;
    if provider_id.is_empty() || upstream_model.is_empty() {
        return Err(RoutingError {
            status: StatusCode::BAD_REQUEST,
            message: "Model must use the provider/model format".to_owned(),
        });
    }

    if allowed_providers.is_some_and(|allowed| {
        !allowed.is_empty() && !allowed.iter().any(|allowed_id| allowed_id == provider_id)
    }) {
        return Err(RoutingError {
            status: StatusCode::FORBIDDEN,
            message: format!("API key is not allowed to access provider '{provider_id}'"),
        });
    }

    let providers = state.providers.read().await;
    let provider = providers
        .get(provider_id)
        .cloned()
        .ok_or_else(|| RoutingError {
            status: StatusCode::BAD_REQUEST,
            message: format!("Unknown provider '{provider_id}' in model"),
        })?;
    let endpoint = provider
        .endpoints
        .iter()
        .find(|endpoint| endpoint.api_type == expected_type)
        .ok_or_else(|| RoutingError {
            status: StatusCode::BAD_REQUEST,
            message: format!("Provider '{provider_id}' has no matching API endpoint"),
        })?;
    let api_key = if let Some(route) = provider.route_for_model(upstream_model) {
        let (route_endpoint, key) = provider
            .endpoint_and_key(&route.endpoint_id, &route.api_key_id)
            .ok_or_else(|| RoutingError {
                status: StatusCode::CONFLICT,
                message: format!(
                    "Model route '{}' refers to a missing API key",
                    route.pattern
                ),
            })?;
        if route_endpoint.id != endpoint.id || !key.enabled {
            return Err(RoutingError {
                status: StatusCode::CONFLICT,
                message: format!(
                    "Model route '{}' has no enabled key for this endpoint",
                    route.pattern
                ),
            });
        }
        Some(key.clone())
    } else {
        endpoint.select_api_key().cloned()
    };
    if endpoint.requires_api_key && api_key.is_none() {
        return Err(RoutingError {
            status: StatusCode::CONFLICT,
            message: format!("Endpoint '{}' has no enabled API key", endpoint.id),
        });
    }

    payload["model"] = serde_json::Value::String(upstream_model.to_owned());
    let body = serde_json::to_vec(&payload).expect("serialize validated request body");
    Ok((provider.id.clone(), endpoint.clone(), api_key, body))
}

async fn forward(
    client: reqwest::Client,
    provider_id: String,
    endpoint: ApiEndpoint,
    api_key: Option<ApiKey>,
    parts: axum::http::request::Parts,
    body: Vec<u8>,
) -> Response {
    let request_headers =
        sanitize_request_headers(parts.headers, endpoint.api_type, api_key.as_ref());
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    let target = join_upstream_url(&endpoint.base_url, path_and_query);

    let mut upstream = client.request(to_reqwest_method(&parts.method), target);
    upstream = upstream.headers(request_headers).body(body);

    let upstream_response = match upstream.send().await {
        Ok(response) => response,
        Err(err) => {
            error!(provider = %provider_id, endpoint = %endpoint.id, %err, "upstream request failed");
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

pub(crate) fn join_upstream_url(base_url: &str, path_and_query: &str) -> String {
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

fn sanitize_request_headers(
    mut headers: HeaderMap,
    api_type: ApiType,
    api_key: Option<&ApiKey>,
) -> HeaderMap {
    for name in [
        header::HOST.as_str(),
        header::AUTHORIZATION.as_str(),
        "x-api-key",
        header::CONTENT_LENGTH.as_str(),
    ] {
        headers.remove(name);
    }
    if let Some(api_key) = api_key {
        let (name, value) = match api_type {
            ApiType::OpenaiCompatible => (
                header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", api_key.secret)),
            ),
            ApiType::Anthropic => (
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(&api_key.secret),
            ),
        };
        headers.insert(name, value.expect("validated API key header value"));
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

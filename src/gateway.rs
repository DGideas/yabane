use std::time::Instant;

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    response::Response,
};
use futures_util::StreamExt;
use rand::RngCore;
use tracing::error;

use crate::{
    activity::RequestLog,
    auth,
    config::{ApiEndpoint, ApiKey, ApiType, AppState, Provider},
    error::api_error,
    usage::UsageTracker,
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
    let requested_streaming = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.get("stream").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    let (provider, endpoint, api_key, model, body) =
        match resolve_provider(&state, &body, expected_type, allowed_providers.as_deref()).await {
            Ok(resolved) => resolved,
            Err(err) => return api_error(err.status, err.message),
        };

    forward(
        state,
        ForwardRequest {
            provider,
            endpoint,
            api_key,
            model,
            parts,
            body,
            requested_streaming,
        },
    )
    .await
}

async fn resolve_provider(
    state: &AppState,
    body: &[u8],
    expected_type: ApiType,
    allowed_providers: Option<&[String]>,
) -> Result<(Provider, ApiEndpoint, Option<ApiKey>, String, Vec<u8>), RoutingError> {
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
        })?
        .to_owned();
    let route_target = state.routes.resolve(&model).await;
    let (provider_id, upstream_model) = if let Some(target) = &route_target {
        (target.provider_id.as_str(), target.upstream_model.as_str())
    } else {
        model.split_once('/').ok_or_else(|| RoutingError {
            status: StatusCode::BAD_REQUEST,
            message: "Model must match a model route or use the provider/model format".to_owned(),
        })?
    };
    if provider_id.is_empty() || upstream_model.is_empty() {
        return Err(RoutingError {
            status: StatusCode::BAD_REQUEST,
            message: "Model must match a model route or use the provider/model format".to_owned(),
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
    let endpoint = if let Some(target) = &route_target {
        provider
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == target.endpoint_id)
    } else {
        let discovered = provider.model_endpoints.get(upstream_model);
        let preferred = provider.preferred_endpoint_id(upstream_model, expected_type);
        provider
            .endpoints
            .iter()
            .find(|endpoint| {
                preferred == Some(endpoint.id.as_str())
                    && endpoint.api_type == expected_type
                    && discovered.is_none_or(|endpoint_ids| endpoint_ids.contains(&endpoint.id))
            })
            .or_else(|| {
                provider.endpoints.iter().find(|endpoint| {
                    endpoint.api_type == expected_type
                        && discovered.is_none_or(|endpoint_ids| endpoint_ids.contains(&endpoint.id))
                })
            })
    }
    .ok_or_else(|| RoutingError {
        status: StatusCode::BAD_REQUEST,
        message: format!("Provider '{provider_id}' has no endpoint for model '{upstream_model}'"),
    })?;
    if endpoint.api_type != expected_type {
        return Err(RoutingError {
            status: StatusCode::BAD_REQUEST,
            message: format!("Model '{upstream_model}' is not available through this API type"),
        });
    }
    let api_key = if let Some(target) = &route_target {
        let key = endpoint
            .api_keys
            .iter()
            .find(|key| key.id == target.api_key_id)
            .ok_or_else(|| RoutingError {
                status: StatusCode::CONFLICT,
                message: format!("Model route for '{model}' refers to a missing API key"),
            })?;
        if !key.enabled {
            return Err(RoutingError {
                status: StatusCode::CONFLICT,
                message: format!("Model route for '{model}' has no enabled API key"),
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

    let endpoint = endpoint.clone();
    payload["model"] = serde_json::Value::String(upstream_model.to_owned());
    let body = serde_json::to_vec(&payload).expect("serialize validated request body");
    Ok((provider, endpoint, api_key, model, body))
}

fn request_id() -> String {
    let mut random = [0_u8; 16];
    rand::rng().fill_bytes(&mut random);
    let mut id = String::with_capacity(36);
    id.push_str("req-");
    for byte in random {
        use std::fmt::Write as _;
        write!(&mut id, "{byte:02x}").expect("write request ID");
    }
    id
}

struct ForwardRequest {
    provider: Provider,
    endpoint: ApiEndpoint,
    api_key: Option<ApiKey>,
    model: String,
    parts: axum::http::request::Parts,
    body: Vec<u8>,
    requested_streaming: bool,
}

async fn forward(state: AppState, request: ForwardRequest) -> Response {
    let ForwardRequest {
        provider,
        endpoint,
        api_key,
        model,
        parts,
        body,
        requested_streaming,
    } = request;
    let started = Instant::now();
    let path = parts.uri.path().to_owned();
    let request_id = request_id();
    let provider_defaults_apply = provider.request_defaults_apply_to(&endpoint.id);
    let empty_headers = std::collections::HashMap::new();
    let empty_body = serde_json::Map::new();
    let request_headers = sanitize_request_headers(
        parts.headers,
        endpoint.api_type,
        api_key.as_ref(),
        if provider_defaults_apply {
            &provider.extra_headers
        } else {
            &empty_headers
        },
        &endpoint.extra_headers,
    );
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    let target = join_upstream_url(&endpoint.base_url, path_and_query);
    let body = apply_extra_body(
        body,
        if provider_defaults_apply {
            &provider.extra_body
        } else {
            &empty_body
        },
        &endpoint.extra_body,
    );

    let client = match endpoint.client(&state.client) {
        Ok(client) => client,
        Err(err) => {
            error!(provider = %provider.id, endpoint = %endpoint.id, %err, "could not configure endpoint client");
            return api_error(StatusCode::BAD_GATEWAY, err);
        }
    };
    let mut upstream = client.request(to_reqwest_method(&parts.method), target);
    upstream = upstream.headers(request_headers).body(body);

    let upstream_response = match upstream.send().await {
        Ok(response) => response,
        Err(err) => {
            error!(provider = %provider.id, endpoint = %endpoint.id, %err, "upstream request failed");
            return api_error(StatusCode::BAD_GATEWAY, "Upstream request failed");
        }
    };

    let status = upstream_response.status();
    let response_headers = upstream_response.headers().clone();
    let content_type = response_headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let event_stream = content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
    let activity = state.activity.clone();
    let provider_id = provider.id.clone();
    let endpoint_id = endpoint.id.clone();
    let api_type = endpoint.api_type;
    let stream = async_stream::stream! {
        let mut upstream = upstream_response.bytes_stream();
        let mut usage = UsageTracker::new(api_type, event_stream);
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(chunk) => {
                    usage.observe(&chunk);
                    yield Ok::<bytes::Bytes, std::io::Error>(chunk);
                }
                Err(err) => { yield Err(std::io::Error::other(err.to_string())); break; }
            }
        }
        let usage = usage.finish();
        activity.record(RequestLog { timestamp: crate::auth::now(), request_id, source_instance_id: None, path, model, provider: provider_id, endpoint: endpoint_id, status: status.as_u16(), latency_ms: started.elapsed().as_millis() as u64, input_tokens: usage.input, output_tokens: usage.output, cached_tokens: usage.cached, cost: usage.cost, streaming: requested_streaming || event_stream }).await;
    };
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
    provider_headers: &std::collections::HashMap<String, String>,
    endpoint_headers: &std::collections::HashMap<String, String>,
) -> HeaderMap {
    for name in [
        header::HOST.as_str(),
        header::AUTHORIZATION.as_str(),
        "x-api-key",
        header::CONTENT_LENGTH.as_str(),
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
    for (name, value) in provider_headers.iter().chain(endpoint_headers) {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
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

fn apply_extra_body(
    body: Vec<u8>,
    provider: &serde_json::Map<String, serde_json::Value>,
    endpoint: &serde_json::Map<String, serde_json::Value>,
) -> Vec<u8> {
    if provider.is_empty() && endpoint.is_empty() {
        return body;
    }
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    if let Some(object) = value.as_object_mut() {
        for (key, value) in provider.iter().chain(endpoint) {
            object.insert(key.clone(), value.clone());
        }
    }
    serde_json::to_vec(&value).expect("serialize request with extra body")
}

fn copy_response_headers(target: &mut HeaderMap, source: &reqwest::header::HeaderMap) {
    for (name, value) in source {
        if is_hop_by_hop_header(name.as_str()) || name == reqwest::header::CONTENT_LENGTH {
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

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn to_reqwest_method(method: &Method) -> reqwest::Method {
    reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST)
}

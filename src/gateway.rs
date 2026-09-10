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
    activity::{ActivityStore, RequestLog},
    auth,
    config::{ApiEndpoint, ApiKey, ApiType, AppState, Provider},
    error::api_error,
    openai_subscription,
    protocol::{self, Protocol},
    protocol_stream::StreamConverter,
    usage::{TokenUsage, UsageTracker},
};

const MAX_REQUEST_BODY_SIZE: usize = 32 * 1024 * 1024;

#[derive(Debug)]
struct RoutingError {
    status: StatusCode,
    message: String,
}

#[derive(Clone, Copy)]
enum ApiSurface {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
}

impl ApiSurface {
    fn protocol(self) -> Protocol {
        match self {
            Self::OpenAiChat => Protocol::OpenAiChat,
            Self::OpenAiResponses => Protocol::OpenAiResponses,
            Self::Anthropic => Protocol::AnthropicMessages,
        }
    }

    fn supports(self, api_type: ApiType) -> bool {
        matches!(
            (self, api_type),
            (
                Self::OpenAiChat,
                ApiType::OpenaiCompatible | ApiType::OpenaiChatCompletions
            ) | (
                Self::OpenAiResponses,
                ApiType::OpenaiCompatible | ApiType::OpenaiResponses | ApiType::OpenaiCodex
            ) | (Self::Anthropic, ApiType::Anthropic)
        )
    }

    fn upstream_protocol(self, api_type: ApiType) -> Protocol {
        match api_type {
            ApiType::Anthropic => Protocol::AnthropicMessages,
            ApiType::OpenaiCodex | ApiType::OpenaiResponses => Protocol::OpenAiResponses,
            ApiType::OpenaiChatCompletions => Protocol::OpenAiChat,
            ApiType::OpenaiCompatible => match self {
                Self::OpenAiResponses => Protocol::OpenAiResponses,
                Self::OpenAiChat | Self::Anthropic => Protocol::OpenAiChat,
            },
        }
    }
}

pub async fn proxy_openai(State(state): State<AppState>, request: Request) -> Response {
    let surface = if request.uri().path() == "/v1/responses" {
        ApiSurface::OpenAiResponses
    } else {
        ApiSurface::OpenAiChat
    };
    route_request(state, request, surface).await
}

pub async fn proxy_anthropic(State(state): State<AppState>, request: Request) -> Response {
    route_request(state, request, ApiSurface::Anthropic).await
}

async fn route_request(state: AppState, request: Request, surface: ApiSurface) -> Response {
    let request_started = Instant::now();
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
        match resolve_provider(&state, &body, surface, allowed_providers.as_deref()).await {
            Ok(resolved) => resolved,
            Err(err) => return api_error(err.status, err.message),
        };

    let upstream_protocol = surface.upstream_protocol(endpoint.api_type);
    let body = match protocol::convert_request(&body, surface.protocol(), upstream_protocol) {
        Ok(body) => body,
        Err(err) => return api_error(StatusCode::BAD_REQUEST, err),
    };

    let endpoint = if endpoint.api_type == ApiType::OpenaiCodex {
        match openai_subscription::refreshed_endpoint(&state, &provider.id, &endpoint.id).await {
            Ok(endpoint) => endpoint,
            Err(err) => {
                error!(provider = %provider.id, endpoint = %endpoint.id, %err, "OpenAI subscription authentication failed");
                return api_error(StatusCode::BAD_GATEWAY, err);
            }
        }
    } else {
        endpoint
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
            request_started,
            caller_protocol: surface.protocol(),
            upstream_protocol,
        },
    )
    .await
}

async fn resolve_provider(
    state: &AppState,
    body: &[u8],
    surface: ApiSurface,
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
        provider
            .endpoints
            .iter()
            .find(|endpoint| {
                surface.supports(endpoint.api_type)
                    && provider.preferred_endpoint_id(upstream_model, endpoint.api_type)
                        == Some(endpoint.id.as_str())
                    && discovered.is_none_or(|endpoint_ids| endpoint_ids.contains(&endpoint.id))
            })
            .or_else(|| {
                provider.endpoints.iter().find(|endpoint| {
                    surface.supports(endpoint.api_type)
                        && discovered.is_none_or(|endpoint_ids| endpoint_ids.contains(&endpoint.id))
                })
            })
            .or_else(|| {
                provider.endpoints.iter().find(|endpoint| {
                    discovered.is_none_or(|endpoint_ids| endpoint_ids.contains(&endpoint.id))
                })
            })
    }
    .ok_or_else(|| RoutingError {
        status: StatusCode::BAD_REQUEST,
        message: format!("Provider '{provider_id}' has no endpoint for model '{upstream_model}'"),
    })?;
    let api_key = if endpoint.api_type == ApiType::OpenaiCodex {
        None
    } else if let Some(target) = &route_target {
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

struct ProxyActivity {
    store: ActivityStore,
    request_id: String,
    path: String,
    model: String,
    provider: String,
    endpoint: String,
    caller_protocol: Protocol,
    upstream_protocol: Protocol,
    started: Instant,
    gateway_ms: u64,
    upstream_response_ms: u64,
}

impl ProxyActivity {
    async fn record(
        &self,
        status: StatusCode,
        usage: TokenUsage,
        streaming: bool,
        first_byte_ms: Option<u64>,
    ) {
        let latency_ms = self.started.elapsed().as_millis() as u64;
        self.record_at(status, usage, streaming, first_byte_ms, latency_ms)
            .await;
    }

    async fn record_at(
        &self,
        status: StatusCode,
        usage: TokenUsage,
        streaming: bool,
        first_byte_ms: Option<u64>,
        latency_ms: u64,
    ) {
        self.store
            .record(RequestLog {
                timestamp: crate::auth::now(),
                request_id: self.request_id.clone(),
                source_instance_id: None,
                path: self.path.clone(),
                model: self.model.clone(),
                provider: self.provider.clone(),
                endpoint: self.endpoint.clone(),
                caller_protocol: Some(self.caller_protocol.name().to_owned()),
                upstream_protocol: Some(self.upstream_protocol.name().to_owned()),
                status: status.as_u16(),
                latency_ms,
                gateway_ms: Some(self.gateway_ms),
                upstream_response_ms: Some(self.upstream_response_ms),
                first_byte_ms,
                generation_ms: first_byte_ms
                    .map(|first_byte_ms| latency_ms.saturating_sub(first_byte_ms)),
                input_tokens: usage.input,
                output_tokens: usage.output,
                cached_tokens: usage.cached,
                cost: usage.cost,
                streaming,
            })
            .await;
    }
}

struct ForwardRequest {
    provider: Provider,
    endpoint: ApiEndpoint,
    api_key: Option<ApiKey>,
    model: String,
    parts: axum::http::request::Parts,
    body: Vec<u8>,
    requested_streaming: bool,
    request_started: Instant,
    caller_protocol: Protocol,
    upstream_protocol: Protocol,
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
        request_started: started,
        caller_protocol,
        upstream_protocol,
    } = request;
    let path = parts.uri.path().to_owned();
    let request_id = request_id();
    let provider_defaults_apply = provider.request_defaults_apply_to(&endpoint.id);
    let empty_headers = std::collections::HashMap::new();
    let empty_body = serde_json::Map::new();
    let request_headers = sanitize_request_headers(
        parts.headers,
        endpoint.api_type,
        (caller_protocol, upstream_protocol),
        api_key.as_ref(),
        endpoint.openai_subscription.as_ref(),
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
    let target_path = match upstream_protocol {
        Protocol::OpenAiChat => "/v1/chat/completions",
        Protocol::OpenAiResponses => "/v1/responses",
        Protocol::AnthropicMessages => "/v1/messages",
    };
    let target = if endpoint.api_type == ApiType::OpenaiCodex {
        join_upstream_url(&endpoint.base_url, "/codex/responses")
    } else if caller_protocol == upstream_protocol {
        join_upstream_url(&endpoint.base_url, path_and_query)
    } else {
        join_upstream_url(&endpoint.base_url, target_path)
    };
    let body = apply_extra_body(
        body,
        if provider_defaults_apply {
            &provider.extra_body
        } else {
            &empty_body
        },
        &endpoint.extra_body,
    );
    let body = if endpoint.api_type == ApiType::OpenaiCodex {
        apply_codex_body(body)
    } else {
        body
    };

    let client = match endpoint.client(&state.client) {
        Ok(client) => client,
        Err(err) => {
            error!(provider = %provider.id, endpoint = %endpoint.id, %err, "could not configure endpoint client");
            return api_error(StatusCode::BAD_GATEWAY, err);
        }
    };
    let mut upstream = client.request(to_reqwest_method(&parts.method), target);
    upstream = upstream.headers(request_headers).body(body);

    let upstream_started = Instant::now();
    let upstream_response = match upstream.send().await {
        Ok(response) => response,
        Err(err) => {
            error!(provider = %provider.id, endpoint = %endpoint.id, %err, "upstream request failed");
            let failed_at_ms = started.elapsed().as_millis() as u64;
            state
                .activity
                .record(RequestLog {
                    timestamp: crate::auth::now(),
                    request_id,
                    source_instance_id: None,
                    path,
                    model,
                    provider: provider.id.clone(),
                    endpoint: endpoint.id.clone(),
                    caller_protocol: Some(caller_protocol.name().to_owned()),
                    upstream_protocol: Some(upstream_protocol.name().to_owned()),
                    status: StatusCode::BAD_GATEWAY.as_u16(),
                    latency_ms: failed_at_ms,
                    gateway_ms: Some(upstream_started.duration_since(started).as_millis() as u64),
                    upstream_response_ms: Some(upstream_started.elapsed().as_millis() as u64),
                    first_byte_ms: None,
                    generation_ms: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    cached_tokens: 0,
                    cost: None,
                    streaming: requested_streaming,
                })
                .await;
            return api_error(StatusCode::BAD_GATEWAY, "Upstream request failed");
        }
    };

    let upstream_response_ms = upstream_started.elapsed().as_millis() as u64;
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
    let activity = ProxyActivity {
        store: state.activity.clone(),
        request_id,
        path,
        model,
        provider: provider.id.clone(),
        endpoint: endpoint.id.clone(),
        caller_protocol,
        upstream_protocol,
        started,
        gateway_ms: upstream_started.duration_since(started).as_millis() as u64,
        upstream_response_ms,
    };
    let api_type = endpoint.api_type;
    let converting = status.is_success() && caller_protocol != upstream_protocol;

    if converting && event_stream && !requested_streaming {
        let mut upstream = upstream_response.bytes_stream();
        let mut usage = UsageTracker::new(api_type, true);
        let mut converter = StreamConverter::new_aggregating(upstream_protocol, caller_protocol);
        let mut failure = None;
        let mut first_byte_ms = None;
        while let Some(chunk) = upstream.next().await {
            let chunk = match chunk {
                Ok(chunk) => {
                    first_byte_ms.get_or_insert_with(|| started.elapsed().as_millis() as u64);
                    chunk
                }
                Err(err) => {
                    error!(provider = %provider.id, endpoint = %endpoint.id, %err, "could not read upstream stream for protocol conversion");
                    failure = Some("Upstream stream failed".to_owned());
                    break;
                }
            };
            usage.observe(&chunk);
            if let Err(err) = converter.push(&chunk) {
                error!(provider = %provider.id, endpoint = %endpoint.id, %err, "could not convert upstream stream");
                failure = Some(err);
                break;
            }
        }
        if failure.is_none()
            && let Err(err) = converter.finish()
        {
            failure = Some(err);
        }
        let completion_ms = started.elapsed().as_millis() as u64;
        let converted = if failure.is_none() {
            match converter.non_stream_response() {
                Ok(converted) => Some(converted),
                Err(err) => {
                    failure = Some(err);
                    None
                }
            }
        } else {
            None
        };
        let usage = usage.finish();
        if let Some(err) = failure {
            activity
                .record_at(
                    StatusCode::BAD_GATEWAY,
                    usage,
                    true,
                    first_byte_ms,
                    completion_ms,
                )
                .await;
            return api_error(StatusCode::BAD_GATEWAY, err);
        }
        activity
            .record_at(status, usage, true, first_byte_ms, completion_ms)
            .await;
        let mut response = Response::new(Body::from(converted.expect("successful conversion")));
        *response.status_mut() = status;
        copy_response_headers(response.headers_mut(), &response_headers);
        strip_transformed_response_headers(response.headers_mut());
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        set_conversion_header(response.headers_mut(), upstream_protocol, caller_protocol);
        return response;
    }

    if converting && !event_stream {
        let bytes = match upstream_response.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                error!(provider = %provider.id, endpoint = %endpoint.id, %err, "could not read upstream response for protocol conversion");
                activity
                    .record(StatusCode::BAD_GATEWAY, TokenUsage::default(), false, None)
                    .await;
                return api_error(StatusCode::BAD_GATEWAY, "Could not read upstream response");
            }
        };
        let mut usage = UsageTracker::new(api_type, false);
        usage.observe(&bytes);
        let converted = protocol::convert_response(&bytes, upstream_protocol, caller_protocol);
        let usage = usage.finish();
        let converted = match converted {
            Ok(converted) => converted,
            Err(err) => {
                error!(provider = %provider.id, endpoint = %endpoint.id, %err, "could not convert upstream response");
                activity
                    .record(StatusCode::BAD_GATEWAY, usage, requested_streaming, None)
                    .await;
                return api_error(StatusCode::BAD_GATEWAY, err);
            }
        };
        activity
            .record(status, usage, requested_streaming, None)
            .await;
        let mut response = Response::new(Body::from(converted));
        *response.status_mut() = status;
        copy_response_headers(response.headers_mut(), &response_headers);
        strip_transformed_response_headers(response.headers_mut());
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        set_conversion_header(response.headers_mut(), upstream_protocol, caller_protocol);
        return response;
    }

    let stream = async_stream::stream! {
        let mut upstream = upstream_response.bytes_stream();
        let mut usage = UsageTracker::new(api_type, event_stream);
        let mut converter = converting.then(|| StreamConverter::new(upstream_protocol, caller_protocol));
        let mut conversion_failed = false;
        let mut first_byte_ms = None;
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(chunk) => {
                    first_byte_ms.get_or_insert_with(|| started.elapsed().as_millis() as u64);
                    usage.observe(&chunk);
                    if let Some(converter) = &mut converter {
                        match converter.push(&chunk) {
                            Ok(converted) => {
                                let failed = converter.has_failed();
                                if !converted.is_empty() {
                                    yield Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(converted));
                                }
                                if failed {
                                    conversion_failed = true;
                                    break;
                                }
                            }
                            Err(err) => {
                                conversion_failed = true;
                                yield Err(std::io::Error::other(err));
                                break;
                            }
                        }
                    } else {
                        yield Ok::<bytes::Bytes, std::io::Error>(chunk);
                    }
                }
                Err(err) => {
                    conversion_failed = true;
                    yield Err(std::io::Error::other(err.to_string()));
                    break;
                }
            }
        }
        if !conversion_failed && let Some(converter) = &mut converter {
            match converter.finish() {
                Ok(converted) => {
                    conversion_failed = converter.has_failed();
                    if !converted.is_empty() {
                        yield Ok(bytes::Bytes::from(converted));
                    }
                }
                Err(err) => {
                    conversion_failed = true;
                    yield Err(std::io::Error::other(err));
                }
            }
        }
        let usage = usage.finish();
        let recorded_status = if conversion_failed { StatusCode::BAD_GATEWAY } else { status };
        activity.record(recorded_status, usage, requested_streaming || event_stream, first_byte_ms).await;
    };
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    copy_response_headers(response.headers_mut(), &response_headers);
    if converting {
        strip_transformed_response_headers(response.headers_mut());
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        set_conversion_header(response.headers_mut(), upstream_protocol, caller_protocol);
    }
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
    protocol_route: (Protocol, Protocol),
    api_key: Option<&ApiKey>,
    subscription: Option<&crate::config::OpenAiSubscription>,
    provider_headers: &std::collections::HashMap<String, String>,
    endpoint_headers: &std::collections::HashMap<String, String>,
) -> HeaderMap {
    for name in [
        header::HOST.as_str(),
        header::AUTHORIZATION.as_str(),
        header::COOKIE.as_str(),
        "x-api-key",
        "chatgpt-account-id",
        "originator",
        "openai-beta",
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
    if protocol_route.0 != protocol_route.1 {
        for name in [
            "anthropic-beta",
            "anthropic-version",
            "openai-organization",
            "openai-project",
        ] {
            headers.remove(name);
        }
    }
    if api_type == ApiType::OpenaiCodex {
        headers.remove(header::CONTENT_ENCODING);
    }
    for (name, value) in provider_headers.iter().chain(endpoint_headers) {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    if protocol_route.0 != protocol_route.1 {
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
    }
    if let Some(api_key) = api_key {
        let (name, value) = match api_type {
            ApiType::OpenaiCompatible
            | ApiType::OpenaiChatCompletions
            | ApiType::OpenaiResponses
            | ApiType::OpenaiCodex => (
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
    if api_type == ApiType::Anthropic {
        headers
            .entry(HeaderName::from_static("anthropic-version"))
            .or_insert(HeaderValue::from_static("2023-06-01"));
    }
    if let Some(subscription) = subscription {
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", subscription.access_token))
                .expect("validated OpenAI access token header value"),
        );
        headers.insert(
            HeaderName::from_static("chatgpt-account-id"),
            HeaderValue::from_str(&subscription.account_id)
                .expect("validated ChatGPT account ID header value"),
        );
        headers.insert(
            HeaderName::from_static("originator"),
            HeaderValue::from_static("yabane"),
        );
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_static(concat!("yabane/", env!("YABANE_GIT_COMMIT"))),
        );
        headers.insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("responses=experimental"),
        );
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    headers
}

fn apply_codex_body(body: Vec<u8>) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("store".to_owned(), serde_json::Value::Bool(false));
        object.insert("stream".to_owned(), serde_json::Value::Bool(true));
        if object
            .get("instructions")
            .and_then(serde_json::Value::as_str)
            .is_none()
        {
            object.insert(
                "instructions".to_owned(),
                serde_json::Value::String("You are a helpful assistant.".to_owned()),
            );
        }
        if let Some(input) = object
            .get("input")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        {
            object.insert(
                "input".to_owned(),
                serde_json::json!([{"role": "user", "content": [{"type": "input_text", "text": input}]}]),
            );
        }
        object
            .entry("text")
            .or_insert_with(|| serde_json::json!({"verbosity": "low"}));
        object
            .entry("tool_choice")
            .or_insert_with(|| serde_json::Value::String("auto".to_owned()));
        object
            .entry("parallel_tool_calls")
            .or_insert(serde_json::Value::Bool(true));
        let include = object
            .entry("include")
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        if !include.is_array() {
            *include = serde_json::Value::Array(Vec::new());
        }
        let items = include.as_array_mut().expect("normalized include array");
        if !items
            .iter()
            .any(|item| item.as_str() == Some("reasoning.encrypted_content"))
        {
            items.push(serde_json::Value::String(
                "reasoning.encrypted_content".to_owned(),
            ));
        }
    }
    serde_json::to_vec(&value).expect("serialize OpenAI subscription request")
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

fn set_conversion_header(headers: &mut HeaderMap, source: Protocol, target: Protocol) {
    let value = format!("{}->{}", source.name(), target.name());
    if let Ok(value) = HeaderValue::from_str(&value) {
        headers.insert(
            HeaderName::from_static("x-yabane-protocol-conversion"),
            value,
        );
    }
}

fn copy_response_headers(target: &mut HeaderMap, source: &reqwest::header::HeaderMap) {
    for (name, value) in source {
        if is_hop_by_hop_header(name.as_str())
            || name == reqwest::header::CONTENT_LENGTH
            || name == reqwest::header::SET_COOKIE
        {
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

fn strip_transformed_response_headers(headers: &mut HeaderMap) {
    for name in [
        header::CONTENT_ENCODING.as_str(),
        header::CONTENT_RANGE.as_str(),
        header::ETAG.as_str(),
        "content-digest",
        "digest",
    ] {
        headers.remove(name);
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::http::{HeaderMap, HeaderValue};

    use super::{
        ApiSurface, ApiType, Protocol, apply_codex_body, copy_response_headers,
        sanitize_request_headers, strip_transformed_response_headers,
    };
    use crate::config::OpenAiSubscription;

    #[test]
    fn codex_adapter_applies_required_response_fields() {
        let body = apply_codex_body(
            br#"{"model":"gpt-5.4","input":"hello","stream":true,"store":true}"#.to_vec(),
        );
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["stream"], true);
        assert_eq!(value["store"], false);
        assert_eq!(value["instructions"], "You are a helpful assistant.");
        assert_eq!(value["input"][0]["role"], "user");
        assert_eq!(value["input"][0]["content"][0]["text"], "hello");
        assert_eq!(value["text"]["verbosity"], "low");
        assert_eq!(value["tool_choice"], "auto");
        assert_eq!(value["parallel_tool_calls"], true);
        assert_eq!(value["include"][0], "reasoning.encrypted_content");
    }

    #[test]
    fn subscription_uses_responses_upstream_for_every_caller_surface() {
        assert_eq!(
            ApiSurface::OpenAiResponses.upstream_protocol(ApiType::OpenaiCodex),
            Protocol::OpenAiResponses
        );
        assert_eq!(
            ApiSurface::OpenAiChat.upstream_protocol(ApiType::OpenaiCodex),
            Protocol::OpenAiResponses
        );
        assert_eq!(
            ApiSurface::Anthropic.upstream_protocol(ApiType::OpenaiCodex),
            Protocol::OpenAiResponses
        );
    }

    #[test]
    fn proxy_does_not_forward_caller_cookies_or_upstream_set_cookies() {
        let mut request_headers = HeaderMap::new();
        request_headers.insert("cookie", HeaderValue::from_static("yabane_session=private"));
        let sanitized = sanitize_request_headers(
            request_headers,
            ApiType::OpenaiCompatible,
            (Protocol::OpenAiChat, Protocol::OpenAiChat),
            None,
            None,
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(!sanitized.contains_key("cookie"));

        let mut upstream_headers = reqwest::header::HeaderMap::new();
        upstream_headers.insert(
            reqwest::header::SET_COOKIE,
            reqwest::header::HeaderValue::from_static("yabane_session=attacker"),
        );
        upstream_headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        upstream_headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("gzip"),
        );
        upstream_headers.insert(
            reqwest::header::ETAG,
            reqwest::header::HeaderValue::from_static("\"upstream-body\""),
        );
        let mut response_headers = HeaderMap::new();
        copy_response_headers(&mut response_headers, &upstream_headers);
        assert!(!response_headers.contains_key("set-cookie"));
        assert_eq!(response_headers["content-type"], "application/json");
        assert_eq!(response_headers["content-encoding"], "gzip");
        strip_transformed_response_headers(&mut response_headers);
        assert!(!response_headers.contains_key("content-encoding"));
        assert!(!response_headers.contains_key("etag"));
    }

    #[test]
    fn cross_protocol_headers_do_not_leak_caller_protocol_context() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "openai-organization",
            HeaderValue::from_static("org-caller"),
        );
        headers.insert("openai-project", HeaderValue::from_static("proj-caller"));
        headers.insert("anthropic-beta", HeaderValue::from_static("caller-beta"));
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static("caller-version"),
        );
        let endpoint_headers =
            HashMap::from([("anthropic-beta".to_owned(), "endpoint-beta".to_owned())]);

        let sanitized = sanitize_request_headers(
            headers,
            ApiType::Anthropic,
            (Protocol::OpenAiChat, Protocol::AnthropicMessages),
            None,
            None,
            &HashMap::new(),
            &endpoint_headers,
        );

        assert_eq!(sanitized["accept-encoding"], "identity");
        assert!(!sanitized.contains_key("openai-organization"));
        assert!(!sanitized.contains_key("openai-project"));
        assert_eq!(sanitized["anthropic-version"], "2023-06-01");
        assert_eq!(sanitized["anthropic-beta"], "endpoint-beta");
    }

    #[test]
    fn subscription_headers_replace_caller_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer caller"));
        headers.insert(
            "chatgpt-account-id",
            HeaderValue::from_static("caller-account"),
        );
        headers.insert("content-encoding", HeaderValue::from_static("zstd"));
        let credential = OpenAiSubscription {
            access_token: "access-token".to_owned(),
            refresh_token: "refresh-token".to_owned(),
            expires_at: u64::MAX,
            account_id: "account-123".to_owned(),
        };
        let sanitized = sanitize_request_headers(
            headers,
            ApiType::OpenaiCodex,
            (Protocol::OpenAiResponses, Protocol::OpenAiResponses),
            None,
            Some(&credential),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(sanitized["authorization"], "Bearer access-token");
        assert_eq!(sanitized["chatgpt-account-id"], "account-123");
        assert_eq!(sanitized["originator"], "yabane");
        assert_eq!(sanitized["openai-beta"], "responses=experimental");
        assert!(!sanitized.contains_key("content-encoding"));
    }
}

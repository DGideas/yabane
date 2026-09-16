use std::{error::Error as StdError, time::Instant};

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
    activity::{ActivityStore, RequestFailure, RequestLog},
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
    let allowed_providers = auth::authorized_provider_ids(&request).map(<[String]>::to_vec);
    let (parts, body) = request.into_parts();
    let body = match axum::body::to_bytes(body, MAX_REQUEST_BODY_SIZE).await {
        Ok(body) => body,
        Err(_) => return api_error(StatusCode::PAYLOAD_TOO_LARGE, "Request body is too large"),
    };
    let requested_streaming = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.get("stream").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    let (provider, endpoint, api_key, model, upstream_model, body) =
        match resolve_provider(&state, &body, surface, allowed_providers.as_deref()).await {
            Ok(resolved) => resolved,
            Err(err) => return api_error(err.status, err.message),
        };

    let upstream_protocol = if endpoint.api_type == ApiType::OpenaiCodex {
        match state.extensions.provider_endpoint("openai_codex") {
            Some(implementation) => {
                protocol_from_extension(implementation.endpoint_type().upstream_protocol)
            }
            None => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "OpenAI Subscription Extension is not enabled",
                );
            }
        }
    } else {
        surface.upstream_protocol(endpoint.api_type)
    };
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
            upstream_model,
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
) -> Result<
    (
        Provider,
        ApiEndpoint,
        Option<ApiKey>,
        String,
        String,
        Vec<u8>,
    ),
    RoutingError,
> {
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
    let api_key = if endpoint.api_type == ApiType::OpenaiCodex || !endpoint.requires_api_key {
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
    let upstream_model = upstream_model.to_owned();
    payload["model"] = serde_json::Value::String(upstream_model.clone());
    let body = serde_json::to_vec(&payload).expect("serialize validated request body");
    Ok((provider, endpoint, api_key, model, upstream_model, body))
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
    upstream_model: Option<String>,
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
        self.record_failure_at(status, usage, streaming, first_byte_ms, latency_ms, None)
            .await;
    }

    async fn record_failure(
        &self,
        status: StatusCode,
        usage: TokenUsage,
        streaming: bool,
        first_byte_ms: Option<u64>,
        failure: RequestFailure,
    ) {
        let latency_ms = self.started.elapsed().as_millis() as u64;
        self.record_failure_at(
            status,
            usage,
            streaming,
            first_byte_ms,
            latency_ms,
            Some(failure),
        )
        .await;
    }

    async fn record_failure_at(
        &self,
        status: StatusCode,
        usage: TokenUsage,
        streaming: bool,
        first_byte_ms: Option<u64>,
        latency_ms: u64,
        failure: Option<RequestFailure>,
    ) {
        self.store
            .record(RequestLog {
                timestamp: crate::auth::now(),
                request_id: self.request_id.clone(),
                source_instance_id: None,
                path: self.path.clone(),
                model: self.model.clone(),
                upstream_model: self.upstream_model.clone(),
                provider: self.provider.clone(),
                endpoint: self.endpoint.clone(),
                caller_protocol: Some(self.caller_protocol.name().to_owned()),
                upstream_protocol: Some(self.upstream_protocol.name().to_owned()),
                status: status.as_u16(),
                failure,
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
    upstream_model: String,
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
        upstream_model: resolved_upstream_model,
        parts,
        body,
        requested_streaming,
        request_started: started,
        caller_protocol,
        upstream_protocol,
    } = request;
    let path = parts.uri.path().to_owned();
    let request_id = request_id();
    let upstream_model = resolved_upstream_model;
    #[cfg(feature = "extension-request-defaults")]
    let provider_defaults_apply = provider.request_defaults_apply_to(&endpoint.id);
    let extension_context = yabane_extension_api::RequestContext {
        request_id: &request_id,
        public_model: &model,
        upstream_model: &upstream_model,
        provider_id: &provider.id,
        endpoint_id: &endpoint.id,
        caller_protocol: extension_protocol(caller_protocol),
        upstream_protocol: extension_protocol(upstream_protocol),
        requested_streaming,
    };
    #[cfg(feature = "extension-request-defaults")]
    let request_defaults_enabled = state.extensions.is_enabled("request-defaults");
    #[cfg(feature = "extension-request-defaults")]
    let empty_headers = std::collections::HashMap::new();
    #[cfg(feature = "extension-request-defaults")]
    let empty_body = serde_json::Map::new();
    #[cfg(feature = "extension-request-defaults")]
    let request_defaults = yabane_extension_request_defaults::RequestDefaults::new(
        if provider_defaults_apply {
            &provider.extra_headers
        } else {
            &empty_headers
        },
        &endpoint.extra_headers,
        if provider_defaults_apply {
            &provider.extra_body
        } else {
            &empty_body
        },
        &endpoint.extra_body,
    );
    #[cfg_attr(
        not(any(
            feature = "extension-request-defaults",
            feature = "extension-traffic-capture"
        )),
        allow(unused_mut)
    )]
    let mut extension_hooks = crate::extensions::RequestHooks::default();
    #[cfg(feature = "extension-request-defaults")]
    if request_defaults_enabled && !request_defaults.is_empty() {
        extension_hooks.upstream_request.push(&request_defaults);
        extension_hooks.upstream_headers.push(&request_defaults);
    }
    #[cfg(feature = "extension-traffic-capture")]
    if state.extensions.is_enabled("traffic-capture") {
        extension_hooks
            .upstream_exchange
            .push(state.traffic_capture.as_ref());
    }
    let mut request_headers = sanitize_request_headers(
        parts.headers,
        endpoint.api_type,
        (caller_protocol, upstream_protocol),
    );
    if !extension_hooks.upstream_headers.is_empty() {
        let overlay = match state
            .extensions
            .run_upstream_headers(&extension_context, &extension_hooks.upstream_headers)
        {
            Ok(crate::extensions::DispatchOutcome::Continue(headers)) => headers,
            Ok(crate::extensions::DispatchOutcome::Reject(rejection)) => {
                return crate::extensions::rejection_response(rejection);
            }
            Err(failure) => return crate::extensions::execution_error(failure),
        };
        request_headers.extend(overlay);
    }
    apply_core_upstream_headers(
        &mut request_headers,
        endpoint.api_type,
        (caller_protocol, upstream_protocol),
        api_key.as_ref(),
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
    let mut target_path = if caller_protocol == upstream_protocol {
        path_and_query.to_owned()
    } else {
        target_path.to_owned()
    };
    let body = if extension_hooks.upstream_request.is_empty() {
        body
    } else {
        match state.extensions.run_upstream_request(
            &extension_context,
            bytes::Bytes::from(body),
            &extension_hooks.upstream_request,
        ) {
            Ok(crate::extensions::DispatchOutcome::Continue(body)) => body.to_vec(),
            Ok(crate::extensions::DispatchOutcome::Reject(rejection)) => {
                return crate::extensions::rejection_response(rejection);
            }
            Err(failure) => return crate::extensions::execution_error(failure),
        }
    };
    let mut body = body;
    if endpoint.api_type == ApiType::OpenaiCodex {
        let implementation = match state.extensions.provider_endpoint("openai_codex") {
            Some(implementation) => implementation,
            None => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "OpenAI Subscription Extension is not enabled",
                );
            }
        };
        let subscription = endpoint
            .openai_subscription
            .as_ref()
            .expect("refreshed subscription");
        if let Err(error) =
            implementation.prepare_request(yabane_extension_api::ProviderEndpointRequest {
                headers: &mut request_headers,
                body: &mut body,
                target_path: &mut target_path,
                credential: Some(yabane_extension_api::ProviderEndpointCredential {
                    access_token: &subscription.access_token,
                    account_id: &subscription.account_id,
                }),
            })
        {
            error!(provider = %provider.id, endpoint = %endpoint.id, %error, "OpenAI subscription request preparation failed");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "OpenAI Subscription Extension failed",
            );
        }
    }
    // Request extensions and Endpoint implementations may explicitly rewrite or
    // remove the routed model. Activity must describe the final wire request, not
    // merely the route target selected before those transformations ran.
    let sent_upstream_model = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.get("model")?.as_str().map(str::to_owned));
    let endpoint_implementation = if endpoint.api_type == ApiType::OpenaiCodex {
        state.extensions.provider_endpoint("openai_codex")
    } else {
        None
    };
    let target = join_upstream_url(
        provider_endpoint_base_url(&endpoint.base_url, endpoint_implementation),
        &target_path,
    );

    let interested_exchange_hooks = state
        .extensions
        .interested_upstream_exchange(&extension_context, &extension_hooks.upstream_exchange);
    let observed_request_headers = if interested_exchange_hooks.is_empty() {
        Vec::new()
    } else {
        observed_headers(&request_headers)
    };
    let mut exchange_observers = state.extensions.begin_upstream_exchange(
        &extension_context,
        yabane_extension_api::ObservedUpstreamRequest {
            headers: &observed_request_headers,
            body: &body,
        },
        &interested_exchange_hooks,
    );

    let client = match endpoint.client(&state.client) {
        Ok(client) => client,
        Err(err) => {
            complete_exchange_observers(
                &mut exchange_observers,
                yabane_extension_api::ExchangeOutcome::TransportError,
            );
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
            complete_exchange_observers(
                &mut exchange_observers,
                yabane_extension_api::ExchangeOutcome::TransportError,
            );
            let using_socks5_proxy = endpoint.socks5_proxy.is_some();
            let failure = upstream_transport_failure(&err, using_socks5_proxy);
            error!(
                provider = %provider.id,
                endpoint = %endpoint.id,
                request_id,
                using_socks5_proxy,
                failure_stage = %failure.stage,
                failure_category = %failure.category,
                failure = %failure.message,
                "upstream request failed"
            );
            let failed_at_ms = started.elapsed().as_millis() as u64;
            state
                .activity
                .record(RequestLog {
                    timestamp: crate::auth::now(),
                    request_id,
                    source_instance_id: None,
                    path,
                    model,
                    upstream_model: sent_upstream_model,
                    provider: provider.id.clone(),
                    endpoint: endpoint.id.clone(),
                    caller_protocol: Some(caller_protocol.name().to_owned()),
                    upstream_protocol: Some(upstream_protocol.name().to_owned()),
                    status: StatusCode::BAD_GATEWAY.as_u16(),
                    failure: Some(failure.clone()),
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
            return api_error(StatusCode::BAD_GATEWAY, &failure.message);
        }
    };

    let upstream_response_ms = upstream_started.elapsed().as_millis() as u64;
    let status = upstream_response.status();
    let api_type = endpoint.api_type;
    let response_headers = upstream_response.headers().clone();
    if !exchange_observers.is_empty() {
        let observed_response_headers = observed_headers(&response_headers);
        observe_response_head(&mut exchange_observers, status, &observed_response_headers);
    }
    let content_type = response_headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    // The ChatGPT Codex endpoint is always SSE, but currently omits Content-Type
    // on successful responses. pi-ai parses it as SSE by protocol, not by header.
    let event_stream = response_is_event_stream(
        status,
        api_type,
        state
            .extensions
            .provider_endpoint("openai_codex")
            .is_some_and(|implementation| implementation.endpoint_type().always_event_stream),
        content_type,
    );
    let activity = ProxyActivity {
        store: state.activity.clone(),
        request_id,
        path,
        model,
        upstream_model: sent_upstream_model,
        provider: provider.id.clone(),
        endpoint: endpoint.id.clone(),
        caller_protocol,
        upstream_protocol,
        started,
        gateway_ms: upstream_started.duration_since(started).as_millis() as u64,
        upstream_response_ms,
    };
    let converting = status.is_success() && caller_protocol != upstream_protocol;

    // Some upstreams, including ChatGPT Codex subscriptions, require SSE even
    // when a native Responses caller requested one non-streaming response.
    if event_stream && !requested_streaming && (converting || api_type == ApiType::OpenaiCodex) {
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
            observe_response_chunk(&mut exchange_observers, &chunk);
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
        let (usage, protocol_failed) = usage.finish();
        if failure.is_none() && protocol_failed {
            failure = Some("Upstream stream reported a failure".to_owned());
        }
        if let Some(err) = failure {
            activity
                .record_failure_at(
                    StatusCode::BAD_GATEWAY,
                    usage,
                    true,
                    first_byte_ms,
                    completion_ms,
                    Some(stream_failure(&err)),
                )
                .await;
            complete_exchange_observers(
                &mut exchange_observers,
                yabane_extension_api::ExchangeOutcome::Interrupted,
            );
            return api_error(StatusCode::BAD_GATEWAY, err);
        }
        complete_exchange_observers(
            &mut exchange_observers,
            yabane_extension_api::ExchangeOutcome::Complete,
        );
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
        if converting {
            set_conversion_header(response.headers_mut(), upstream_protocol, caller_protocol);
        }
        return response;
    }

    if converting && !event_stream {
        let bytes = match upstream_response.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                error!(provider = %provider.id, endpoint = %endpoint.id, %err, "could not read upstream response for protocol conversion");
                activity
                    .record_failure(
                        StatusCode::BAD_GATEWAY,
                        TokenUsage::default(),
                        false,
                        None,
                        RequestFailure::new(
                            "upstream_response",
                            "read_failed",
                            "Could not read the upstream response",
                        ),
                    )
                    .await;
                complete_exchange_observers(
                    &mut exchange_observers,
                    yabane_extension_api::ExchangeOutcome::ResponseReadError,
                );
                return api_error(StatusCode::BAD_GATEWAY, "Could not read upstream response");
            }
        };
        observe_response_chunk(&mut exchange_observers, &bytes);
        let mut usage = UsageTracker::new(api_type, false);
        usage.observe(&bytes);
        let converted = protocol::convert_response(&bytes, upstream_protocol, caller_protocol);
        let (usage, protocol_failed) = usage.finish();
        let converted = match converted {
            Ok(converted) => converted,
            Err(err) => {
                error!(provider = %provider.id, endpoint = %endpoint.id, %err, "could not convert upstream response");
                activity
                    .record_failure(
                        StatusCode::BAD_GATEWAY,
                        usage,
                        requested_streaming,
                        None,
                        RequestFailure::new(
                            "protocol_conversion",
                            "invalid_response",
                            "Could not convert the upstream response",
                        ),
                    )
                    .await;
                complete_exchange_observers(
                    &mut exchange_observers,
                    yabane_extension_api::ExchangeOutcome::Complete,
                );
                return api_error(StatusCode::BAD_GATEWAY, err);
            }
        };
        if protocol_failed || status.is_client_error() || status.is_server_error() {
            let failure = if protocol_failed {
                RequestFailure::new(
                    "upstream_response",
                    "protocol_failure",
                    "Upstream response reported a failure",
                )
            } else {
                upstream_http_failure(status)
            };
            activity
                .record_failure(status, usage, requested_streaming, None, failure)
                .await;
        } else {
            activity
                .record(status, usage, requested_streaming, None)
                .await;
        }
        complete_exchange_observers(
            &mut exchange_observers,
            yabane_extension_api::ExchangeOutcome::Complete,
        );
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
        let mut exchange_observers = exchange_observers;
        let mut upstream = upstream_response.bytes_stream();
        let mut usage = UsageTracker::new(api_type, event_stream);
        let mut converter = converting.then(|| StreamConverter::new(upstream_protocol, caller_protocol));
        let mut conversion_failed = false;
        let mut first_byte_ms = None;
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(chunk) => {
                    first_byte_ms.get_or_insert_with(|| started.elapsed().as_millis() as u64);
                    observe_response_chunk(&mut exchange_observers, &chunk);
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
        let exchange_outcome = if conversion_failed {
            yabane_extension_api::ExchangeOutcome::Interrupted
        } else {
            yabane_extension_api::ExchangeOutcome::Complete
        };
        complete_exchange_observers(&mut exchange_observers, exchange_outcome);
        let (usage, protocol_failed) = usage.finish();
        let recorded_status = if conversion_failed || protocol_failed { StatusCode::BAD_GATEWAY } else { status };
        if conversion_failed {
            activity.record_failure(
                recorded_status,
                usage,
                requested_streaming || event_stream,
                first_byte_ms,
                RequestFailure::new(
                    "upstream_stream",
                    "interrupted",
                    "Upstream stream ended or could not be converted",
                ),
            ).await;
        } else if protocol_failed {
            activity.record_failure(
                recorded_status,
                usage,
                requested_streaming || event_stream,
                first_byte_ms,
                RequestFailure::new(
                    "upstream_stream",
                    "protocol_failure",
                    "Upstream stream reported a failure",
                ),
            ).await;
        } else if status.is_client_error() || status.is_server_error() {
            activity.record_failure(
                status,
                usage,
                requested_streaming || event_stream,
                first_byte_ms,
                upstream_http_failure(status),
            ).await;
        } else {
            activity.record(status, usage, requested_streaming || event_stream, first_byte_ms).await;
        }
    };
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    copy_response_headers(response.headers_mut(), &response_headers);
    if api_type == ApiType::OpenaiCodex {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
    }
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

fn observed_headers(
    headers: &reqwest::header::HeaderMap,
) -> Vec<yabane_extension_api::ObservedHeader> {
    headers
        .iter()
        .map(|(name, value)| yabane_extension_api::ObservedHeader {
            name: name.as_str().to_owned(),
            value: if sensitive_observed_header(name.as_str()) {
                "[REDACTED]".to_owned()
            } else {
                value.to_str().unwrap_or("[NON-UTF8]").to_owned()
            },
        })
        .collect()
}

fn sensitive_observed_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "proxy-authorization"
            | "proxy-authenticate"
            | "x-api-key"
            | "cookie"
            | "set-cookie"
            | "chatgpt-account-id"
            | "session-id"
            | "x-client-request-id"
    )
}

fn observe_response_head(
    observers: &mut [Box<dyn yabane_extension_api::UpstreamExchangeObserver>],
    status: StatusCode,
    headers: &[yabane_extension_api::ObservedHeader],
) {
    for observer in observers {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            observer.on_response_head(yabane_extension_api::ObservedUpstreamResponseHead {
                status,
                headers,
            });
        }));
    }
}

fn observe_response_chunk(
    observers: &mut [Box<dyn yabane_extension_api::UpstreamExchangeObserver>],
    chunk: &bytes::Bytes,
) {
    for observer in observers {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            observer.on_response_chunk(chunk);
        }));
    }
}

fn complete_exchange_observers(
    observers: &mut Vec<Box<dyn yabane_extension_api::UpstreamExchangeObserver>>,
    outcome: yabane_extension_api::ExchangeOutcome,
) {
    for mut observer in observers.drain(..) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            observer.on_complete(outcome);
        }));
    }
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
        headers.remove(header::USER_AGENT);
        headers.remove("session-id");
        headers.remove("x-client-request-id");
    }
    headers
}

fn apply_core_upstream_headers(
    headers: &mut HeaderMap,
    api_type: ApiType,
    protocol_route: (Protocol, Protocol),
    api_key: Option<&ApiKey>,
) {
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
}

fn provider_endpoint_base_url<'a>(
    configured: &'a str,
    implementation: Option<&dyn yabane_extension_api::ProviderEndpoint>,
) -> &'a str {
    implementation
        .and_then(|implementation| implementation.endpoint_type().fixed_base_url)
        .unwrap_or(configured)
}

fn protocol_from_extension(protocol: yabane_extension_api::Protocol) -> Protocol {
    match protocol {
        yabane_extension_api::Protocol::OpenAiChatCompletions => Protocol::OpenAiChat,
        yabane_extension_api::Protocol::OpenAiResponses => Protocol::OpenAiResponses,
        yabane_extension_api::Protocol::AnthropicMessages => Protocol::AnthropicMessages,
    }
}

fn extension_protocol(protocol: Protocol) -> yabane_extension_api::Protocol {
    match protocol {
        Protocol::OpenAiChat => yabane_extension_api::Protocol::OpenAiChatCompletions,
        Protocol::OpenAiResponses => yabane_extension_api::Protocol::OpenAiResponses,
        Protocol::AnthropicMessages => yabane_extension_api::Protocol::AnthropicMessages,
    }
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

fn upstream_transport_failure(err: &reqwest::Error, using_socks5_proxy: bool) -> RequestFailure {
    let route = if using_socks5_proxy {
        " through the configured SOCKS5 proxy"
    } else {
        ""
    };
    if err.is_timeout() {
        return RequestFailure::new(
            "upstream_connect",
            "timeout",
            format!("Upstream request{route} timed out"),
        );
    }
    let detail = io_failure_detail(err).map(|detail| format!(": {detail}"));
    if err.is_connect() {
        let category = if using_socks5_proxy {
            "proxy_connect_failed"
        } else {
            "connect_failed"
        };
        return RequestFailure::new(
            "upstream_connect",
            category,
            format!(
                "Could not connect to upstream{route}{}",
                detail.unwrap_or_default()
            ),
        );
    }
    RequestFailure::new(
        "upstream_transport",
        "request_failed",
        format!(
            "Upstream request{route} failed{}",
            detail.unwrap_or_default()
        ),
    )
}

fn upstream_http_failure(status: StatusCode) -> RequestFailure {
    let category = match status.as_u16() {
        401 | 403 => "authentication",
        408 | 504 => "timeout",
        429 => "rate_limited",
        400..=499 => "rejected",
        _ => "server_error",
    };
    RequestFailure::new(
        "upstream_response",
        category,
        format!("Upstream returned HTTP {}", status.as_u16()),
    )
}

fn stream_failure(message: &str) -> RequestFailure {
    let (stage, category, safe_message) = if message.contains("8 MiB") {
        (
            "protocol_conversion",
            "frame_too_large",
            "Upstream stream exceeded the conversion limit",
        )
    } else if message.contains("valid JSON") || message.contains("convert") {
        (
            "protocol_conversion",
            "invalid_response",
            "Could not convert the upstream stream",
        )
    } else if message.contains("terminal event") {
        (
            "upstream_stream",
            "truncated",
            "Upstream stream ended before completion",
        )
    } else {
        (
            "upstream_stream",
            "failed",
            "Upstream stream reported a failure",
        )
    };
    RequestFailure::new(stage, category, safe_message)
}

fn io_failure_detail(err: &(dyn StdError + 'static)) -> Option<&'static str> {
    let mut current = Some(err);
    while let Some(error) = current {
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            return match io.kind() {
                std::io::ErrorKind::ConnectionRefused => Some("connection refused"),
                std::io::ErrorKind::ConnectionReset => Some("connection reset"),
                std::io::ErrorKind::ConnectionAborted => Some("connection aborted"),
                std::io::ErrorKind::NotConnected => Some("not connected"),
                std::io::ErrorKind::AddrNotAvailable => Some("address unavailable"),
                std::io::ErrorKind::TimedOut => Some("connection timed out"),
                std::io::ErrorKind::UnexpectedEof => Some("connection closed unexpectedly"),
                _ => None,
            };
        }
        current = error.source();
    }
    None
}

fn response_is_event_stream(
    status: StatusCode,
    api_type: ApiType,
    endpoint_always_streams: bool,
    content_type: &str,
) -> bool {
    (status.is_success() && api_type == ApiType::OpenaiCodex && endpoint_always_streams)
        || content_type
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
}

fn to_reqwest_method(method: &Method) -> reqwest::Method {
    reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST)
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::{
        ApiSurface, ApiType, Protocol, apply_core_upstream_headers, copy_response_headers,
        provider_endpoint_base_url, response_is_event_stream, sanitize_request_headers,
        strip_transformed_response_headers, upstream_transport_failure,
    };

    #[test]
    fn codex_http_errors_are_not_misclassified_as_event_streams() {
        assert!(!response_is_event_stream(
            axum::http::StatusCode::BAD_REQUEST,
            ApiType::OpenaiCodex,
            true,
            "application/json",
        ));
        assert!(response_is_event_stream(
            axum::http::StatusCode::OK,
            ApiType::OpenaiCodex,
            true,
            "",
        ));
        assert!(response_is_event_stream(
            axum::http::StatusCode::BAD_GATEWAY,
            ApiType::OpenaiCodex,
            true,
            "text/event-stream; charset=utf-8",
        ));
    }

    #[test]
    fn provider_endpoint_fixed_base_url_overrides_persisted_configuration() {
        assert_eq!(
            provider_endpoint_base_url(
                "https://attacker.invalid/backend-api",
                Some(&yabane_extension_openai_subscription::ENDPOINT),
            ),
            "https://chatgpt.com/backend-api"
        );
        assert_eq!(
            provider_endpoint_base_url("https://configured.example/v1", None),
            "https://configured.example/v1"
        );
    }

    #[test]
    fn codex_adapter_applies_required_response_fields() {
        let mut body =
            br#"{"model":"gpt-5.4","input":"hello","stream":true,"store":true}"#.to_vec();
        yabane_extension_openai_subscription::adapt_body(&mut body);
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["stream"], true);
        assert_eq!(value["store"], false);
        assert_eq!(value["instructions"], "You are a helpful assistant.");
        assert_eq!(value["input"][0]["role"], "user");
        assert_eq!(value["input"][0]["content"][0]["text"], "hello");
        assert!(value.get("max_output_tokens").is_none());
        assert_eq!(value["text"]["verbosity"], "low");
        assert_eq!(value["tool_choice"], "auto");
        assert_eq!(value["parallel_tool_calls"], true);
        assert_eq!(value["include"][0], "reasoning.encrypted_content");
    }

    #[test]
    fn codex_adapter_matches_pi_ai_system_prompt_and_output_limit_shape() {
        let mut body = br#"{"model":"gpt-5.6-sol","input":[{"role":"developer","content":"Pi system prompt"},{"role":"user","content":[{"type":"input_text","text":"hello"}]}],"max_output_tokens":128000}"#.to_vec();
        yabane_extension_openai_subscription::adapt_body(&mut body);
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["instructions"], "Pi system prompt");
        assert_eq!(value["input"].as_array().unwrap().len(), 1);
        assert_eq!(value["input"][0]["role"], "user");
        assert!(value.get("max_output_tokens").is_none());
    }

    #[test]
    fn codex_session_affinity_matches_pi_ai_and_rejects_invalid_values() {
        let long = format!("{}tail", "x".repeat(64));
        let body = serde_json::to_vec(&serde_json::json!({"prompt_cache_key": long})).unwrap();
        assert_eq!(
            yabane_extension_openai_subscription::session_id(&body).unwrap(),
            "x".repeat(64)
        );
        assert!(
            yabane_extension_openai_subscription::session_id(br#"{"prompt_cache_key":""}"#)
                .is_none()
        );
        assert!(
            yabane_extension_openai_subscription::session_id(
                br#"{"prompt_cache_key":"bad\nheader"}"#
            )
            .is_none()
        );
        assert!(yabane_extension_openai_subscription::session_id(br#"{}"#).is_none());
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
        let mut sanitized = sanitize_request_headers(
            headers,
            ApiType::Anthropic,
            (Protocol::OpenAiChat, Protocol::AnthropicMessages),
        );
        apply_core_upstream_headers(
            &mut sanitized,
            ApiType::Anthropic,
            (Protocol::OpenAiChat, Protocol::AnthropicMessages),
            None,
        );

        assert_eq!(sanitized["accept-encoding"], "identity");
        assert!(!sanitized.contains_key("openai-organization"));
        assert!(!sanitized.contains_key("openai-project"));
        assert_eq!(sanitized["anthropic-version"], "2023-06-01");
        assert!(!sanitized.contains_key("anthropic-beta"));
    }

    #[test]
    fn upstream_transport_errors_identify_the_configured_proxy_without_exposing_it() {
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("socks5h://127.0.0.1:1").unwrap())
            .build()
            .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(client.get("https://example.com").send())
            .unwrap_err();
        let failure = upstream_transport_failure(&error, true);
        assert_eq!(failure.stage, "upstream_connect");
        assert_eq!(failure.category, "proxy_connect_failed");
        assert!(failure.message.contains("configured SOCKS5 proxy"));
        assert!(!failure.message.contains("127.0.0.1"));
        assert!(!failure.message.contains("example.com"));
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
        headers.insert("session-id", HeaderValue::from_static("caller-session"));
        headers.insert(
            "x-client-request-id",
            HeaderValue::from_static("caller-request"),
        );
        headers.insert(
            "user-agent",
            HeaderValue::from_static("pi (test-os; test-arch)"),
        );
        let mut sanitized = sanitize_request_headers(
            headers,
            ApiType::OpenaiCodex,
            (Protocol::OpenAiResponses, Protocol::OpenAiResponses),
        );
        apply_core_upstream_headers(
            &mut sanitized,
            ApiType::OpenaiCodex,
            (Protocol::OpenAiResponses, Protocol::OpenAiResponses),
            None,
        );
        let mut body = br#"{"model":"gpt-5.4","input":"hello"}"#.to_vec();
        let mut target_path = "/v1/responses".to_owned();
        yabane_extension_api::ProviderEndpoint::prepare_request(
            &yabane_extension_openai_subscription::ENDPOINT,
            yabane_extension_api::ProviderEndpointRequest {
                headers: &mut sanitized,
                body: &mut body,
                target_path: &mut target_path,
                credential: Some(yabane_extension_api::ProviderEndpointCredential {
                    access_token: "access-token",
                    account_id: "account-123",
                }),
            },
        )
        .unwrap();
        assert_eq!(sanitized["authorization"], "Bearer access-token");
        assert_eq!(sanitized["chatgpt-account-id"], "account-123");
        assert_eq!(sanitized["originator"], "pi");
        assert_eq!(
            sanitized["user-agent"],
            yabane_extension_openai_subscription::pi_user_agent()
        );
        assert_ne!(sanitized["user-agent"], "pi (test-os; test-arch)");
        assert!(
            sanitized["user-agent"]
                .to_str()
                .unwrap()
                .starts_with("pi (")
        );
        assert_eq!(sanitized["openai-beta"], "responses=experimental");
        assert_eq!(sanitized["accept-encoding"], "identity");
        assert!(!sanitized.contains_key("content-encoding"));
        assert!(!sanitized.contains_key("session-id"));
        assert!(!sanitized.contains_key("x-client-request-id"));
    }
}

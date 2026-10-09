use std::{
    error::Error as StdError,
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    response::Response,
};
use futures_util::StreamExt;
use rand::RngCore;
use tracing::{error, warn};

use crate::{
    activity::{ActivityStore, CostSource, RequestFailure, RequestLog},
    auth,
    config::{
        ApiEndpoint, ApiType, AppState, Credential, CredentialChoice, Provider, RateLimitCooldown,
        RateLimitCooldownMode, destination_availability,
    },
    endpoint_signin,
    error::{self, api_error},
    health,
    limits::MAX_BUFFERED_BODY_BYTES,
    pricing,
    protocol::{self, Protocol},
    protocol_stream::StreamConverter,
    routes::RouteMode,
    usage::{TokenUsage, UsageTracker},
};

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

    /// Whether an Endpoint type can serve this caller API. A native Endpoint
    /// type follows Core's own table; an Extension-owned one answers through its
    /// declaration instead of being inferred from a type name.
    fn supports(
        self,
        api_type: ApiType,
        extensions: &crate::extensions::ExtensionRegistry,
    ) -> bool {
        if let Some(declaration) = extensions.endpoint_declaration(api_type) {
            return declaration
                .surfaces
                .contains(&extension_protocol(self.protocol()));
        }
        if api_type.extension_endpoint_type().is_some() {
            // The Extension is not enabled, so the Endpoint cannot serve at all.
            return false;
        }
        matches!(
            (self, api_type),
            (
                Self::OpenAiChat,
                ApiType::OpenaiCompatible | ApiType::OpenaiChatCompletions
            ) | (
                Self::OpenAiResponses,
                ApiType::OpenaiCompatible | ApiType::OpenaiResponses
            ) | (Self::Anthropic, ApiType::Anthropic)
        )
    }

    fn upstream_protocol(self, api_type: ApiType) -> Protocol {
        match api_type {
            ApiType::Anthropic => Protocol::AnthropicMessages,
            ApiType::OpenaiResponses => Protocol::OpenAiResponses,
            ApiType::OpenaiChatCompletions => Protocol::OpenAiChat,
            ApiType::OpenaiCompatible | ApiType::Extension(_) => match self {
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
    let request_id = request_id();
    let mut response = route_proxied(state, request, surface, request_id.clone()).await;
    error::attach_request_id(&mut response, &request_id);
    if response.status().is_client_error() || response.status().is_server_error() {
        // Upstream-authored error passthrough names its own origin; every other
        // failure at this point is a message Yabane wrote.
        error::default_error_origin(&mut response, error::ErrorOrigin::Yabane);
    }
    response
}

/// Reads a caller body up to `limit` bytes. `None` means the body is unusable,
/// either past the limit or failed mid-stream; both are answered as Yabane's own
/// `413` so a caller never mistakes it for a Provider answer. The limit is a
/// parameter so a test can cross it without materializing the production ceiling.
async fn read_body(body: Body, limit: usize) -> Option<bytes::Bytes> {
    axum::body::to_bytes(body, limit).await.ok()
}

fn request_body_too_large() -> Response {
    api_error(StatusCode::PAYLOAD_TOO_LARGE, "Request body is too large")
}

async fn route_proxied(
    state: AppState,
    request: Request,
    surface: ApiSurface,
    request_id: String,
) -> Response {
    let request_started = Instant::now();
    let allowed_providers = auth::authorized_provider_ids(&request).map(<[String]>::to_vec);
    let gateway_api_key = auth::authorized_gateway_key(&request).cloned();
    let (parts, body) = request.into_parts();
    let body = match read_body(body, MAX_BUFFERED_BODY_BYTES).await {
        Some(body) => body,
        None => return request_body_too_large(),
    };
    let requested_streaming = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.get("stream").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    let ResolvedRoute {
        provider,
        endpoint,
        credential,
        credential_cooling,
        model,
        upstream_model,
        body,
        route,
    } = match resolve_provider(&state, &body, surface, allowed_providers.as_deref()).await {
        Ok(resolved) => resolved,
        Err(err) => return api_error(err.status, err.message),
    };

    let upstream_protocol = match state.extensions.endpoint_declaration(endpoint.api_type) {
        Some(declaration) => protocol_from_extension(declaration.upstream_protocol),
        None if endpoint.extension_endpoint_type().is_some() => {
            return endpoint_type_unavailable(&endpoint.id);
        }
        None => surface.upstream_protocol(endpoint.api_type),
    };
    let body = match protocol::convert_request(&body, surface.protocol(), upstream_protocol) {
        Ok(body) => body,
        Err(err) => return api_error(StatusCode::BAD_REQUEST, err),
    };

    // A signed-in account is refreshed per request when its Endpoint type
    // declares accounts that expire; a pasted secret is used as configured.
    let endpoint_declaration = state.extensions.endpoint_declaration(endpoint.api_type);
    let endpoint_declares_account = endpoint_declaration.is_some_and(|declaration| {
        declaration
            .credential_kinds
            .iter()
            .any(|kind| kind.flow == yabane_extension_api::CredentialFlow::Subscription)
    });
    let credential = if endpoint_declares_account {
        let Some(credential) = credential else {
            return api_error(
                StatusCode::CONFLICT,
                missing_identity_message(&endpoint, endpoint_declaration),
            );
        };
        match endpoint_signin::refreshed_credential(
            &state,
            &provider.id,
            &endpoint.id,
            &credential.id,
        )
        .await
        {
            Ok(credential) => Some(credential),
            Err(err) => {
                error!(provider = %provider.id, endpoint = %endpoint.id, %err, "the Endpoint type could not refresh its account");
                return api_error(StatusCode::BAD_GATEWAY, err);
            }
        }
    } else {
        credential
    };

    forward(
        state,
        ForwardRequest {
            request_id,
            provider,
            endpoint,
            credential,
            credential_cooling,
            gateway_api_key,
            model,
            upstream_model,
            parts,
            body,
            route,
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
) -> Result<ResolvedRoute, RoutingError> {
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
    let route_choice = {
        // The Provider snapshot is taken before the route lock, which is the
        // order Provider and Endpoint mutations use, so a route decision that
        // reads Endpoint health can never deadlock with a console write.
        let providers = state.providers.read().await;
        state
            .routes
            .resolve(&model, |target| {
                destination_availability(
                    &providers,
                    &state.extensions,
                    &state.credential_health,
                    target,
                )
            })
            .await
    };
    let route_target = route_choice.as_ref().map(|choice| &choice.target);
    let (provider_id, upstream_model) = if let Some(target) = route_target {
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
    if route_choice
        .as_ref()
        .is_some_and(|choice| choice.all_cooling)
    {
        warn!(
            %model,
            provider = provider_id,
            "every destination of the model route is cooling down; serving the request from the preferred one"
        );
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
    let endpoint = if let Some(target) = route_target {
        provider
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == target.endpoint_id)
    } else {
        let discovered = provider.model_endpoints.get(upstream_model);
        let endpoint_available = |endpoint: &ApiEndpoint| {
            match endpoint.extension_endpoint_type() {
                // An Extension-owned Endpoint serves only while its Extension is
                // enabled; Core reads that from the registry, not from a name.
                Some(endpoint_type) => state.extensions.provider_endpoint(endpoint_type).is_some(),
                None => true,
            }
        };
        let endpoint = provider
            .endpoints
            .iter()
            .find(|endpoint| {
                endpoint_available(endpoint)
                    && surface.supports(endpoint.api_type, &state.extensions)
                    && provider.preferred_endpoint_id(upstream_model, endpoint.api_type)
                        == Some(endpoint.id.as_str())
                    && discovered.is_none_or(|endpoint_ids| endpoint_ids.contains(&endpoint.id))
            })
            .or_else(|| {
                provider.endpoints.iter().find(|endpoint| {
                    endpoint_available(endpoint)
                        && surface.supports(endpoint.api_type, &state.extensions)
                        && discovered.is_none_or(|endpoint_ids| endpoint_ids.contains(&endpoint.id))
                })
            })
            .or_else(|| {
                provider.endpoints.iter().find(|endpoint| {
                    endpoint_available(endpoint)
                        && discovered.is_none_or(|endpoint_ids| endpoint_ids.contains(&endpoint.id))
                })
            });
        if endpoint.is_none()
            && provider.endpoints.iter().any(|endpoint| {
                endpoint.extension_endpoint_type().is_some()
                    && !endpoint_available(endpoint)
                    && discovered.is_none_or(|endpoint_ids| endpoint_ids.contains(&endpoint.id))
            })
        {
            return Err(RoutingError {
                status: StatusCode::BAD_REQUEST,
                message: unavailable_endpoint_type_message(&provider.endpoints, state),
            });
        }
        endpoint
    }
    .ok_or_else(|| RoutingError {
        status: StatusCode::BAD_REQUEST,
        message: format!("Provider '{provider_id}' has no endpoint for model '{upstream_model}'"),
    })?;
    let (credential, credential_cooling) = if !endpoint.requires_credential {
        (None, false)
    } else if let Some(target) = route_target {
        if target.credential_id.is_empty() {
            // An omitted identity means the Endpoint owns credential selection.
            let choice = select_endpoint_credential(state, endpoint, provider_id)?;
            (Some(choice.credential), choice.all_exhausted)
        } else {
            let credential = endpoint
                .credentials
                .iter()
                .find(|credential| credential.id == target.credential_id)
                .ok_or_else(|| RoutingError {
                    status: StatusCode::CONFLICT,
                    message: format!("Model route for '{model}' refers to a missing credential"),
                })?;
            if !credential.enabled {
                return Err(RoutingError {
                    status: StatusCode::CONFLICT,
                    message: format!("Model route for '{model}' has no enabled credential"),
                });
            }
            // A pinned identity is used as configured: it does not take part in
            // cooldown avoidance, so an exhausted pin fails visibly. Activity
            // still records that the pin was exhausted when it served.
            let cooling = state.credential_health.is_cooling(&health::credential_key(
                provider_id,
                &endpoint.id,
                &credential.id,
            ));
            (Some(credential.clone()), cooling)
        }
    } else {
        let choice = select_endpoint_credential(state, endpoint, provider_id)?;
        (Some(choice.credential), choice.all_exhausted)
    };

    let endpoint = endpoint.clone();
    let upstream_model = upstream_model.to_owned();
    payload["model"] = serde_json::Value::String(upstream_model.clone());
    let body = serde_json::to_vec(&payload).expect("serialize validated request body");
    Ok(ResolvedRoute {
        provider,
        endpoint,
        credential,
        credential_cooling,
        model,
        upstream_model,
        body,
        route: route_choice.as_ref().map(|choice| RouteFacts {
            mode: choice.mode,
            priority: choice.target.priority,
            failover: choice.failover,
        }),
    })
}

/// How the model route that chose this destination decided. Recorded as facts so
/// a `429` from a preferred destination stays explainable after its cooldown
/// expires; a request that named its Provider directly carries none.
#[derive(Clone, Copy, Debug)]
struct RouteFacts {
    mode: RouteMode,
    priority: u32,
    failover: bool,
}

/// One request's route decision. `credential_cooling` is the exhaustion fact
/// Activity records so a `429` stays explainable after the cooldown expires.
struct ResolvedRoute {
    provider: Provider,
    endpoint: ApiEndpoint,
    credential: Option<Credential>,
    credential_cooling: bool,
    model: String,
    upstream_model: String,
    body: Vec<u8>,
    route: Option<RouteFacts>,
}

/// Applies the Endpoint's credential policy. Every usable credential that is not
/// cooling down is a candidate; when none is left the pool still serves an
/// exhausted credential so the Provider's own answer reaches the caller.
fn select_endpoint_credential(
    state: &AppState,
    endpoint: &ApiEndpoint,
    provider_id: &str,
) -> Result<CredentialChoice, RoutingError> {
    match endpoint.select_credential(&state.credential_health, provider_id) {
        Some(choice) => {
            if choice.all_exhausted {
                warn!(
                    provider_id,
                    endpoint = %endpoint.id,
                    "every credential is cooling down; serving the request from an exhausted identity"
                );
            }
            Ok(choice)
        }
        None => Err(RoutingError {
            status: StatusCode::CONFLICT,
            message: format!("Endpoint '{}' has no enabled credential", endpoint.id),
        }),
    }
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

/// Set once Yabane is stopping and its grace period has ended, so an interrupted
/// response is explained as a shutdown instead of blaming the caller.
static SHUTTING_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Streaming exchanges that have not written their terminal Activity record yet.
/// Shutdown waits briefly for this to reach zero so an exchange it interrupted is
/// still part of the final flush.
static ACTIVE_STREAM_RECORDS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn shutdown_notice_channel() -> &'static tokio::sync::watch::Sender<bool> {
    static NOTICE: std::sync::OnceLock<tokio::sync::watch::Sender<bool>> =
        std::sync::OnceLock::new();
    NOTICE.get_or_init(|| tokio::sync::watch::channel(false).0)
}

fn shutdown_notice() -> tokio::sync::watch::Receiver<bool> {
    shutdown_notice_channel().subscribe()
}

/// Announces that the grace period is over. In-flight responses end themselves
/// through this notice, which lets each one record its own interruption instead
/// of being dropped mid-await when the process stops.
pub(crate) fn mark_shutting_down() {
    SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = shutdown_notice_channel().send(true);
}

pub(crate) fn active_stream_records() -> usize {
    ACTIVE_STREAM_RECORDS.load(std::sync::atomic::Ordering::SeqCst)
}

/// Resolves when Yabane announces that it is stopping, including the case where
/// the notice was already sent before this response started.
async fn wait_for_shutdown(notice: &mut tokio::sync::watch::Receiver<bool>) {
    if *notice.borrow() {
        return;
    }
    while notice.changed().await.is_ok() {
        if *notice.borrow() {
            return;
        }
    }
    std::future::pending::<()>().await;
}

/// A non-streaming cross-protocol conversion needs the whole Provider response in
/// memory, so it is bounded like aggregated output instead of buffering whatever a
/// broken or hostile Provider sends.
async fn read_non_streaming_response(
    mut response: reqwest::Response,
) -> Result<bytes::Bytes, RequestFailure> {
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len().saturating_add(chunk.len()) > MAX_BUFFERED_BODY_BYTES {
                    return Err(RequestFailure::new(
                        "protocol_conversion",
                        "response_too_large",
                        "Provider response exceeded the conversion limit",
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(bytes::Bytes::from(body)),
            Err(err) => return Err(upstream_read_failure(&err, false)),
        }
    }
}

fn shutdown_failure() -> RequestFailure {
    RequestFailure::new(
        "gateway",
        "shutdown",
        "Yabane stopped before the Provider response finished",
    )
}

/// Owns everything that has to be finalized exactly once when a Provider response
/// body ends: normally, on a read or conversion failure, or because the caller
/// disconnected mid-stream and the body was dropped before the exchange finished.
/// The proxy builds it before the response body is created, so a caller that goes
/// away still leaves one Activity record and one observer completion behind.
struct StreamCompletion {
    observers: Vec<Box<dyn yabane_extension_api::UpstreamExchangeObserver>>,
    activity: Option<ProxyActivity>,
    usage: UsageTracker,
    streaming: bool,
    first_byte_ms: Option<u64>,
    completed: bool,
}

impl Drop for StreamCompletion {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        complete_exchange_observers(
            &mut self.observers,
            yabane_extension_api::ExchangeOutcome::Interrupted,
        );
        let Some(activity) = self.activity.take() else {
            return;
        };
        let (usage, _) = self.usage.finish();
        let streaming = self.streaming;
        let first_byte_ms = self.first_byte_ms;
        let failure = if SHUTTING_DOWN.load(std::sync::atomic::Ordering::SeqCst) {
            shutdown_failure()
        } else {
            RequestFailure::new(
                "client",
                "disconnected",
                "Client disconnected before the Provider response finished",
            )
        };
        // Dropping the body happens on a runtime thread, but the Activity store
        // still has to be written, so the record is spawned rather than skipped.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            ACTIVE_STREAM_RECORDS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            return;
        };
        handle.spawn(async move {
            activity
                .record_failure(
                    StatusCode::BAD_GATEWAY,
                    usage,
                    streaming,
                    first_byte_ms,
                    failure,
                )
                .await;
            ACTIVE_STREAM_RECORDS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    }
}

struct ProxyActivity {
    store: ActivityStore,
    request_id: String,
    path: String,
    model: String,
    upstream_model: Option<String>,
    provider: String,
    endpoint: String,
    /// The identity the request actually left with, when the Endpoint uses one.
    credential_id: Option<String>,
    /// That identity's name at the time, so Activity stays readable after the
    /// credential is renamed or deleted. Display metadata only, never the
    /// credential or the account behind it.
    credential_name: Option<String>,
    /// Whether that identity was exhausted when it carried the request.
    credential_cooling: bool,
    /// Selection facts of the model route that chose this destination, when one
    /// did, so the record explains a switch after a rate limit.
    route: Option<RouteFacts>,
    pricing: Option<pricing::ResolvedPricing>,
    gateway_api_key: Option<auth::AuthorizedGatewayKey>,
    caller_protocol: Protocol,
    upstream_protocol: Protocol,
    started: Instant,
    gateway_ms: u64,
    upstream_response_ms: u64,
    /// Whether the Provider response is an event stream, so the recorded
    /// generation time describes an actual generation instead of a body download.
    upstream_streaming: bool,
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
        let (cost, cost_source) = self.cost_for_usage(&usage);
        self.store
            .record(RequestLog {
                timestamp: crate::auth::now(),
                request_id: self.request_id.clone(),
                source_instance_id: None,
                gateway_api_key_id: self.gateway_api_key.as_ref().map(|key| key.id.clone()),
                gateway_api_key_note: self.gateway_api_key.as_ref().map(|key| key.note.clone()),
                gateway_api_key_prefix: self.gateway_api_key.as_ref().map(|key| key.prefix.clone()),
                path: self.path.clone(),
                model: self.model.clone(),
                upstream_model: self.upstream_model.clone(),
                provider: self.provider.clone(),
                endpoint: self.endpoint.clone(),
                upstream_credential_id: self.credential_id.clone(),
                upstream_credential_name: self.credential_name.clone(),
                credential_cooling: self.credential_cooling,
                route_mode: self.route.map(|route| route.mode.name().to_owned()),
                route_priority: self.route.map(|route| route.priority),
                route_failover: self.route.is_some_and(|route| route.failover),
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
                cost_source,
                cost,
                pricing_sources: match cost_source {
                    Some(CostSource::Estimated) => self
                        .pricing
                        .as_ref()
                        .map(|resolved| resolved.sources.clone()),
                    _ => None,
                },
                finish_reason: usage.finish_reason,
                streaming,
                upstream_streaming: self.upstream_streaming,
            })
            .await;
    }

    fn cost_for_usage(&self, usage: &TokenUsage) -> (Option<f64>, Option<CostSource>) {
        if let Some(cost) = usage.cost {
            return (Some(cost), Some(CostSource::Reported));
        }
        self.pricing
            .as_ref()
            .and_then(|resolved| pricing::calculate(&resolved.pricing, usage))
            .map(|cost| (Some(cost), Some(CostSource::Estimated)))
            .unwrap_or((None, None))
    }
}

struct ForwardRequest {
    request_id: String,
    provider: Provider,
    endpoint: ApiEndpoint,
    credential: Option<Credential>,
    credential_cooling: bool,
    /// Selection facts of the model route that chose this destination, when a
    /// model route did.
    route: Option<RouteFacts>,
    gateway_api_key: Option<auth::AuthorizedGatewayKey>,
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
        request_id,
        provider,
        endpoint,
        credential,
        credential_cooling,
        route,
        gateway_api_key,
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
    let mut request_headers =
        sanitize_request_headers(parts.headers, (caller_protocol, upstream_protocol));
    if !extension_hooks.upstream_headers.is_empty() {
        match state.extensions.run_upstream_headers(
            &extension_context,
            &mut request_headers,
            &extension_hooks.upstream_headers,
        ) {
            Ok(crate::extensions::DispatchOutcome::Continue(())) => {}
            Ok(crate::extensions::DispatchOutcome::Reject(rejection)) => {
                return crate::extensions::rejection_response(rejection);
            }
            Err(failure) => return crate::extensions::execution_error(failure),
        };
    }
    let endpoint_always_streams = state
        .extensions
        .endpoint_declaration(endpoint.api_type)
        .is_some_and(|declaration| declaration.always_event_stream);
    if endpoint.extension_endpoint_type().is_none() {
        apply_core_upstream_headers(
            &mut request_headers,
            endpoint.api_type,
            (caller_protocol, upstream_protocol),
            credential.as_ref(),
        );
    }
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
    let endpoint_implementation = match endpoint.extension_endpoint_type() {
        Some(endpoint_type) => match state.extensions.provider_endpoint(endpoint_type) {
            Some(implementation) => Some(implementation),
            None => return endpoint_type_unavailable(&endpoint.id),
        },
        None => None,
    };
    if let Some(implementation) = endpoint_implementation {
        // Core hands over the stored identity as-is: the declaration names the
        // kind, and the material keeps the shape Core stored.
        let request_credential = credential.as_ref().map(|credential| {
            yabane_extension_api::ProviderEndpointCredential {
                kind: credential.kind.as_str(),
                material: match &credential.material {
                    crate::config::CredentialMaterial::Secret { secret } => {
                        yabane_extension_api::ProviderEndpointMaterial::Secret { secret }
                    }
                    crate::config::CredentialMaterial::Subscription {
                        access_token,
                        account_id,
                        ..
                    } => yabane_extension_api::ProviderEndpointMaterial::Subscription {
                        access_token,
                        account_id,
                    },
                },
            }
        });
        if let Err(error) =
            implementation.prepare_request(yabane_extension_api::ProviderEndpointRequest {
                headers: &mut request_headers,
                body: &mut body,
                target_path: &mut target_path,
                credential: request_credential,
            })
        {
            error!(provider = %provider.id, endpoint = %endpoint.id, %error, "Endpoint implementation request preparation failed");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Endpoint '{}' could not prepare the request", endpoint.id),
            );
        }
    }
    // Request extensions and Endpoint implementations may explicitly rewrite or
    // remove the routed model. Activity must describe the final wire request, not
    // merely the route target selected before those transformations ran.
    let sent_upstream_model = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.get("model")?.as_str().map(str::to_owned));
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

    let client = match endpoint.client(&state.client, state.upstream_timeouts) {
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
                    gateway_api_key_id: gateway_api_key.as_ref().map(|key| key.id.clone()),
                    gateway_api_key_note: gateway_api_key.as_ref().map(|key| key.note.clone()),
                    gateway_api_key_prefix: gateway_api_key.as_ref().map(|key| key.prefix.clone()),
                    path,
                    model,
                    upstream_model: sent_upstream_model,
                    provider: provider.id.clone(),
                    endpoint: endpoint.id.clone(),
                    upstream_credential_id: credential
                        .as_ref()
                        .map(|credential| credential.id.clone()),
                    upstream_credential_name: credential
                        .as_ref()
                        .map(|credential| credential.name.clone()),
                    credential_cooling,
                    route_mode: route.map(|route| route.mode.name().to_owned()),
                    route_priority: route.map(|route| route.priority),
                    route_failover: route.is_some_and(|route| route.failover),
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
                    cost_source: None,
                    cost: None,
                    pricing_sources: None,
                    finish_reason: None,
                    streaming: requested_streaming,
                    upstream_streaming: false,
                })
                .await;
            return api_error(StatusCode::BAD_GATEWAY, &failure.message);
        }
    };

    let upstream_response_ms = upstream_started.elapsed().as_millis() as u64;
    let status = upstream_response.status();
    let response_headers = upstream_response.headers().clone();
    // The credential's own answer is the only evidence Yabane accepts that an
    // identity is exhausted, and only an explicitly configured policy turns it
    // into a cooldown. The response itself stays untouched, and a policy that
    // relies on the Provider's own delay arms nothing when there is none. What
    // the answer asked for is recorded next to the length that was used, so a
    // number the ceiling held down is not lost and a missing delay stays
    // readable as the fallback it was.
    if status == StatusCode::TOO_MANY_REQUESTS
        && endpoint.rate_limit_cooldown.enabled()
        && let Some(credential) = &credential
    {
        let observed_at = crate::auth::now();
        let endpoint_key = health::endpoint_key(&provider.id, &endpoint.id);
        let reported = reported_retry_after(&response_headers);
        match cooldown_decision(&endpoint.rate_limit_cooldown, reported) {
            Some((duration, source)) => {
                state.credential_health.cool_down(
                    health::credential_key(&provider.id, &endpoint.id, &credential.id),
                    duration,
                );
                state.credential_health.record_cooldown_applied(
                    endpoint_key,
                    duration.as_secs(),
                    reported,
                    source,
                    observed_at,
                );
            }
            None => state
                .credential_health
                .record_cooldown_skipped(endpoint_key, observed_at),
        }
    }
    if !exchange_observers.is_empty() {
        let observed_response_headers = observed_headers(&response_headers);
        observe_response_head(&mut exchange_observers, status, &observed_response_headers);
    }
    let content_type = response_headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    // Some Endpoint types answer with SSE even when a caller asked for one
    // response, and may omit Content-Type on success. The declaration decides;
    // callers parse the stream by protocol, not by header.
    let event_stream = response_is_event_stream(status, endpoint_always_streams, content_type);
    let activity_pricing = {
        let global_pricing = state.pricing.read().await;
        sent_upstream_model.as_deref().and_then(|model_id| {
            pricing::effective_pricing(&global_pricing, &provider, &endpoint, &model, model_id)
        })
    };
    let activity = ProxyActivity {
        store: state.activity.clone(),
        request_id,
        path,
        model,
        upstream_model: sent_upstream_model,
        provider: provider.id.clone(),
        endpoint: endpoint.id.clone(),
        credential_id: credential.as_ref().map(|credential| credential.id.clone()),
        credential_name: credential
            .as_ref()
            .map(|credential| credential.name.clone()),
        credential_cooling,
        route,
        pricing: activity_pricing,
        gateway_api_key,
        caller_protocol,
        upstream_protocol,
        started,
        gateway_ms: upstream_started.duration_since(started).as_millis() as u64,
        upstream_response_ms,
        upstream_streaming: event_stream,
    };
    let converting = status.is_success() && caller_protocol != upstream_protocol;

    // Some upstreams, including ChatGPT Codex subscriptions, require SSE even
    // when a native Responses caller requested one non-streaming response.
    if event_stream && !requested_streaming && (converting || endpoint_always_streams) {
        let mut upstream = upstream_response.bytes_stream();
        let mut usage = UsageTracker::new(upstream_protocol, true);
        let mut converter = StreamConverter::new_aggregating(upstream_protocol, caller_protocol);
        let mut failure = None;
        let mut failure_record = None;
        let mut first_byte_ms = None;
        let mut shutdown = shutdown_notice();
        while let Some(chunk) = tokio::select! {
            chunk = upstream.next() => chunk,
            () = wait_for_shutdown(&mut shutdown) => {
                // The caller is not waiting any more: stop reading the Provider
                // stream and record why the exchange has no answer.
                let record = shutdown_failure();
                failure = Some(record.message.clone());
                failure_record = Some(record);
                None
            }
        } {
            let chunk = match chunk {
                Ok(chunk) => {
                    first_byte_ms.get_or_insert_with(|| started.elapsed().as_millis() as u64);
                    chunk
                }
                Err(err) => {
                    error!(provider = %provider.id, endpoint = %endpoint.id, %err, "could not read upstream stream for protocol conversion");
                    let record = upstream_read_failure(&err, true);
                    failure = Some(record.message.clone());
                    failure_record = Some(record);
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
            failure = Some("Provider stream reported a failure".to_owned());
        }
        if let Some(err) = failure {
            activity
                .record_failure_at(
                    StatusCode::BAD_GATEWAY,
                    usage,
                    true,
                    first_byte_ms,
                    completion_ms,
                    Some(failure_record.unwrap_or_else(|| stream_failure(&err))),
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
        let bytes = match read_non_streaming_response(upstream_response).await {
            Ok(bytes) => bytes,
            Err(failure) => {
                error!(
                    provider = %provider.id,
                    endpoint = %endpoint.id,
                    stage = %failure.stage,
                    category = %failure.category,
                    "could not read the Provider response for protocol conversion"
                );
                let message = failure.message.clone();
                activity
                    .record_failure(
                        StatusCode::BAD_GATEWAY,
                        TokenUsage::default(),
                        false,
                        None,
                        failure,
                    )
                    .await;
                complete_exchange_observers(
                    &mut exchange_observers,
                    yabane_extension_api::ExchangeOutcome::ResponseReadError,
                );
                return api_error(StatusCode::BAD_GATEWAY, message);
            }
        };
        observe_response_chunk(&mut exchange_observers, &bytes);
        let mut usage = UsageTracker::new(upstream_protocol, false);
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
                            "Could not convert the Provider response",
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
        // An answer that already carries an HTTP error is passed through and
        // recorded as that error; a Protocol failure is only a gateway failure
        // when the Provider claimed success. Otherwise a rate-limit body that
        // names its error would make Activity disagree with the answer the
        // caller actually received.
        if status.is_client_error() || status.is_server_error() {
            activity
                .record_failure(
                    status,
                    usage,
                    requested_streaming,
                    None,
                    upstream_http_failure(status),
                )
                .await;
        } else if protocol_failed {
            activity
                .record_failure(
                    status,
                    usage,
                    requested_streaming,
                    None,
                    RequestFailure::new(
                        "upstream_response",
                        "protocol_failure",
                        "Provider response reported a failure",
                    ),
                )
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

    ACTIVE_STREAM_RECORDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let stream_state = StreamCompletion {
        observers: exchange_observers,
        activity: Some(activity),
        usage: UsageTracker::new(upstream_protocol, event_stream),
        streaming: requested_streaming || event_stream,
        first_byte_ms: None,
        completed: false,
    };
    let stream = async_stream::stream! {
        let mut state = stream_state;
        let mut upstream = upstream_response.bytes_stream();
        let mut converter = converting.then(|| StreamConverter::new(upstream_protocol, caller_protocol));
        let mut conversion_failed = false;
        let mut interrupted_by_shutdown = false;
        let mut shutdown = shutdown_notice();
        while let Some(chunk) = tokio::select! {
            chunk = upstream.next() => chunk,
            () = wait_for_shutdown(&mut shutdown) => {
                interrupted_by_shutdown = true;
                None
            }
        } {
            match chunk {
                Ok(chunk) => {
                    state
                        .first_byte_ms
                        .get_or_insert_with(|| started.elapsed().as_millis() as u64);
                    observe_response_chunk(&mut state.observers, &chunk);
                    state.usage.observe(&chunk);
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
                    let kind = if err.is_timeout() {
                        std::io::ErrorKind::TimedOut
                    } else {
                        std::io::ErrorKind::Other
                    };
                    let failure = upstream_read_failure(&err, true);
                    let message = failure.message.clone();
                    complete_exchange_observers(
                        &mut state.observers,
                        yabane_extension_api::ExchangeOutcome::Interrupted,
                    );
                    let (usage, _) = state.usage.finish();
                    let first_byte_ms = state.first_byte_ms;
                    let streaming = state.streaming;
                    let activity = state.activity.take().expect("one terminal Activity record");
                    // The record is already being written, so a cancelled generator
                    // must not write a second one for the same request.
                    state.completed = true;
                    activity
                        .record_failure(
                            StatusCode::BAD_GATEWAY,
                            usage,
                            streaming,
                            first_byte_ms,
                            failure,
                        )
                        .await;
                    ACTIVE_STREAM_RECORDS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    yield Err(std::io::Error::new(kind, message));
                    return;
                }
            }
        }
        if !conversion_failed
            && !interrupted_by_shutdown
            && let Some(converter) = &mut converter
        {
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
        let exchange_outcome = if conversion_failed || interrupted_by_shutdown {
            yabane_extension_api::ExchangeOutcome::Interrupted
        } else {
            yabane_extension_api::ExchangeOutcome::Complete
        };
        complete_exchange_observers(&mut state.observers, exchange_outcome);
        let (usage, protocol_failed) = state.usage.finish();
        let first_byte_ms = state.first_byte_ms;
        let streaming = state.streaming;
        let activity = state.activity.take().expect("one terminal Activity record");
        // Recorded below: the drop guard must not repeat it if this generator is
        // cancelled after the response finished.
        state.completed = true;
        let http_error = status.is_client_error() || status.is_server_error();
        // A Protocol failure only becomes a gateway status when the Provider
        // claimed success; an answer that already carries an HTTP error keeps
        // the status the caller received, so a rate-limit body that names its
        // error is not recorded as a Yabane failure.
        let recorded_status = if conversion_failed
            || interrupted_by_shutdown
            || (protocol_failed && !http_error)
        {
            StatusCode::BAD_GATEWAY
        } else {
            status
        };
        if interrupted_by_shutdown {
            activity
                .record_failure(
                    recorded_status,
                    usage,
                    streaming,
                    first_byte_ms,
                    shutdown_failure(),
                )
                .await;
        } else if conversion_failed {
            activity.record_failure(
                recorded_status,
                usage,
                streaming,
                first_byte_ms,
                RequestFailure::new(
                    "upstream_stream",
                    "interrupted",
                    "Provider stream ended or could not be converted",
                ),
            ).await;
        } else if http_error {
            activity.record_failure(
                status,
                usage,
                streaming,
                first_byte_ms,
                upstream_http_failure(status),
            ).await;
        } else if protocol_failed {
            activity.record_failure(
                recorded_status,
                usage,
                streaming,
                first_byte_ms,
                RequestFailure::new(
                    if streaming {
                        "upstream_stream"
                    } else {
                        "upstream_response"
                    },
                    "protocol_failure",
                    if streaming {
                        "Provider stream reported a failure"
                    } else {
                        "Provider response reported a failure"
                    },
                ),
            ).await;
        } else {
            activity.record(status, usage, streaming, first_byte_ms).await;
        }
        ACTIVE_STREAM_RECORDS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    };
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    copy_response_headers(response.headers_mut(), &response_headers);
    if status.is_client_error() || status.is_server_error() {
        error::set_error_origin(&mut response, error::ErrorOrigin::Upstream);
    }
    if endpoint_always_streams {
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
    // The base URL is the shared API root, so the operation path is appended
    // below it. Yabane's own operation paths are versioned, and a Provider may
    // version its API with a segment of its own; when the base already names a
    // version, repeating Yabane's would address a second root that the Provider
    // does not serve. See `upstream_operation_path`.
    let path = upstream_operation_path(base_url, path_and_query);
    format!(
        "{base}{}",
        if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        }
    )
}

/// The path an upstream request appends to its Endpoint's base URL.
///
/// The base URL is the Provider's shared API root, and the Provider versions
/// that root itself: Tencent CodeBuddy publishes the OpenAI-compatible surface
/// under `/v2`, Zhipu under `/api/paas/v4` and `/api/paas/v4/saas`, Google
/// under `/v1beta/openai`, OpenCode Go under `/zen/go/v1`. Yabane's own
/// operation paths carry a version segment as well, so appending it below an
/// already-versioned root would address a root the Provider does not serve: the
/// leading version segment is dropped when the base URL's path contains one. A
/// root with no version segment in it, such as a bare origin or a plain path
/// prefix, keeps the operation path as written.
///
/// A version is recognized in any segment, not only the last one, because a
/// root may name its version and then a surface below it, as in `/v4/saas` or
/// `/v1beta/openai`. Matching only a trailing segment would call those at
/// `/v4/saas/v1/chat/completions` and `/v1beta/openai/v1/chat/completions`.
/// Keeping the root's version is the deliberate choice here; a Provider whose
/// real root does sit below a version segment is reached by naming that whole
/// prefix in the base URL, which is normalized to one copy of the version.
fn upstream_operation_path<'a>(base_url: &str, path_and_query: &'a str) -> &'a str {
    if crate::config::base_url_path(base_url)
        .split('/')
        .any(is_version_segment)
        && let Some(rest) = strip_leading_version_segment(path_and_query)
    {
        return rest;
    }
    path_and_query
}

/// A path segment that names an API version: `v` followed by digits and then
/// letters or digits, as in `v1`, `v2`, `v4`, or `v1beta`. A segment such as
/// `version` or `vendors` is a resource name, not a version.
fn is_version_segment(segment: &str) -> bool {
    let Some(version) = segment.strip_prefix('v') else {
        return false;
    };
    let mut characters = version.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_digit())
        && characters.all(|character| character.is_ascii_alphanumeric())
}

/// `path_and_query` without its leading version segment, when it has one. The
/// result still starts with `/`, and any query is preserved.
fn strip_leading_version_segment(path_and_query: &str) -> Option<&str> {
    let rest = path_and_query.strip_prefix('/')?;
    let (segment, tail) = rest.split_once('/')?;
    if tail.is_empty() || !is_version_segment(segment) {
        return None;
    }
    Some(&path_and_query[1 + segment.len()..])
}

/// Removes only headers that are meaningless or unsafe to forward on every upstream
/// request, plus caller-protocol context that must not leak across an explicit
/// protocol conversion. Core stays a transparent proxy here: it never branches on an
/// Endpoint's API type, because provider-specific header ownership belongs to that
/// provider's Extension.
fn sanitize_request_headers(
    mut headers: HeaderMap,
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
    headers
}

fn apply_core_upstream_headers(
    headers: &mut HeaderMap,
    api_type: ApiType,
    protocol_route: (Protocol, Protocol),
    credential: Option<&Credential>,
) {
    if protocol_route.0 != protocol_route.1 {
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
    }
    // Only Core's own secret kind travels as a static header. An Extension-owned
    // Endpoint receives its identity through its own request preparation, so Core
    // never guesses a header for it.
    if let Some(secret) = credential.and_then(Credential::secret) {
        let (name, value) = match api_type {
            ApiType::Anthropic => (
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(secret),
            ),
            _ => (
                header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {secret}")),
            ),
        };
        headers.insert(name, value.expect("validated credential header value"));
    }
    if api_type == ApiType::Anthropic {
        headers
            .entry(HeaderName::from_static("anthropic-version"))
            .or_insert(HeaderValue::from_static("2023-06-01"));
    }
}

/// The delay the Provider's own answer suggests for later requests, when its
/// `Retry-After` carries an integer number of seconds. Every other shape — a
/// date, an unparseable value, a missing header — is not a delay this instance
/// reads, so it never becomes a duration by accident.
fn reported_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// Resolves how long a credential stays out of selection, or `None` when the
/// policy arms nothing for this answer. The configured length is the ceiling in
/// every mode, and the Provider's reported delay only supplies a number when the
/// policy chose a source that reads it.
fn cooldown_decision(
    policy: &RateLimitCooldown,
    reported: Option<u64>,
) -> Option<(Duration, health::CooldownSource)> {
    if !policy.enabled() {
        return None;
    }
    // The configured length is the ceiling in every mode, so an identity can
    // never stay out longer than the administrator accepted.
    let (seconds, source) = match (policy.mode, reported) {
        (RateLimitCooldownMode::Fixed, _) => (policy.seconds, health::CooldownSource::Fixed),
        (RateLimitCooldownMode::PreferProvider, None) => {
            (policy.seconds, health::CooldownSource::Fallback)
        }
        (RateLimitCooldownMode::ProviderOnly, None) => return None,
        (_, Some(seconds)) if seconds > policy.seconds => {
            (policy.seconds, health::CooldownSource::Capped)
        }
        (_, Some(seconds)) => (seconds, health::CooldownSource::Provider),
    };
    Some((Duration::from_secs(seconds), source))
}

/// The refusal used whenever an Endpoint's Extension is not enabled. Core names
/// the Endpoint type from configuration and never a vendor.
fn endpoint_type_unavailable(endpoint_id: &str) -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        format!(
            "Endpoint '{endpoint_id}' uses an Endpoint type that no enabled Extension provides"
        ),
    )
}

fn unavailable_endpoint_type_message(endpoints: &[ApiEndpoint], state: &AppState) -> String {
    let endpoint_types: Vec<&str> = endpoints
        .iter()
        .filter_map(|endpoint| endpoint.extension_endpoint_type())
        .filter(|endpoint_type| state.extensions.provider_endpoint(endpoint_type).is_none())
        .collect();
    if let Some(endpoint_type) = endpoint_types.first() {
        format!(
            "Endpoint type '{endpoint_type}' is not available because its Extension is not enabled"
        )
    } else {
        "Endpoint type is not available because its Extension is not enabled".to_owned()
    }
}

/// How an Endpoint type without a connected account is described to a caller,
/// in the words of the declaration that owns the identity.
fn missing_identity_message(
    endpoint: &ApiEndpoint,
    declaration: Option<&yabane_extension_api::ProviderEndpointType>,
) -> String {
    match declaration {
        Some(declaration) => format!(
            "Endpoint '{}' has no connected account for {}",
            endpoint.id, declaration.display_name
        ),
        None => format!("Endpoint '{}' has no connected identity", endpoint.id),
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
            // Yabane owns this namespace; an upstream must never forge into it.
            || name.as_str().starts_with("x-yabane-")
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
        let (stage, category, message) = if err.is_connect() {
            (
                "upstream_connect",
                "connect_timeout",
                format!("Could not connect to the Provider{route} before the deadline"),
            )
        } else {
            (
                "upstream_response",
                "timeout",
                format!("Provider request{route} timed out"),
            )
        };
        return RequestFailure::new(stage, category, message);
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
                "Could not connect to the Provider{route}{}",
                detail.unwrap_or_default()
            ),
        );
    }
    RequestFailure::new(
        "upstream_transport",
        "request_failed",
        format!(
            "Provider request{route} failed{}",
            detail.unwrap_or_default()
        ),
    )
}

fn upstream_read_failure(err: &reqwest::Error, streaming: bool) -> RequestFailure {
    if err.is_timeout() {
        return RequestFailure::new(
            if streaming {
                "upstream_stream"
            } else {
                "upstream_response"
            },
            "timeout",
            "Provider response timed out",
        );
    }
    RequestFailure::new(
        if streaming {
            "upstream_stream"
        } else {
            "upstream_response"
        },
        if streaming {
            "interrupted"
        } else {
            "read_failed"
        },
        if streaming {
            "Provider stream ended or could not be converted"
        } else {
            "Could not read the Provider response"
        },
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
        format!("Provider returned HTTP {}", status.as_u16()),
    )
}

fn stream_failure(message: &str) -> RequestFailure {
    let (stage, category, safe_message) =
        if message.contains("memory limit") || message.contains("conversion limit") {
            (
                "protocol_conversion",
                "response_too_large",
                "Provider response exceeded the conversion limit",
            )
        } else if message.contains("8 MiB") {
            (
                "protocol_conversion",
                "frame_too_large",
                "Provider stream exceeded the conversion limit",
            )
        } else if message.contains("valid JSON") || message.contains("convert") {
            (
                "protocol_conversion",
                "invalid_response",
                "Could not convert the Provider stream",
            )
        } else if message.contains("terminal event") {
            (
                "upstream_stream",
                "truncated",
                "Provider stream ended before completion",
            )
        } else {
            (
                "upstream_stream",
                "failed",
                "Provider stream reported a failure",
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
    endpoint_always_streams: bool,
    content_type: &str,
) -> bool {
    (status.is_success() && endpoint_always_streams)
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
    use std::time::Duration;

    use axum::{
        body::Body,
        http::{HeaderMap, HeaderValue, StatusCode},
    };
    use reqwest::header;

    use crate::config::{RateLimitCooldown, RateLimitCooldownMode};

    #[cfg(feature = "extension-openai-subscription")]
    use super::provider_endpoint_base_url;
    use super::{
        ApiSurface, ApiType, Protocol, apply_core_upstream_headers, cooldown_decision,
        copy_response_headers, join_upstream_url, read_body, reported_retry_after,
        response_is_event_stream, sanitize_request_headers, strip_transformed_response_headers,
        upstream_transport_failure,
    };

    /// ENDPOINT-21: a base URL is the Provider's shared API root, and the
    /// Provider versions that root itself. Yabane's operation path must join
    /// below whatever version the root already names instead of stacking a
    /// second version segment on top of it.
    #[test]
    fn upstream_url_does_not_repeat_the_version_a_base_url_already_names() {
        // The version is recognized wherever it sits in the root's path, so a
        // root that names its version and then a surface below it keeps both.
        // This is the deliberate difference from a trailing-segment-only rule:
        // both shapes below would otherwise receive a second `/v1`.
        for (base, operation, expected) in [
            // The reported shape: CodeBuddy serves `/v2/chat/completions`, so a
            // `/v1` root join addressed a route the Provider does not have.
            (
                "https://copilot.tencent.com/v2",
                "/v1/chat/completions",
                "https://copilot.tencent.com/v2/chat/completions",
            ),
            (
                "https://copilot.tencent.com/v2/",
                "/v1/chat/completions?trace=1",
                "https://copilot.tencent.com/v2/chat/completions?trace=1",
            ),
            // A version below a plain prefix belongs to the Provider too.
            (
                "https://open.bigmodel.cn/api/paas/v4",
                "/v1/chat/completions",
                "https://open.bigmodel.cn/api/paas/v4/chat/completions",
            ),
            (
                "https://open.bigmodel.cn/api/paas/v4/saas",
                "/v1/chat/completions",
                "https://open.bigmodel.cn/api/paas/v4/saas/chat/completions",
            ),
            // A suffixed version is still a version.
            (
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "/v1/chat/completions",
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            ),
            // OpenCode Go's nested root keeps its own `/v1`-below-a-prefix shape.
            (
                "https://opencode.ai/zen/go/v1",
                "/v1/models",
                "https://opencode.ai/zen/go/v1/models",
            ),
            // A root that names no version is a plain prefix: the operation path
            // is used exactly as written.
            (
                "https://example.com",
                "/v1/chat/completions",
                "https://example.com/v1/chat/completions",
            ),
            (
                "https://example.com/openai/v1-preview",
                "/v1/chat/completions",
                "https://example.com/openai/v1-preview/v1/chat/completions",
            ),
            (
                "https://example.com/vendors",
                "/v1/chat/completions",
                "https://example.com/vendors/v1/chat/completions",
            ),
            // A path without a leading version segment is never rewritten, so
            // an explicit model-route or Extension path keeps its own meaning.
            (
                "https://copilot.tencent.com/v2",
                "/codex/responses",
                "https://copilot.tencent.com/v2/codex/responses",
            ),
            // An operation path with no leading version segment is appended as
            // written, so a versioned operation path never reaches the Provider
            // twice.
            (
                "https://copilot.tencent.com/v2",
                "/v2/chat/completions",
                "https://copilot.tencent.com/v2/chat/completions",
            ),
        ] {
            assert_eq!(
                join_upstream_url(base, operation),
                expected,
                "{base} + {operation}"
            );
        }
    }

    /// ENDPOINT-21: only a real version segment is treated as one, so a path
    /// that merely resembles a version stays the resource name it is.
    #[test]
    fn upstream_url_version_detection_requires_a_real_version_segment() {
        for (base, operation, expected) in [
            // "v" must be followed by a digit, so `vendors` is a resource name.
            (
                "https://example.com/vendors",
                "/v1/chat/completions",
                "https://example.com/vendors/v1/chat/completions",
            ),
            (
                "https://example.com/api/v",
                "/v1/chat/completions",
                "https://example.com/api/v/v1/chat/completions",
            ),
            // A base URL that already names Yabane's own version is normalized
            // to exactly one copy of it rather than two.
            (
                "https://example.com/v1",
                "/v1/chat/completions",
                "https://example.com/v1/chat/completions",
            ),
            // A query on a version-less operation path survives unchanged.
            (
                "https://example.com",
                "/v1/models?limit=1000",
                "https://example.com/v1/models?limit=1000",
            ),
            // The query is kept when the version segment is dropped.
            (
                "https://example.com/v1",
                "/v1/models?limit=1000",
                "https://example.com/v1/models?limit=1000",
            ),
        ] {
            assert_eq!(
                join_upstream_url(base, operation),
                expected,
                "{base} + {operation}"
            );
        }
    }

    /// ENDPOINT-21: a version is recognized wherever it sits in the root, not
    /// only as the last segment. A trailing-segment-only rule would call
    /// `/api/paas/v4/saas` at `/api/paas/v4/saas/v1/chat/completions` and
    /// `/v1beta/openai` at `/v1beta/openai/v1/chat/completions`; this test pins
    /// the choice not to do that. A Provider whose real root does sit below a
    /// version segment is reached by naming that whole prefix, which the join
    /// normalizes to one copy of the version instead of refusing the root.
    #[test]
    fn upstream_url_keeps_a_version_that_is_not_the_last_path_segment() {
        for (base, expected) in [
            (
                "https://open.bigmodel.cn/api/paas/v4/saas",
                "https://open.bigmodel.cn/api/paas/v4/saas/chat/completions",
            ),
            (
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            ),
            (
                "https://example.com/v1/tenant-a/v1",
                "https://example.com/v1/tenant-a/v1/chat/completions",
            ),
        ] {
            assert_eq!(
                join_upstream_url(base, "/v1/chat/completions"),
                expected,
                "{base}"
            );
        }
    }

    /// PROXY-41: crossing the buffered-body ceiling is a Yabane-authored `413`,
    /// and a body at the limit still passes.
    #[tokio::test]
    async fn request_body_past_the_limit_is_rejected_as_a_yabane_413() {
        assert!(
            read_body(Body::from("12345"), 4).await.is_none(),
            "a body past the limit must be refused"
        );
        assert!(
            read_body(Body::from("1234"), 4).await.is_some(),
            "a body at the limit is accepted"
        );
        let response = super::request_body_too_large();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the error body");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("error body JSON");
        assert_eq!(value["error"]["message"], "Request body is too large");
        assert_eq!(value["error"]["type"], "yabane_error");
    }

    #[test]
    fn cooldown_decision_records_the_source_used_at_response_time() {
        // ENDPOINT-42: never reinterpret a historical decision under a new policy.
        use crate::health::CooldownSource;
        for (mode, reported, expected_seconds, expected_source) in [
            (
                RateLimitCooldownMode::Fixed,
                Some(120),
                300,
                CooldownSource::Fixed,
            ),
            (
                RateLimitCooldownMode::Fixed,
                None,
                300,
                CooldownSource::Fixed,
            ),
            (
                RateLimitCooldownMode::PreferProvider,
                Some(120),
                120,
                CooldownSource::Provider,
            ),
            (
                RateLimitCooldownMode::PreferProvider,
                Some(300),
                300,
                CooldownSource::Provider,
            ),
            (
                RateLimitCooldownMode::PreferProvider,
                Some(600),
                300,
                CooldownSource::Capped,
            ),
            (
                RateLimitCooldownMode::PreferProvider,
                None,
                300,
                CooldownSource::Fallback,
            ),
            (
                RateLimitCooldownMode::PreferProvider,
                Some(0),
                0,
                CooldownSource::Provider,
            ),
            (
                RateLimitCooldownMode::ProviderOnly,
                Some(0),
                0,
                CooldownSource::Provider,
            ),
            (
                RateLimitCooldownMode::ProviderOnly,
                Some(600),
                300,
                CooldownSource::Capped,
            ),
        ] {
            assert_eq!(
                cooldown_decision(&RateLimitCooldown { seconds: 300, mode }, reported),
                Some((Duration::from_secs(expected_seconds), expected_source))
            );
        }
    }

    #[test]
    fn reported_retry_after_reads_only_an_integer_number_of_seconds() {
        let mut headers = header::HeaderMap::new();
        assert_eq!(reported_retry_after(&headers), None);

        headers.insert(header::RETRY_AFTER, HeaderValue::from_static("120"));
        assert_eq!(reported_retry_after(&headers), Some(120));
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static(" 120 "));
        assert_eq!(reported_retry_after(&headers), Some(120));
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static("0"));
        assert_eq!(reported_retry_after(&headers), Some(0));

        // A date, a negative number, and an arbitrary value are not delays.
        for value in ["Wed, 21 Oct 2015 07:28:00 GMT", "-1", "1.5", "not a delay"] {
            headers.insert(header::RETRY_AFTER, HeaderValue::from_static(value));
            assert_eq!(reported_retry_after(&headers), None);
        }
    }

    #[test]
    fn cooldown_duration_reads_the_delay_source_under_the_configured_ceiling() {
        let cooldown_duration = |policy: &RateLimitCooldown, reported| {
            cooldown_decision(policy, reported).map(|(duration, _)| duration)
        };
        let policy = |seconds, mode| RateLimitCooldown { seconds, mode };
        let reported = Duration::from_secs(120);
        let configured = Duration::from_secs(300);

        // A fixed policy ignores anything the Provider reports.
        assert_eq!(
            cooldown_duration(&policy(300, RateLimitCooldownMode::Fixed), Some(120)),
            Some(configured)
        );
        assert_eq!(
            cooldown_duration(&policy(300, RateLimitCooldownMode::Fixed), None),
            Some(configured)
        );

        // The Provider-first policy prefers the reported delay and falls back to
        // the configured length, which is also the ceiling in both directions.
        assert_eq!(
            cooldown_duration(
                &policy(300, RateLimitCooldownMode::PreferProvider),
                Some(120)
            ),
            Some(reported)
        );
        assert_eq!(
            cooldown_duration(
                &policy(60, RateLimitCooldownMode::PreferProvider),
                Some(120)
            ),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            cooldown_duration(&policy(300, RateLimitCooldownMode::PreferProvider), None),
            Some(configured)
        );

        // The Provider-only policy never invents a length for the Provider, so
        // an unusable or absent delay arms nothing at all.
        assert_eq!(
            cooldown_duration(&policy(300, RateLimitCooldownMode::ProviderOnly), Some(120)),
            Some(reported)
        );
        assert_eq!(
            cooldown_duration(&policy(60, RateLimitCooldownMode::ProviderOnly), Some(120)),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            cooldown_duration(&policy(300, RateLimitCooldownMode::ProviderOnly), None),
            None
        );

        // The configured length is the ceiling even when the Provider asks for
        // longer, so a cooldown can never outlast what the administrator accepted.
        assert_eq!(
            cooldown_duration(
                &policy(300, RateLimitCooldownMode::PreferProvider),
                Some(999_999_999)
            ),
            Some(configured)
        );
        assert_eq!(
            cooldown_duration(
                &policy(
                    RateLimitCooldown::MAX_SECONDS,
                    RateLimitCooldownMode::ProviderOnly
                ),
                Some(999_999_999)
            ),
            Some(Duration::from_secs(RateLimitCooldown::MAX_SECONDS))
        );

        // A policy of zero seconds disables the cooldown entirely.
        assert_eq!(
            cooldown_duration(&policy(0, RateLimitCooldownMode::PreferProvider), Some(120)),
            None
        );
        assert_eq!(
            cooldown_duration(&policy(0, RateLimitCooldownMode::PreferProvider), None),
            None
        );
    }

    #[test]
    fn declared_always_stream_endpoints_are_not_misclassified_on_errors() {
        assert!(!response_is_event_stream(
            axum::http::StatusCode::BAD_REQUEST,
            true,
            "application/json",
        ));
        assert!(response_is_event_stream(
            axum::http::StatusCode::OK,
            true,
            "",
        ));
        assert!(response_is_event_stream(
            axum::http::StatusCode::BAD_GATEWAY,
            true,
            "text/event-stream; charset=utf-8",
        ));
    }

    #[cfg(feature = "extension-openai-subscription")]
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

    #[cfg(feature = "extension-openai-subscription")]
    #[test]
    fn codex_adapter_applies_required_response_fields() {
        let mut body =
            br#"{"model":"gpt-5.4","input":"hello","stream":true,"store":true,"temperature":0.2,"top_p":0.9,"truncation":"disabled","metadata":{"trace":"caller"},"service_tier":"auto","user":"caller","safety_identifier":"caller-id","prompt_cache_options":{"mode":"implicit"},"prompt_cache_retention":"24h","previous_response_id":"resp_caller"}"#.to_vec();
        yabane_extension_openai_subscription::adapt_body(&mut body);
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["stream"], true);
        assert_eq!(value["store"], false);
        assert_eq!(value["instructions"], "You are a helpful assistant.");
        assert_eq!(value["input"][0]["role"], "user");
        assert_eq!(value["input"][0]["content"][0]["text"], "hello");
        for dropped in [
            "max_output_tokens",
            "temperature",
            "top_p",
            "truncation",
            "metadata",
            "service_tier",
            "user",
            "safety_identifier",
            "prompt_cache_options",
            "prompt_cache_retention",
        ] {
            assert!(
                value.get(dropped).is_none(),
                "unsupported field '{dropped}' must not reach Codex"
            );
        }
        // Dropping this one would silently discard the caller's conversation.
        assert_eq!(value["previous_response_id"], "resp_caller");
        assert_eq!(value["text"]["verbosity"], "low");
        assert_eq!(value["tool_choice"], "auto");
        assert_eq!(value["parallel_tool_calls"], true);
        assert_eq!(value["include"][0], "reasoning.encrypted_content");
    }

    #[cfg(feature = "extension-openai-subscription")]
    #[test]
    fn codex_adapter_folds_array_content_system_messages_into_instructions() {
        // VS Code's native Responses mode sends the system prompt as a message
        // item whose content is a list of input_text parts, which the Codex
        // backend refuses with `400 System messages are not allowed`.
        let mut body = br#"{"model":"gpt-6-astra","input":[{"type":"message","role":"system","content":[{"type":"input_text","text":"Be terse."}]},{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]},{"role":"system","content":"Late note."}]}"#.to_vec();
        yabane_extension_openai_subscription::adapt_body(&mut body);
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["instructions"], "Be terse.\n\nLate note.");
        assert_eq!(value["input"].as_array().unwrap().len(), 1);
        assert_eq!(value["input"][0]["role"], "user");
    }

    #[cfg(feature = "extension-openai-subscription")]
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

    #[cfg(feature = "extension-openai-subscription")]
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
    fn an_endpoint_type_serves_only_the_caller_surfaces_it_declares() {
        // The declaration decides which caller APIs an Extension-owned Endpoint
        // accepts and which protocol Core speaks upstream for it.
        let registry = crate::extensions::tests::registry_with_an_acme_endpoint();
        assert!(ApiSurface::OpenAiResponses.supports(ApiType::Extension("acme_plan"), &registry));
        assert!(!ApiSurface::OpenAiChat.supports(ApiType::Extension("acme_plan"), &registry));
        assert!(!ApiSurface::Anthropic.supports(ApiType::Extension("acme_plan"), &registry));
        assert!(!ApiSurface::OpenAiResponses.supports(
            ApiType::Extension("acme_plan"),
            &crate::extensions::ExtensionRegistry::without_endpoint_types()
        ));
    }

    #[test]
    fn proxy_does_not_forward_caller_cookies_or_upstream_set_cookies() {
        let mut request_headers = HeaderMap::new();
        request_headers.insert("cookie", HeaderValue::from_static("yabane_session=private"));
        let sanitized = sanitize_request_headers(
            request_headers,
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
        upstream_headers.insert(
            reqwest::header::HeaderName::from_static("x-yabane-error-origin"),
            reqwest::header::HeaderValue::from_static("yabane"),
        );
        upstream_headers.insert(
            reqwest::header::HeaderName::from_static("x-yabane-request-id"),
            reqwest::header::HeaderValue::from_static("req-forged"),
        );
        let mut response_headers = HeaderMap::new();
        copy_response_headers(&mut response_headers, &upstream_headers);
        assert!(!response_headers.contains_key("set-cookie"));
        assert_eq!(response_headers["content-type"], "application/json");
        assert_eq!(response_headers["content-encoding"], "gzip");
        assert!(!response_headers.contains_key("x-yabane-error-origin"));
        assert!(!response_headers.contains_key("x-yabane-request-id"));
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
        let mut sanitized =
            sanitize_request_headers(headers, (Protocol::OpenAiChat, Protocol::AnthropicMessages));
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
    fn core_sanitization_leaves_provider_specific_headers_to_extensions() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer caller"));
        headers.insert("cookie", HeaderValue::from_static("session=private"));
        // Core removes the two credential headers above on every request, but it must
        // not decide what any of the following mean: they are provider concerns owned
        // by that provider's Extension, which is what keeps Core a transparent proxy.
        headers.insert("content-encoding", HeaderValue::from_static("zstd"));
        headers.insert("user-agent", HeaderValue::from_static("caller-agent"));
        headers.insert("session-id", HeaderValue::from_static("caller-session"));
        headers.insert(
            "x-client-request-id",
            HeaderValue::from_static("caller-request"),
        );
        headers.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.7"));
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.7"));

        let sanitized = sanitize_request_headers(
            headers,
            (Protocol::OpenAiResponses, Protocol::OpenAiResponses),
        );

        assert!(!sanitized.contains_key("authorization"));
        assert!(!sanitized.contains_key("cookie"));
        assert_eq!(sanitized["content-encoding"], "zstd");
        assert_eq!(sanitized["user-agent"], "caller-agent");
        assert_eq!(sanitized["session-id"], "caller-session");
        assert_eq!(sanitized["x-client-request-id"], "caller-request");
        assert_eq!(sanitized["cf-connecting-ip"], "203.0.113.7");
        assert_eq!(sanitized["x-forwarded-for"], "203.0.113.7");
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

    #[cfg(feature = "extension-openai-subscription")]
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
            (Protocol::OpenAiResponses, Protocol::OpenAiResponses),
        );
        apply_core_upstream_headers(
            &mut sanitized,
            ApiType::Extension("openai_codex"),
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
                    kind: "openai_subscription",
                    material: yabane_extension_api::ProviderEndpointMaterial::Subscription {
                        access_token: "access-token",
                        account_id: "account-123",
                    },
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

use axum::{
    http::{HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Correlates a proxy response with its Activity record. Present on every
/// response produced by the proxy handler, successful or failed. Requests that
/// never reached it (for example gateway API-key rejection in the auth
/// middleware) have no Activity record and therefore no ID.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-yabane-request-id");

/// Names the author of an error response body. `upstream` means the body bytes
/// are the provider's own error verbatim; `yabane` means Yabane wrote it, which
/// covers both failures that never reached an Endpoint and failures Yabane had
/// to re-render. It attributes the message, not the blame.
pub const ERROR_ORIGIN_HEADER: HeaderName = HeaderName::from_static("x-yabane-error-origin");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorOrigin {
    Yabane,
    Upstream,
}

impl ErrorOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Yabane => "yabane",
            Self::Upstream => "upstream",
        }
    }
}

pub fn attach_request_id(response: &mut Response, request_id: &str) {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
}

pub fn set_error_origin(response: &mut Response, origin: ErrorOrigin) {
    response.headers_mut().insert(
        ERROR_ORIGIN_HEADER,
        HeaderValue::from_static(origin.as_str()),
    );
}

/// Sets the origin unless the producing code already named it.
pub fn default_error_origin(response: &mut Response, origin: ErrorOrigin) {
    if !response.headers().contains_key(&ERROR_ORIGIN_HEADER) {
        set_error_origin(response, origin);
    }
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

pub fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    api_error_with_type(status, message, "yabane_error")
}

pub fn api_error_with_type(
    status: StatusCode,
    message: impl Into<String>,
    kind: &'static str,
) -> Response {
    (
        status,
        axum::Json(ApiError {
            error: ApiErrorBody {
                message: message.into(),
                kind,
            },
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yabane_owned_headers_name_the_author_and_can_be_narrowed() {
        let mut response = api_error(StatusCode::BAD_GATEWAY, "Could not connect to upstream");
        attach_request_id(&mut response, "req-01");
        default_error_origin(&mut response, ErrorOrigin::Yabane);
        assert_eq!(response.headers()[&REQUEST_ID_HEADER], "req-01");
        assert_eq!(response.headers()[&ERROR_ORIGIN_HEADER], "yabane");

        // The passthrough path names its own author; a later default must not overwrite it.
        set_error_origin(&mut response, ErrorOrigin::Upstream);
        default_error_origin(&mut response, ErrorOrigin::Yabane);
        assert_eq!(response.headers()[&ERROR_ORIGIN_HEADER], "upstream");
    }
}

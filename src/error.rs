use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

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

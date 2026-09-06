use axum::{http::header, response::IntoResponse};

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_CSS: &str = include_str!("../web/app.css");
const APP_JS: &str = include_str!("../web/app.js");
const API_DOCS: &str = include_str!("../web/docs.html");
const OPENAPI_SPEC: &str = include_str!("../web/openapi.json");
const FAVICON: &str = include_str!("../web/favicon.svg");
const UBUNTU_SANS_REGULAR: &[u8] = include_bytes!("../web/fonts/ubuntu-sans-regular.woff2");
const UBUNTU_SANS_MEDIUM: &[u8] = include_bytes!("../web/fonts/ubuntu-sans-medium.woff2");

pub async fn api_docs() -> impl IntoResponse {
    no_store("text/html; charset=utf-8", API_DOCS)
}

pub async fn openapi_spec() -> impl IntoResponse {
    no_store("application/json; charset=utf-8", OPENAPI_SPEC)
}

pub async fn index() -> impl IntoResponse {
    no_store("text/html; charset=utf-8", INDEX_HTML)
}

pub async fn css() -> impl IntoResponse {
    no_store("text/css; charset=utf-8", APP_CSS)
}

pub async fn js() -> impl IntoResponse {
    no_store("text/javascript; charset=utf-8", APP_JS)
}

pub async fn favicon() -> impl IntoResponse {
    no_store("image/svg+xml", FAVICON)
}

pub async fn ubuntu_sans_regular() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "font/woff2")], UBUNTU_SANS_REGULAR)
}

pub async fn ubuntu_sans_medium() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "font/woff2")], UBUNTU_SANS_MEDIUM)
}

pub async fn health() -> &'static str {
    "ok"
}

fn no_store(content_type: &'static str, contents: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
        ],
        contents,
    )
}

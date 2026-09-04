use std::{env, net::SocketAddr, sync::Arc};

use axum::{
    Router, middleware,
    routing::{delete, get, patch, post},
};
use tokio::{net::TcpListener, sync::RwLock};
use tracing::info;
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

mod activity;
mod admin_user;
mod auth;
mod config;
mod control;
mod error;
mod gateway;
mod models;
mod routes;
mod storage;
mod usage;
mod web;

use auth::load_auth;
use config::{AppState, load_providers};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let log_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .with_env_var("YABANE_LOG")
        .from_env()
        .expect("YABANE_LOG must contain a valid tracing filter");
    tracing_subscriber::fmt().with_env_filter(log_filter).init();

    let providers = load_providers().await.expect("load provider configuration");
    let activity = activity::ActivityStore::load()
        .await
        .expect("load activity");
    activity.start_flusher();
    let routes = routes::RouteStore::load().await.expect("load routes");
    let admin = admin_user::load_admin().await.expect("load administrator");
    let auth = load_auth()
        .await
        .expect("load authentication configuration");
    let state = AppState {
        client: reqwest::Client::builder()
            .pool_max_idle_per_host(64)
            .tcp_nodelay(true)
            .build()
            .expect("build HTTP client"),
        providers: Arc::new(RwLock::new(providers)),
        auth: Arc::new(RwLock::new(auth)),
        activity,
        routes,
        admin: admin_user::AdminState {
            user: Arc::new(RwLock::new(admin)),
            ..admin_user::AdminState::default()
        },
    };

    let inference = Router::new()
        .route("/v1/models", get(models::list_models))
        .route("/v1/chat/completions", post(gateway::proxy_openai))
        .route("/v1/responses", post(gateway::proxy_openai))
        .route("/v1/messages", post(gateway::proxy_anthropic))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authorize,
        ));

    let profile_admin = Router::new()
        .route("/admin/profile", patch(admin_user::update_profile))
        .route(
            "/admin/management-keys",
            get(admin_user::list_management_api_keys).post(admin_user::create_management_api_key),
        )
        .route(
            "/admin/management-keys/{id}",
            delete(admin_user::delete_management_api_key),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            admin_user::require_browser_admin,
        ));

    let protected_admin = control::router(state.clone());

    let app = Router::new()
        .route("/", get(web::index))
        .route("/login", get(web::index))
        .route("/home", get(web::index))
        .route("/providers", get(web::index))
        .route("/providers/{id}", get(web::index))
        .route("/model-routing", get(web::index))
        .route("/api-access", get(web::index))
        .route("/activity", get(web::index))
        .route("/management-api", get(web::index))
        .route("/docs", get(web::api_docs))
        .route("/openapi.json", get(web::openapi_spec))
        .route("/app.css", get(web::css))
        .route("/app.js", get(web::js))
        .route(
            "/fonts/ubuntu-sans-regular.woff2",
            get(web::ubuntu_sans_regular),
        )
        .route(
            "/fonts/ubuntu-sans-medium.woff2",
            get(web::ubuntu_sans_medium),
        )
        .route("/healthz", get(web::health))
        .route("/admin/session", get(admin_user::session))
        .route("/admin/turnstile-config", get(admin_user::turnstile_config))
        .route("/admin/setup", post(admin_user::setup))
        .route("/admin/login", post(admin_user::login))
        .route("/admin/logout", post(admin_user::logout))
        .merge(profile_admin)
        .merge(protected_admin)
        .merge(inference)
        .with_state(state.clone());

    let address: SocketAddr = env::var("YABANE_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse()
        .expect("YABANE_ADDR must be an address");
    let listener = TcpListener::bind(address).await.expect("bind server");
    let browser_host = match address.ip() {
        std::net::IpAddr::V4(ip) if ip.is_unspecified() => "127.0.0.1".to_owned(),
        std::net::IpAddr::V6(ip) if ip.is_unspecified() => "[::1]".to_owned(),
        ip => ip.to_string(),
    };
    let admin_url = format!("http://{browser_host}:{}/", address.port());
    info!(%address, %admin_url, "Yabane is ready");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("serve Yabane");
    state.activity.flush().await;
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

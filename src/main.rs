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

const HELP: &str = "Yabane — a clear, reliable gateway to every LLM

Usage: yabane [OPTIONS]

Options:
  --addr <ADDRESS>  Listen address [default: 127.0.0.1:8080]
  --log <FILTER>    Tracing filter [env: YABANE_LOG] [default: info]
  -h, --help        Print help
  -V, --version     Print commit information

Environment:
  YABANE_ACTIVITY_RETENTION_DAYS  Initial Activity retention before a setting is saved [default: 30]
  TURNSTILE_SITE_KEY              Cloudflare Turnstile widget site key
  TURNSTILE_SECRET                Cloudflare Turnstile server secret
  TURNSTILE_HOSTNAMES             Comma-separated accepted hostnames

Yabane also reads a .env file in the current directory for non-listener settings.
Persistent configuration is stored in data/.

Examples:
  yabane --addr 127.0.0.1:9090
  yabane --addr 0.0.0.0:8080 --log debug
  YABANE_ACTIVITY_RETENTION_DAYS=90 yabane
";

#[derive(Default)]
struct Cli {
    address: Option<String>,
    log_filter: Option<String>,
}

impl Cli {
    fn parse() -> Self {
        let mut cli = Self::default();
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => {
                    print!("{HELP}");
                    std::process::exit(0);
                }
                "-V" | "--version" => {
                    println!(
                        "yabane {} ({})",
                        env!("YABANE_GIT_COMMIT"),
                        env!("YABANE_GIT_COMMIT_TIME")
                    );
                    std::process::exit(0);
                }
                "--addr" => cli.address = Some(required_value(&mut args, "--addr")),
                "--log" => cli.log_filter = Some(required_value(&mut args, "--log")),
                _ if arg.starts_with("--addr=") => {
                    cli.address = Some(arg["--addr=".len()..].to_owned())
                }
                _ if arg.starts_with("--log=") => {
                    cli.log_filter = Some(arg["--log=".len()..].to_owned())
                }
                _ => cli_error(&format!("unexpected argument '{arg}'")),
            }
        }
        cli
    }
}

fn required_value(args: &mut impl Iterator<Item = String>, option: &str) -> String {
    args.next()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| cli_error(&format!("a value is required for '{option}'")))
}

fn cli_error(message: &str) -> ! {
    eprintln!("error: {message}\n\nFor more information, try '--help'.");
    std::process::exit(2);
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    dotenvy::dotenv().ok();

    let log_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .parse(
            cli.log_filter
                .or_else(|| env::var("YABANE_LOG").ok())
                .unwrap_or_else(|| "info".to_owned()),
        )
        .unwrap_or_else(|error| cli_error(&format!("invalid log filter: {error}")));
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
        .route("/favicon.svg", get(web::favicon))
        .route(
            "/fonts/ubuntu-sans-regular.woff2",
            get(web::ubuntu_sans_regular),
        )
        .route(
            "/fonts/ubuntu-sans-medium.woff2",
            get(web::ubuntu_sans_medium),
        )
        .route("/healthz", get(web::health))
        .route("/about", get(web::about))
        .route("/admin/session", get(admin_user::session))
        .route("/admin/turnstile-config", get(admin_user::turnstile_config))
        .route("/admin/setup", post(admin_user::setup))
        .route("/admin/login", post(admin_user::login))
        .route("/admin/logout", post(admin_user::logout))
        .merge(profile_admin)
        .merge(protected_admin)
        .merge(inference)
        .with_state(state.clone());

    let address: SocketAddr = cli
        .address
        .unwrap_or_else(|| "127.0.0.1:8080".to_owned())
        .parse()
        .unwrap_or_else(|error| cli_error(&format!("invalid --addr value: {error}")));
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

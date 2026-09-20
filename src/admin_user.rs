use std::{collections::HashMap, io::ErrorKind, path::Path, sync::Arc};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::Response,
};
use password_hash::{SaltString, rand_core::OsRng};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::warn;

use crate::{
    auth::{constant_time_eq, hash_secret},
    config::AppState,
    error::api_error,
};

const ADMIN_FILE: &str = "data/admin.json";
const SESSION_COOKIE: &str = "yabane_session";
const SESSION_TTL_SECONDS: u64 = 24 * 60 * 60;
const TURNSTILE_SITEVERIFY: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
const TURNSTILE_TEST_SITE_KEY: &str = "1x00000000000000000000AA";
const TURNSTILE_TEST_SECRET_PREFIX: &str = "1x0000000000000000000000000000000";

#[derive(Clone, Default)]
pub struct AdminState {
    pub user: Arc<RwLock<Option<AdminUser>>>,
    pub(crate) sessions: Arc<RwLock<HashMap<String, u64>>>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct AdminUser {
    pub username: String,
    pub email: String,
    password_hash: String,
    #[serde(default)]
    pub management_api_keys: Vec<ManagementApiKey>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ManagementApiKey {
    pub id: String,
    pub name: String,
    pub secret_hash: String,
    pub prefix: String,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub last_used_at: Option<u64>,
}

#[derive(Deserialize)]
pub struct CreateManagementApiKey {
    pub name: String,
    pub expires_at: Option<u64>,
}

#[derive(Serialize)]
pub struct ManagementApiKeyView {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub last_used_at: Option<u64>,
}

#[derive(Serialize)]
pub struct CreatedManagementApiKey {
    pub api_key: ManagementApiKeyView,
    pub secret: String,
}

impl From<&ManagementApiKey> for ManagementApiKeyView {
    fn from(key: &ManagementApiKey) -> Self {
        Self {
            id: key.id.clone(),
            name: key.name.clone(),
            prefix: key.prefix.clone(),
            created_at: key.created_at,
            expires_at: key.expires_at,
            last_used_at: key.last_used_at,
        }
    }
}

#[derive(Deserialize)]
pub struct Credentials {
    pub username: String,
    pub email: Option<String>,
    pub password: String,
    #[serde(default)]
    pub turnstile_token: String,
}

#[derive(Deserialize)]
pub struct UpdateProfile {
    pub username: String,
    pub email: String,
    #[serde(default)]
    pub current_password: String,
    #[serde(default)]
    pub new_password: String,
}

#[derive(Serialize)]
pub struct SessionView {
    pub configured: bool,
    pub authenticated: bool,
    pub username: Option<String>,
    pub email: Option<String>,
}

#[derive(Serialize)]
pub struct TurnstileConfig {
    pub enabled: bool,
    pub site_key: Option<String>,
}

#[derive(Deserialize)]
struct TurnstileResponse {
    success: bool,
    action: Option<String>,
    hostname: Option<String>,
}

pub async fn load_admin() -> Result<Option<AdminUser>, String> {
    let contents = match tokio::fs::read(ADMIN_FILE).await {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("read {ADMIN_FILE}: {err}")),
    };
    let user: AdminUser =
        serde_json::from_slice(&contents).map_err(|err| format!("parse {ADMIN_FILE}: {err}"))?;
    validate_management_key_identities(&user)?;
    Ok(Some(user))
}

fn validate_management_key_identities(user: &AdminUser) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    if user
        .management_api_keys
        .iter()
        .any(|key| key.id.is_empty() || !ids.insert(key.id.as_str()))
    {
        return Err(format!(
            "parse {ADMIN_FILE}: Management API key IDs must be non-empty and unique"
        ));
    }
    Ok(())
}

pub async fn turnstile_config() -> axum::Json<TurnstileConfig> {
    let site_key = turnstile_credentials().map(|(_, site_key)| site_key);
    axum::Json(TurnstileConfig {
        enabled: site_key.is_some(),
        site_key,
    })
}

pub async fn session(State(state): State<AppState>, request: Request) -> axum::Json<SessionView> {
    let user = state.admin.user.read().await.clone();
    let authenticated = browser_session_valid(&state, request.headers()).await;
    axum::Json(SessionView {
        configured: user.is_some(),
        authenticated,
        username: authenticated
            .then(|| user.as_ref().map(|user| user.username.clone()))
            .flatten(),
        email: authenticated
            .then(|| user.as_ref().map(|user| user.email.clone()))
            .flatten(),
    })
}

pub async fn setup(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<Credentials>,
) -> Response {
    if state.admin.user.read().await.is_some() {
        return api_error(StatusCode::CONFLICT, "Administrator is already configured");
    }
    let Some(email) = input
        .email
        .as_deref()
        .map(str::trim)
        .filter(|email| email.contains('@'))
    else {
        return api_error(StatusCode::BAD_REQUEST, "A valid email is required");
    };
    if input.username.trim().is_empty() || input.password.len() < 8 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Username and a password of at least 8 characters are required",
        );
    }
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(input.password.as_bytes(), &salt)
        .expect("hash password")
        .to_string();
    let user = AdminUser {
        username: input.username.trim().to_owned(),
        email: email.to_owned(),
        password_hash,
        management_api_keys: Vec::new(),
    };
    let mut current = state.admin.user.write().await;
    if current.is_some() {
        return api_error(StatusCode::CONFLICT, "Administrator is already configured");
    }
    if let Err(err) = save_admin(&user).await {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not save administrator: {err}"),
        );
    }
    *current = Some(user);
    drop(current);
    create_session(&state).await
}

pub async fn login(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<Credentials>,
) -> Response {
    if let Err(response) = verify_turnstile(&state, &input.turnstile_token, "login").await {
        return *response;
    }
    let user = state.admin.user.read().await.clone();
    let valid = user.as_ref().is_some_and(|user| {
        (user.username == input.username.trim() || user.email == input.username.trim())
            && PasswordHash::new(&user.password_hash).is_ok_and(|hash| {
                Argon2::default()
                    .verify_password(input.password.as_bytes(), &hash)
                    .is_ok()
            })
    });
    if !valid {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "Invalid username, email, or password",
        );
    }
    create_session(&state).await
}

pub async fn update_profile(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<UpdateProfile>,
) -> Response {
    let username = input.username.trim();
    let email = input.email.trim();
    if username.is_empty() || !email.contains('@') {
        return api_error(
            StatusCode::BAD_REQUEST,
            "A username and valid email are required",
        );
    }
    if !input.new_password.is_empty() && input.new_password.len() < 8 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "New password must contain at least 8 characters",
        );
    }

    let mut user_guard = state.admin.user.write().await;
    let Some(current) = user_guard.as_ref() else {
        return api_error(StatusCode::NOT_FOUND, "Administrator is not configured");
    };
    let password_hash = if input.new_password.is_empty() {
        current.password_hash.clone()
    } else {
        let valid = PasswordHash::new(&current.password_hash).is_ok_and(|hash| {
            Argon2::default()
                .verify_password(input.current_password.as_bytes(), &hash)
                .is_ok()
        });
        if !valid {
            return api_error(StatusCode::FORBIDDEN, "Current password is incorrect");
        }
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(input.new_password.as_bytes(), &salt)
            .expect("hash password")
            .to_string()
    };
    let updated = AdminUser {
        username: username.to_owned(),
        email: email.to_owned(),
        password_hash,
        management_api_keys: current.management_api_keys.clone(),
    };
    if let Err(err) = save_admin(&updated).await {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not save administrator: {err}"),
        );
    }
    *user_guard = Some(updated);
    StatusCode::NO_CONTENT.into_response()
}

pub async fn list_management_api_keys(
    State(state): State<AppState>,
) -> axum::Json<Vec<ManagementApiKeyView>> {
    let keys = state
        .admin
        .user
        .read()
        .await
        .as_ref()
        .map(|user| {
            user.management_api_keys
                .iter()
                .map(ManagementApiKeyView::from)
                .collect()
        })
        .unwrap_or_default();
    axum::Json(keys)
}

pub async fn create_management_api_key(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<CreateManagementApiKey>,
) -> Response {
    let name = input.name.trim();
    if name.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "API key name is required");
    }
    if input
        .expires_at
        .is_some_and(|expires| expires <= crate::auth::now())
    {
        return api_error(StatusCode::BAD_REQUEST, "Expiration must be in the future");
    }

    let secret = management_secret();
    let id = format!("mak_{}", &hash_secret(&secret)[..16]);
    let key = ManagementApiKey {
        id,
        name: name.to_owned(),
        secret_hash: hash_secret(&secret),
        prefix: format!("{}…{}", &secret[..12], &secret[secret.len() - 4..]),
        created_at: crate::auth::now(),
        expires_at: input.expires_at,
        last_used_at: None,
    };
    let mut user = state.admin.user.write().await;
    let Some(user) = user.as_mut() else {
        return api_error(StatusCode::NOT_FOUND, "Administrator is not configured");
    };
    user.management_api_keys.push(key.clone());
    if let Err(err) = save_admin(user).await {
        user.management_api_keys
            .retain(|existing| existing.id != key.id);
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not save management API key: {err}"),
        );
    }
    axum::Json(CreatedManagementApiKey {
        api_key: ManagementApiKeyView::from(&key),
        secret,
    })
    .into_response()
}

pub async fn delete_management_api_key(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let mut user = state.admin.user.write().await;
    let Some(user) = user.as_mut() else {
        return api_error(StatusCode::NOT_FOUND, "Administrator is not configured");
    };
    let mut updated = user.clone();
    let before = updated.management_api_keys.len();
    updated.management_api_keys.retain(|key| key.id != id);
    if updated.management_api_keys.len() == before {
        return api_error(StatusCode::NOT_FOUND, "Management API key not found");
    }
    match save_admin(&updated).await {
        Ok(()) => {
            *user = updated;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not revoke management API key: {err}"),
        ),
    }
}

pub async fn logout(State(state): State<AppState>, request: Request) -> Response {
    if let Some(token) = session_token(request.headers()) {
        state.admin.sessions.write().await.remove(token);
    }
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, expired_cookie())],
    )
        .into_response()
}

pub async fn require_browser_admin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let authorized = browser_session_valid(&state, request.headers()).await;
    if !authorized {
        return api_error(StatusCode::UNAUTHORIZED, "Administrator session required");
    }
    next.run(request).await
}

pub async fn require_admin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let session_authorized = browser_session_valid(&state, request.headers()).await;
    if session_authorized || authenticate_management_key(&state, request.headers()).await {
        return next.run(request).await;
    }
    api_error(
        StatusCode::UNAUTHORIZED,
        "Administrator session or Management API key required",
    )
}

async fn create_session(state: &AppState) -> Response {
    let mut random = [0_u8; 32];
    rand::rng().fill_bytes(&mut random);
    let token = hex(&random);
    let now = crate::auth::now();
    let mut sessions = state.admin.sessions.write().await;
    sessions.retain(|_, created_at| now.saturating_sub(*created_at) < SESSION_TTL_SECONDS);
    sessions.insert(token.clone(), now);
    drop(sessions);
    (
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            format!(
                "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_TTL_SECONDS}"
            ),
        )],
    )
        .into_response()
}

async fn browser_session_valid(state: &AppState, headers: &axum::http::HeaderMap) -> bool {
    let Some(token) = session_token(headers) else {
        return false;
    };
    let now = crate::auth::now();
    let mut sessions = state.admin.sessions.write().await;
    let valid = sessions
        .get(token)
        .is_some_and(|created_at| now.saturating_sub(*created_at) < SESSION_TTL_SECONDS);
    if !valid {
        sessions.remove(token);
    }
    valid
}

async fn verify_turnstile(
    state: &AppState,
    token: &str,
    action: &str,
) -> Result<(), Box<Response>> {
    let Some((secret, _)) = turnstile_credentials() else {
        return Ok(());
    };
    if token.is_empty() || token.len() > 2048 {
        return Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "Turnstile verification required",
        )));
    }
    let testing = secret.starts_with(TURNSTILE_TEST_SECRET_PREFIX);
    let form = vec![("secret", secret), ("response", token.to_owned())];
    let result = state
        .client
        .post(TURNSTILE_SITEVERIFY)
        .form(&form)
        .send()
        .await
        .map_err(|_| {
            Box::new(api_error(
                StatusCode::FORBIDDEN,
                "Turnstile verification failed",
            ))
        })?
        .json::<TurnstileResponse>()
        .await
        .map_err(|_| {
            Box::new(api_error(
                StatusCode::FORBIDDEN,
                "Turnstile verification failed",
            ))
        })?;
    let allowed_hostnames: Vec<_> = std::env::var("TURNSTILE_HOSTNAMES")
        .unwrap_or_else(|_| {
            if testing {
                "example.com".to_owned()
            } else {
                "localhost,127.0.0.1".to_owned()
            }
        })
        .split(',')
        .map(str::trim)
        .filter(|hostname| !hostname.is_empty())
        .map(str::to_owned)
        .collect();
    if !result.success
        || (!testing && result.action.as_deref() != Some(action))
        || result
            .hostname
            .as_ref()
            .is_none_or(|hostname| !allowed_hostnames.contains(hostname))
    {
        return Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "Turnstile verification failed",
        )));
    }
    Ok(())
}

async fn authenticate_management_key(state: &AppState, headers: &axum::http::HeaderMap) -> bool {
    let Some(secret) = bearer_secret(headers) else {
        return false;
    };
    let hash = hash_secret(secret);
    let now = crate::auth::now();
    let mut user = state.admin.user.write().await;
    let Some(user) = user.as_mut() else {
        return false;
    };
    let Some(key_index) = user.management_api_keys.iter().position(|key| {
        constant_time_eq(&key.secret_hash, &hash)
            && !key.expires_at.is_some_and(|expires| expires <= now)
    }) else {
        return false;
    };
    let key_id = user.management_api_keys[key_index].id.clone();
    if let Err(err) = persist_management_key_use(user, key_index, now, ADMIN_FILE).await {
        warn!(%err, %key_id, "could not persist Management API key usage");
    }
    true
}

async fn persist_management_key_use(
    user: &mut AdminUser,
    key_index: usize,
    used_at: u64,
    path: impl AsRef<Path>,
) -> Result<(), std::io::Error> {
    let mut updated = user.clone();
    updated.management_api_keys[key_index].last_used_at = Some(used_at);
    save_admin_at(path, &updated).await?;
    *user = updated;
    Ok(())
}

fn bearer_secret(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|secret| !secret.is_empty())
}

fn management_secret() -> String {
    let mut random = [0_u8; 32];
    rand::rng().fill_bytes(&mut random);
    format!("yab_mgmt_{}", hex(&random))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    output
}

fn turnstile_credentials() -> Option<(String, String)> {
    let secret = std::env::var("TURNSTILE_SECRET")
        .ok()
        .filter(|value| !value.trim().is_empty())?;
    let site_key = std::env::var("TURNSTILE_SITE_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            secret
                .starts_with(TURNSTILE_TEST_SECRET_PREFIX)
                .then(|| TURNSTILE_TEST_SITE_KEY.to_owned())
        })?;
    Some((secret, site_key))
}

async fn save_admin(user: &AdminUser) -> Result<(), std::io::Error> {
    save_admin_at(ADMIN_FILE, user).await
}

async fn save_admin_at(path: impl AsRef<Path>, user: &AdminUser) -> Result<(), std::io::Error> {
    crate::storage::write_json_atomic(path, user).await
}

fn session_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|cookie| cookie.strip_prefix(&format!("{SESSION_COOKIE}=")))
}

fn expired_cookie() -> &'static str {
    "yabane_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"
}

use axum::response::IntoResponse;

#[cfg(test)]
mod tests {
    use super::{AdminUser, ManagementApiKey, persist_management_key_use};

    #[test]
    fn rejects_ambiguous_management_api_key_identities() {
        let key = |id: &str| ManagementApiKey {
            id: id.to_owned(),
            name: "key".to_owned(),
            secret_hash: "hash".to_owned(),
            prefix: "yab_mgmt_…test".to_owned(),
            created_at: 0,
            expires_at: None,
            last_used_at: None,
        };
        let user = AdminUser {
            username: "admin".to_owned(),
            email: "admin@example.com".to_owned(),
            password_hash: "hash".to_owned(),
            management_api_keys: vec![key("duplicate"), key("duplicate")],
        };

        assert!(super::validate_management_key_identities(&user).is_err());
    }

    #[tokio::test]
    async fn failed_last_use_persistence_does_not_change_memory() {
        let directory = std::env::temp_dir().join(format!(
            "yabane-admin-last-use-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let blocked_parent = directory.join("not-a-directory");
        tokio::fs::write(&blocked_parent, b"blocked").await.unwrap();
        let mut user = AdminUser {
            username: "admin".to_owned(),
            email: "admin@example.com".to_owned(),
            password_hash: "hash".to_owned(),
            management_api_keys: vec![ManagementApiKey {
                id: "key-id".to_owned(),
                name: "key".to_owned(),
                secret_hash: "hash".to_owned(),
                prefix: "yab_mgmt_…test".to_owned(),
                created_at: 0,
                expires_at: None,
                last_used_at: None,
            }],
        };

        let result =
            persist_management_key_use(&mut user, 0, 123, blocked_parent.join("admin.json")).await;

        assert!(result.is_err());
        assert_eq!(user.management_api_keys[0].last_used_at, None);
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
}

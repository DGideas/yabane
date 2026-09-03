use std::{
    io::ErrorKind,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::Response,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::{api_error, config::AppState};

pub const AUTH_FILE: &str = "data/auth.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuthConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub api_keys: Vec<GatewayApiKey>,
}

fn default_enabled() -> bool {
    true
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_keys: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GatewayApiKey {
    pub id: String,
    pub note: String,
    pub secret_hash: String,
    pub prefix: String,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub provider_ids: Vec<String>,
}

#[derive(Serialize)]
pub struct GatewayApiKeyView {
    pub id: String,
    pub note: String,
    pub prefix: String,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub provider_ids: Vec<String>,
}

impl From<&GatewayApiKey> for GatewayApiKeyView {
    fn from(key: &GatewayApiKey) -> Self {
        Self {
            id: key.id.clone(),
            note: key.note.clone(),
            prefix: key.prefix.clone(),
            created_at: key.created_at,
            expires_at: key.expires_at,
            provider_ids: key.provider_ids.clone(),
        }
    }
}

pub async fn load_auth() -> Result<AuthConfig, String> {
    let contents = match tokio::fs::read(AUTH_FILE).await {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(AuthConfig::default()),
        Err(err) => return Err(format!("read {AUTH_FILE}: {err}")),
    };
    serde_json::from_slice(&contents).map_err(|err| format!("parse {AUTH_FILE}: {err}"))
}

pub async fn save_auth(auth: &AuthConfig) -> Result<(), std::io::Error> {
    tokio::fs::create_dir_all("data").await?;
    let contents = serde_json::to_vec_pretty(auth).expect("serialize auth configuration");
    tokio::fs::write(AUTH_FILE, contents).await
}

pub fn generate_secret() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("sk-{}", hex(&bytes))
}

pub fn hash_secret(secret: &str) -> String {
    hex(&Sha256::digest(secret.as_bytes()))
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs()
}

pub async fn authorize(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let auth = state.auth.read().await;
    if !auth.enabled {
        drop(auth);
        return next.run(request).await;
    }
    let Some(secret) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
    else {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "Missing or invalid Authorization: Bearer header",
        );
    };
    let secret_hash = hash_secret(secret);
    let current_time = now();
    let key = auth
        .api_keys
        .iter()
        .find(|key| key.secret_hash == secret_hash);
    let Some(key) = key else {
        return api_error(StatusCode::UNAUTHORIZED, "Invalid API key");
    };
    if key.expires_at.is_some_and(|expiry| expiry <= current_time) {
        return api_error(StatusCode::UNAUTHORIZED, "API key has expired");
    }
    drop(auth);
    next.run(request).await
}

pub async fn authorized_provider_ids(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Option<Vec<String>> {
    let auth = state.auth.read().await;
    if !auth.enabled {
        return None;
    }
    let secret = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    let hash = hash_secret(secret);
    auth.api_keys
        .iter()
        .find(|key| key.secret_hash == hash)
        .map(|key| key.provider_ids.clone())
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

pub type SharedAuth = Arc<RwLock<AuthConfig>>;

#[cfg(test)]
mod tests {
    use super::{generate_secret, hash_secret};

    #[test]
    fn generated_secrets_are_prefixed_and_unique() {
        let first = generate_secret();
        let second = generate_secret();
        assert!(first.starts_with("sk-"));
        assert_ne!(first, second);
        assert_eq!(hash_secret(&first).len(), 64);
    }
}

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

use crate::{config::AppState, error::api_error};

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
    #[serde(default)]
    pub secret: String,
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
    pub secret: String,
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
            secret: key.secret.clone(),
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
    let auth: AuthConfig =
        serde_json::from_slice(&contents).map_err(|err| format!("parse {AUTH_FILE}: {err}"))?;
    validate_auth_identities(&auth)?;
    Ok(auth)
}

fn validate_auth_identities(auth: &AuthConfig) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    if auth
        .api_keys
        .iter()
        .any(|key| key.id.is_empty() || !ids.insert(key.id.as_str()))
    {
        return Err(format!(
            "parse {AUTH_FILE}: Gateway API key IDs must be non-empty and unique"
        ));
    }
    Ok(())
}

pub async fn save_auth(auth: &AuthConfig) -> Result<(), std::io::Error> {
    crate::storage::write_json_atomic(AUTH_FILE, auth).await
}

pub fn generate_secret() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("sk-{}", hex(&bytes))
}

pub fn hash_secret(secret: &str) -> String {
    hex(&Sha256::digest(secret.as_bytes()))
}

pub fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedProviders(pub Option<Vec<String>>);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedGatewayKey {
    pub id: String,
    pub note: String,
    pub prefix: String,
}

pub async fn authorize(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let auth = state.auth.read().await;
    if !auth.enabled {
        drop(auth);
        request.extensions_mut().insert(AuthorizedProviders(None));
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
        .find(|key| constant_time_eq(&key.secret_hash, &secret_hash));
    let Some(key) = key else {
        return api_error(StatusCode::UNAUTHORIZED, "Invalid API key");
    };
    if key.expires_at.is_some_and(|expiry| expiry <= current_time) {
        return api_error(StatusCode::UNAUTHORIZED, "API key has expired");
    }
    let provider_ids = key.provider_ids.clone();
    let gateway_key = AuthorizedGatewayKey {
        id: key.id.clone(),
        note: key.note.clone(),
        prefix: key.prefix.clone(),
    };
    drop(auth);
    request
        .extensions_mut()
        .insert(AuthorizedProviders(Some(provider_ids)));
    request.extensions_mut().insert(gateway_key);
    next.run(request).await
}

pub fn authorized_provider_ids(request: &Request) -> Option<&[String]> {
    request
        .extensions()
        .get::<AuthorizedProviders>()
        .and_then(|authorization| authorization.0.as_deref())
}

pub fn authorized_gateway_key(request: &Request) -> Option<&AuthorizedGatewayKey> {
    request.extensions().get::<AuthorizedGatewayKey>()
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
    use axum::body::Body;

    use super::{
        AuthConfig, AuthorizedGatewayKey, AuthorizedProviders, GatewayApiKey,
        authorized_gateway_key, authorized_provider_ids, constant_time_eq, generate_secret,
        hash_secret,
    };

    #[test]
    fn generated_secrets_are_prefixed_and_unique() {
        let first = generate_secret();
        let second = generate_secret();
        assert!(first.starts_with("sk-"));
        assert_ne!(first, second);
        assert_eq!(hash_secret(&first).len(), 64);
    }

    #[test]
    fn secret_hash_comparison_requires_an_exact_match() {
        assert!(constant_time_eq("same-length", "same-length"));
        assert!(!constant_time_eq("same-length", "different!!"));
        assert!(!constant_time_eq("short", "longer"));
    }

    #[test]
    fn rejects_ambiguous_gateway_api_key_identities() {
        let key = |id: &str| GatewayApiKey {
            id: id.to_owned(),
            note: String::new(),
            secret_hash: "hash".to_owned(),
            secret: String::new(),
            prefix: "sk-…test".to_owned(),
            created_at: 0,
            expires_at: None,
            provider_ids: Vec::new(),
        };
        let auth = AuthConfig {
            enabled: true,
            api_keys: vec![key("duplicate"), key("duplicate")],
        };

        assert!(super::validate_auth_identities(&auth).is_err());
    }

    #[test]
    fn provider_scope_comes_from_authorization_result_not_request_header() {
        let mut request = axum::http::Request::new(Body::empty());
        request.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            "Bearer caller-secret".parse().expect("valid header"),
        );
        request
            .extensions_mut()
            .insert(AuthorizedProviders(Some(vec![
                "allowed-provider".to_owned(),
            ])));
        request.extensions_mut().insert(AuthorizedGatewayKey {
            id: "key-id".to_owned(),
            note: "Production".to_owned(),
            prefix: "sk-…test".to_owned(),
        });

        assert_eq!(
            authorized_provider_ids(&request),
            Some(["allowed-provider".to_owned()].as_slice())
        );
        assert_eq!(authorized_gateway_key(&request).unwrap().id, "key-id");
    }
}

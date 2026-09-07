use std::collections::HashMap;

use axum::{
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{
    config::{ApiEndpoint, ApiKey, ApiType, AppState, Provider},
    gateway::join_upstream_url,
};

const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Model {
    pub id: String,
    #[serde(default = "model_object")]
    pub object: String,
    #[serde(default)]
    pub owned_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

#[derive(Clone)]
struct ProviderDiscovery {
    models: Vec<Model>,
    model_endpoints: HashMap<String, Vec<String>>,
}

#[derive(Serialize)]
struct ListModelsResponse {
    object: &'static str,
    data: Vec<Model>,
}

#[derive(Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModel>,
}

#[derive(Deserialize)]
struct OpenAiModel {
    id: String,
    #[serde(default)]
    owned_by: String,
    created: Option<i64>,
    context_window: Option<u64>,
    context_length: Option<u64>,
}

#[derive(Deserialize)]
struct AnthropicModelsResponse {
    data: Vec<AnthropicModel>,
    #[serde(default)]
    has_more: bool,
    last_id: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicModel {
    id: String,
    #[serde(default)]
    display_name: String,
    created_at: Option<String>,
    max_input_tokens: Option<u64>,
}

pub async fn list_models(State(state): State<AppState>, request: Request) -> Response {
    let allowed = crate::auth::authorized_provider_ids(&state, request.headers()).await;
    let providers: Vec<_> = state
        .providers
        .read()
        .await
        .values()
        .filter(|provider| {
            allowed
                .as_ref()
                .is_none_or(|ids| ids.is_empty() || ids.contains(&provider.id))
        })
        .cloned()
        .collect();
    let results = discover_providers(&state.client, providers).await;
    update_discoveries(&state, &results).await;

    let mut data = Vec::new();
    for (provider, result) in &results {
        match result {
            Ok(discovery) => {
                data.extend(discovery.models.iter().cloned().map(|mut model| {
                    let upstream_id = model
                        .id
                        .strip_prefix(&format!("{}/", provider.id))
                        .unwrap_or(&model.id);
                    model.id = format!("{}/{upstream_id}", provider.id);
                    if model.owned_by.is_empty() {
                        model.owned_by = provider.id.clone();
                    }
                    model
                }));
            }
            Err(err) => warn!(provider = %provider.id, %err, "could not list provider models"),
        }
    }
    data.sort_by(|a, b| a.id.cmp(&b.id));
    data.dedup_by(|a, b| a.id == b.id);

    axum::Json(ListModelsResponse {
        object: "list",
        data,
    })
    .into_response()
}

pub async fn refresh_provider(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let provider = match state.providers.read().await.get(&id).cloned() {
        Some(provider) => provider,
        None => {
            return crate::error::api_error(
                axum::http::StatusCode::NOT_FOUND,
                "Provider not found",
            );
        }
    };
    let results = discover_providers(&state.client, vec![provider]).await;
    update_discoveries(&state, &results).await;
    match &results[0].1 {
        Ok(discovery) => axum::Json(serde_json::json!({"models": discovery.models.iter().map(|model| &model.id).collect::<Vec<_>>() })).into_response(),
        Err(error) => crate::error::api_error(axum::http::StatusCode::BAD_GATEWAY, error.clone()),
    }
}

async fn discover_providers(
    client: &reqwest::Client,
    providers: Vec<Provider>,
) -> Vec<(Provider, Result<ProviderDiscovery, String>)> {
    join_all(providers.into_iter().map(|provider| {
        let client = client.clone();
        async move {
            let models = discover_provider_models(&client, &provider).await;
            (provider, models)
        }
    }))
    .await
}

async fn update_discoveries(
    state: &AppState,
    results: &[(Provider, Result<ProviderDiscovery, String>)],
) {
    let mut providers = state.providers.write().await;
    let mut updated = providers.clone();
    let mut changed = false;
    for (provider, result) in results {
        let Some(stored) = updated.get_mut(&provider.id) else {
            continue;
        };
        if !discovery_inputs_match(stored, provider) {
            continue;
        }
        changed = true;
        stored.models_discovered_at = Some(crate::auth::now());
        match result {
            Ok(discovery) => {
                stored.discovered_models = discovery
                    .models
                    .iter()
                    .map(|model| model.id.clone())
                    .collect();
                stored.model_endpoints = discovery.model_endpoints.clone();
                stored.model_endpoint_preferences.retain(|preference| {
                    discovery
                        .model_endpoints
                        .get(&preference.model)
                        .is_some_and(|endpoint_ids| endpoint_ids.contains(&preference.endpoint_id))
                        && stored.endpoints.iter().any(|endpoint| {
                            endpoint.id == preference.endpoint_id
                                && endpoint.api_type == preference.api_type
                        })
                });
                stored.model_discovery_error = None;
            }
            Err(error) => stored.model_discovery_error = Some(error.clone()),
        }
    }
    if !changed {
        return;
    }
    if let Err(error) = crate::config::save_providers(&updated).await {
        warn!(%error, "could not persist model discovery");
        return;
    }
    *providers = updated;
}

fn discovery_inputs_match(current: &Provider, snapshot: &Provider) -> bool {
    current.endpoints.len() == snapshot.endpoints.len()
        && current
            .endpoints
            .iter()
            .zip(&snapshot.endpoints)
            .all(|(current, snapshot)| {
                current.id == snapshot.id
                    && current.api_type == snapshot.api_type
                    && current.base_url == snapshot.base_url
                    && current.socks5_proxy == snapshot.socks5_proxy
                    && current.requires_api_key == snapshot.requires_api_key
                    && discovery_keys_match(&current.api_keys, &snapshot.api_keys)
                    && current.openai_subscription == snapshot.openai_subscription
            })
}

fn discovery_keys_match(current: &[ApiKey], snapshot: &[ApiKey]) -> bool {
    current.len() == snapshot.len()
        && current.iter().zip(snapshot).all(|(current, snapshot)| {
            current.id == snapshot.id
                && current.secret == snapshot.secret
                && current.enabled == snapshot.enabled
        })
}

async fn discover_provider_models(
    client: &reqwest::Client,
    provider: &Provider,
) -> Result<ProviderDiscovery, String> {
    let results = join_all(provider.endpoints.iter().map(|endpoint| async move {
        (
            endpoint.id.clone(),
            list_endpoint_models(client, provider, endpoint).await,
        )
    }))
    .await;
    let mut models = Vec::new();
    let mut model_endpoints: HashMap<String, Vec<String>> = HashMap::new();
    let mut errors = Vec::new();
    for (endpoint_id, result) in results {
        match result {
            Ok(endpoint_models) => {
                for model in endpoint_models {
                    model_endpoints
                        .entry(model.id.clone())
                        .or_default()
                        .push(endpoint_id.clone());
                    models.push(model);
                }
            }
            Err(err) => errors.push(err),
        }
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    if models.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(ProviderDiscovery {
        models,
        model_endpoints,
    })
}

async fn list_endpoint_models(
    client: &reqwest::Client,
    provider: &Provider,
    endpoint: &ApiEndpoint,
) -> Result<Vec<Model>, String> {
    if endpoint.api_type == ApiType::OpenaiCodex {
        if endpoint.openai_subscription.is_none() {
            return Err(format!(
                "endpoint '{}' has no connected OpenAI subscription",
                endpoint.id
            ));
        }
        return Ok(crate::openai_subscription::MODELS
            .iter()
            .map(|id| Model {
                id: (*id).to_owned(),
                object: model_object(),
                owned_by: "openai".to_owned(),
                created: None,
                context_window: None,
            })
            .collect());
    }
    let keys: Vec<Option<ApiKey>> = if endpoint.requires_api_key {
        endpoint
            .api_keys
            .iter()
            .filter(|key| key.enabled)
            .cloned()
            .map(Some)
            .collect()
    } else {
        vec![None]
    };
    if keys.is_empty() {
        return Err(format!("endpoint '{}' has no enabled API key", endpoint.id));
    }

    let responses = join_all(
        keys.iter()
            .map(|key| fetch_models(client, endpoint, key.as_ref(), provider)),
    )
    .await;
    let mut models = Vec::new();
    let mut errors = Vec::new();
    for response in responses {
        match response {
            Ok(key_models) => models.extend(key_models),
            Err(err) => errors.push(err),
        }
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    if models.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(models)
}

async fn fetch_models(
    client: &reqwest::Client,
    endpoint: &ApiEndpoint,
    key: Option<&ApiKey>,
    provider: &Provider,
) -> Result<Vec<Model>, String> {
    let path = match endpoint.api_type {
        ApiType::OpenaiCompatible => "/v1/models",
        ApiType::OpenaiCodex => unreachable!("subscription models use the built-in catalog"),
        ApiType::Anthropic => "/v1/models?limit=1000",
    };
    let client = endpoint.client(client)?;
    let mut request = client.get(join_upstream_url(&endpoint.base_url, path));
    if let Some(key) = key {
        request = match endpoint.api_type {
            ApiType::OpenaiCompatible => request.bearer_auth(&key.secret),
            ApiType::OpenaiCodex => unreachable!("subscription models use the built-in catalog"),
            ApiType::Anthropic => request
                .header("x-api-key", &key.secret)
                .header("anthropic-version", ANTHROPIC_VERSION),
        };
    } else if endpoint.api_type == ApiType::Anthropic {
        request = request.header("anthropic-version", ANTHROPIC_VERSION);
    }
    let response = request
        .send()
        .await
        .map_err(|err| format!("{}: {err}", endpoint.id))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|err| format!("{}: {err}", endpoint.id))?;
    if !status.is_success() {
        return Err(format!("{} returned {status}", endpoint.id));
    }

    match endpoint.api_type {
        ApiType::OpenaiCompatible => parse_openai_models(&body, provider),
        ApiType::OpenaiCodex => unreachable!("subscription models use the built-in catalog"),
        ApiType::Anthropic => parse_anthropic_models(&body, provider),
    }
}

fn parse_openai_models(body: &[u8], provider: &Provider) -> Result<Vec<Model>, String> {
    let data = serde_json::from_slice::<OpenAiModelsResponse>(body)
        .map(|response| response.data)
        .or_else(|_| serde_json::from_slice::<Vec<OpenAiModel>>(body))
        .map_err(|err| format!("invalid OpenAI models response: {err}"))?;
    Ok(data
        .into_iter()
        .map(|model| Model {
            id: model.id,
            object: model_object(),
            owned_by: if model.owned_by.is_empty() {
                provider.id.clone()
            } else {
                model.owned_by
            },
            created: model.created,
            context_window: model.context_window.or(model.context_length),
        })
        .collect())
}

fn parse_anthropic_models(body: &[u8], provider: &Provider) -> Result<Vec<Model>, String> {
    let response: AnthropicModelsResponse = serde_json::from_slice(body)
        .map_err(|err| format!("invalid Anthropic models response: {err}"))?;
    let _pagination = (response.has_more, response.last_id);
    Ok(response
        .data
        .into_iter()
        .map(|model| {
            let _metadata = (&model.display_name, &model.created_at);
            Model {
                id: model.id,
                object: model_object(),
                owned_by: provider.id.clone(),
                created: None,
                context_window: model.max_input_tokens,
            }
        })
        .collect())
}

fn model_object() -> String {
    "model".to_owned()
}

#[cfg(test)]
mod tests {
    use crate::config::{ApiEndpoint, Provider};

    use super::{discovery_inputs_match, parse_anthropic_models, parse_openai_models};

    fn provider() -> Provider {
        Provider {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            extra_headers: std::collections::HashMap::new(),
            extra_body: serde_json::Map::new(),
            defaults_endpoint_ids: Vec::new(),
            endpoints: Vec::new(),
            discovered_models: Vec::new(),
            model_endpoints: std::collections::HashMap::new(),
            model_endpoint_preferences: Vec::new(),
            models_discovered_at: None,
            model_discovery_error: None,
        }
    }

    #[test]
    fn stale_discovery_snapshot_is_rejected_after_endpoint_changes() {
        let mut snapshot = provider();
        snapshot.endpoints.push(ApiEndpoint {
            id: "primary".to_owned(),
            base_url: "https://old.example/v1".to_owned(),
            requires_api_key: false,
            ..ApiEndpoint::default()
        });
        let mut current = snapshot.clone();
        assert!(discovery_inputs_match(&current, &snapshot));

        current.endpoints[0].base_url = "https://new.example/v1".to_owned();
        assert!(!discovery_inputs_match(&current, &snapshot));

        current = snapshot.clone();
        current.endpoints.clear();
        assert!(!discovery_inputs_match(&current, &snapshot));
    }

    #[test]
    fn parses_openai_envelope_and_array() {
        let envelope = parse_openai_models(
            br#"{"object":"list","data":[{"id":"a","owned_by":"owner"}]}"#,
            &provider(),
        )
        .expect("envelope");
        let array = parse_openai_models(br#"[{"id":"b","context_length":4096}]"#, &provider())
            .expect("array");
        assert_eq!(envelope[0].id, "a");
        assert_eq!(array[0].context_window, Some(4096));
    }

    #[test]
    fn parses_anthropic_models() {
        let models = parse_anthropic_models(
            br#"{"data":[{"id":"claude-test","display_name":"Claude","created_at":"2026-01-01T00:00:00Z","max_input_tokens":200000}],"has_more":false}"#,
            &provider(),
        )
        .expect("anthropic");
        assert_eq!(models[0].id, "claude-test");
        assert_eq!(models[0].context_window, Some(200000));
    }
}

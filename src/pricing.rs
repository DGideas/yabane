use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    config::{ApiEndpoint, Provider},
    usage::TokenUsage,
};

/// A price table belongs to one Global, Provider, or Endpoint scope.
/// `updated_at` is the effective-price timestamp, not a mutable numeric revision.
pub const PRICING_FILE: &str = "data/pricing.json";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PricingTable {
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub models: HashMap<String, ModelPricing>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ModelPricing {
    /// USD per one million non-cached input tokens.
    pub input_per_million: Option<f64>,
    /// USD per one million output tokens.
    pub output_per_million: Option<f64>,
    /// USD per one million cached input tokens. When omitted, cached input uses
    /// the configured input rate rather than being charged twice.
    #[serde(default)]
    pub cache_read_per_million: Option<f64>,
    /// Reserved for a future usage source that reports cache writes.
    #[serde(default)]
    pub cache_write_per_million: Option<f64>,
}

/// Validate user-provided pricing without changing it or inferring model
/// capabilities. Partial entries are allowed because narrower scopes can
/// override only selected fields from broader scopes.
pub fn validate_table(table: &PricingTable, context: &str) -> Result<(), String> {
    for (model, pricing) in &table.models {
        if !crate::routes::valid_model_pattern(model) {
            return Err(format!(
                "{context}: pricing model patterns must be exact IDs or non-empty prefixes ending in one '*'"
            ));
        }
        for (name, value) in [
            ("input_per_million", pricing.input_per_million),
            ("output_per_million", pricing.output_per_million),
            ("cache_read_per_million", pricing.cache_read_per_million),
            ("cache_write_per_million", pricing.cache_write_per_million),
        ] {
            if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
                return Err(format!(
                    "{context}: pricing field '{name}' for model '{model}' must be a finite non-negative number"
                ));
            }
        }
    }
    Ok(())
}

pub async fn load() -> Result<PricingTable, String> {
    let contents = match tokio::fs::read(PRICING_FILE).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PricingTable::default());
        }
        Err(error) => return Err(format!("read {PRICING_FILE}: {error}")),
    };
    let table: PricingTable = serde_json::from_slice(&contents)
        .map_err(|error| format!("parse {PRICING_FILE}: {error}"))?;
    validate_table(&table, "Global pricing")?;
    Ok(table)
}

pub async fn save(table: &PricingTable) -> Result<(), std::io::Error> {
    crate::storage::write_json_atomic(PRICING_FILE, table).await
}

pub fn effective_pricing(
    global: &PricingTable,
    provider: &Provider,
    endpoint: &ApiEndpoint,
    upstream_model: &str,
) -> Option<ModelPricing> {
    let global_pricing = pricing_for_model(global, upstream_model);
    let provider_pricing = provider
        .pricing
        .as_ref()
        .and_then(|table| pricing_for_model(table, upstream_model));
    let endpoint_pricing = endpoint
        .pricing
        .as_ref()
        .and_then(|table| pricing_for_model(table, upstream_model));
    if global_pricing.is_none() && provider_pricing.is_none() && endpoint_pricing.is_none() {
        return None;
    }
    Some(ModelPricing {
        input_per_million: endpoint_pricing
            .and_then(|pricing| pricing.input_per_million)
            .or_else(|| provider_pricing.and_then(|pricing| pricing.input_per_million))
            .or_else(|| global_pricing.and_then(|pricing| pricing.input_per_million)),
        output_per_million: endpoint_pricing
            .and_then(|pricing| pricing.output_per_million)
            .or_else(|| provider_pricing.and_then(|pricing| pricing.output_per_million))
            .or_else(|| global_pricing.and_then(|pricing| pricing.output_per_million)),
        cache_read_per_million: endpoint_pricing
            .and_then(|pricing| pricing.cache_read_per_million)
            .or_else(|| provider_pricing.and_then(|pricing| pricing.cache_read_per_million))
            .or_else(|| global_pricing.and_then(|pricing| pricing.cache_read_per_million)),
        cache_write_per_million: endpoint_pricing
            .and_then(|pricing| pricing.cache_write_per_million)
            .or_else(|| provider_pricing.and_then(|pricing| pricing.cache_write_per_million))
            .or_else(|| global_pricing.and_then(|pricing| pricing.cache_write_per_million)),
    })
}

fn pricing_for_model<'a>(table: &'a PricingTable, model: &str) -> Option<&'a ModelPricing> {
    if let Some(pricing) = table.models.get(model) {
        return Some(pricing);
    }
    table
        .models
        .iter()
        .filter_map(|(pattern, pricing)| {
            crate::routes::model_pattern_specificity(pattern, model)
                .map(|specificity| (specificity, pricing))
        })
        .max_by_key(|(specificity, _)| *specificity)
        .map(|(_, pricing)| pricing)
}

const PRICE_SCALE: f64 = 1_000_000_000_000.0;

fn rate_to_units(rate: f64) -> i128 {
    let scaled = rate * PRICE_SCALE;
    if scaled >= i128::MAX as f64 {
        i128::MAX
    } else {
        scaled.round() as i128
    }
}

/// Calculate an equivalent USD value. Cached tokens are removed from ordinary
/// input before applying the input rate, so they cannot be charged twice.
pub fn calculate(pricing: &ModelPricing, usage: &TokenUsage) -> Option<f64> {
    if usage.input == 0 && usage.output == 0 && usage.cached == 0 {
        return None;
    }
    let input_rate = rate_to_units(pricing.input_per_million?);
    let output_rate = rate_to_units(pricing.output_per_million?);
    let cache_rate = rate_to_units(
        pricing
            .cache_read_per_million
            .or(pricing.input_per_million)?,
    );
    let cached = usage.cached.min(usage.input);
    let uncached_units = i128::from(usage.input - cached).saturating_mul(input_rate);
    let cached_units = i128::from(cached).saturating_mul(cache_rate);
    let output_units = i128::from(usage.output).saturating_mul(output_rate);
    let units = uncached_units
        .saturating_add(cached_units)
        .saturating_add(output_units)
        .checked_div(1_000_000)?;
    let cost = units as f64 / PRICE_SCALE;
    cost.is_finite().then_some(cost)
}

#[cfg(test)]
mod tests {
    use super::{ModelPricing, PricingTable, calculate, effective_pricing};
    use crate::{
        config::{ApiEndpoint, ApiType, Provider},
        usage::TokenUsage,
    };
    use std::collections::HashMap;

    fn provider() -> Provider {
        Provider {
            id: "p".to_owned(),
            name: "P".to_owned(),
            extra_headers: HashMap::new(),
            extra_body: serde_json::Map::new(),
            defaults_endpoint_ids: Vec::new(),
            pricing: Some(PricingTable {
                updated_at: 123,
                models: HashMap::from([(
                    "model".to_owned(),
                    ModelPricing {
                        input_per_million: Some(1.0),
                        output_per_million: Some(2.0),
                        cache_read_per_million: Some(0.5),
                        cache_write_per_million: None,
                    },
                )]),
            }),
            endpoints: vec![],
            discovered_models: Vec::new(),
            model_endpoints: HashMap::new(),
            model_endpoint_preferences: Vec::new(),
            models_discovered_at: None,
            model_discovery_error: None,
        }
    }

    #[test]
    fn endpoint_fields_override_provider_fields() {
        let provider = provider();
        let endpoint = ApiEndpoint {
            id: "e".to_owned(),
            api_type: ApiType::OpenaiCompatible,
            pricing: Some(PricingTable {
                updated_at: 456,
                models: HashMap::from([(
                    "model".to_owned(),
                    ModelPricing {
                        input_per_million: None,
                        output_per_million: Some(3.0),
                        cache_read_per_million: None,
                        cache_write_per_million: None,
                    },
                )]),
            }),
            ..ApiEndpoint::default()
        };
        let pricing =
            effective_pricing(&PricingTable::default(), &provider, &endpoint, "model").unwrap();
        assert_eq!(pricing.input_per_million, Some(1.0));
        assert_eq!(pricing.output_per_million, Some(3.0));
        assert_eq!(pricing.cache_read_per_million, Some(0.5));
    }

    #[test]
    fn pricing_must_be_configured_explicitly() {
        let provider = Provider {
            pricing: None,
            ..provider()
        };
        let codex_endpoint = ApiEndpoint {
            api_type: ApiType::OpenaiCodex,
            ..ApiEndpoint::default()
        };
        let qwen_endpoint = ApiEndpoint {
            base_url: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
                .to_owned(),
            ..ApiEndpoint::default()
        };
        assert!(
            effective_pricing(
                &PricingTable::default(),
                &provider,
                &codex_endpoint,
                "gpt-5.4-mini"
            )
            .is_none()
        );
        assert!(
            effective_pricing(
                &PricingTable::default(),
                &provider,
                &qwen_endpoint,
                "qwen3.8-flash"
            )
            .is_none()
        );
    }

    #[test]
    fn cached_tokens_are_not_charged_as_normal_input() {
        let provider = provider();
        let endpoint = ApiEndpoint::default();
        let usage = TokenUsage {
            input: 1_000_000,
            output: 1_000_000,
            cached: 400_000,
            cost: None,
            finish_reason: None,
        };
        // 600k uncached input at $1/M + 400k cached input at $0.50/M
        // + 1M output at $2/M.
        let pricing =
            effective_pricing(&PricingTable::default(), &provider, &endpoint, "model").unwrap();
        assert_eq!(calculate(&pricing, &usage), Some(2.8));
    }

    #[test]
    fn exact_and_longest_prefix_patterns_follow_model_routing_rules() {
        let global = PricingTable {
            updated_at: 1,
            models: HashMap::from([
                (
                    "model-*".to_owned(),
                    ModelPricing {
                        input_per_million: Some(1.0),
                        output_per_million: Some(2.0),
                        ..ModelPricing::default()
                    },
                ),
                (
                    "model-fast-*".to_owned(),
                    ModelPricing {
                        input_per_million: Some(3.0),
                        output_per_million: Some(4.0),
                        ..ModelPricing::default()
                    },
                ),
                (
                    "model-fast-one".to_owned(),
                    ModelPricing {
                        input_per_million: Some(5.0),
                        output_per_million: Some(6.0),
                        ..ModelPricing::default()
                    },
                ),
            ]),
        };
        let provider = Provider {
            pricing: None,
            ..provider()
        };
        let endpoint = ApiEndpoint::default();

        let exact = effective_pricing(&global, &provider, &endpoint, "model-fast-one").unwrap();
        assert_eq!(exact.input_per_million, Some(5.0));
        assert_eq!(exact.output_per_million, Some(6.0));

        let longest = effective_pricing(&global, &provider, &endpoint, "model-fast-two").unwrap();
        assert_eq!(longest.input_per_million, Some(3.0));
        assert_eq!(longest.output_per_million, Some(4.0));
    }

    #[test]
    fn pricing_patterns_allow_only_one_trailing_wildcard() {
        let table = PricingTable {
            models: HashMap::from([("model-*-invalid".to_owned(), ModelPricing::default())]),
            ..PricingTable::default()
        };
        assert!(super::validate_table(&table, "test").is_err());
    }

    #[test]
    fn global_fields_are_defaults_for_provider_and_endpoint_overrides() {
        let global = PricingTable {
            updated_at: 1,
            models: HashMap::from([(
                "model".to_owned(),
                ModelPricing {
                    input_per_million: Some(0.8),
                    output_per_million: Some(1.6),
                    cache_read_per_million: Some(0.2),
                    cache_write_per_million: None,
                },
            )]),
        };
        let provider = Provider {
            pricing: Some(PricingTable {
                updated_at: 2,
                models: HashMap::from([(
                    "model".to_owned(),
                    ModelPricing {
                        output_per_million: Some(2.0),
                        ..ModelPricing::default()
                    },
                )]),
            }),
            ..provider()
        };
        let endpoint = ApiEndpoint {
            pricing: Some(PricingTable {
                updated_at: 3,
                models: HashMap::from([(
                    "model".to_owned(),
                    ModelPricing {
                        cache_read_per_million: Some(0.1),
                        ..ModelPricing::default()
                    },
                )]),
            }),
            ..ApiEndpoint::default()
        };

        let pricing = effective_pricing(&global, &provider, &endpoint, "model").unwrap();
        assert_eq!(pricing.input_per_million, Some(0.8));
        assert_eq!(pricing.output_per_million, Some(2.0));
        assert_eq!(pricing.cache_read_per_million, Some(0.1));
    }
}

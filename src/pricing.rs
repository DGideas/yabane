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
    /// Rates for the model IDs Yabane sends to the Provider, keyed by exact ID
    /// or one trailing `*` prefix. This is the historical pricing namespace and
    /// keeps its meaning; see `incoming_models` for the caller-facing name.
    #[serde(default)]
    pub models: HashMap<String, ModelPricing>,
    /// Rates for the model names callers send, including public routing aliases.
    /// A caller-facing name belongs to no single Provider or Endpoint connection,
    /// so these rules are only supported by Global pricing.
    #[serde(default)]
    pub incoming_models: HashMap<String, ModelPricing>,
}

impl PricingTable {
    pub fn is_empty(&self) -> bool {
        self.models.is_empty() && self.incoming_models.is_empty()
    }
}

/// Which pricing table supplied a rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PricingScope {
    Global,
    Provider,
    Endpoint,
}

/// Which model name a pricing rule matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PricingName {
    /// The model name the caller sent, including a public routing alias.
    Incoming,
    /// The model ID Yabane sent to the Provider.
    Outgoing,
}

/// The configured rule behind one effective rate, kept in Activity so an
/// estimate stays explainable after pricing configuration changes.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct PricingSource {
    pub scope: PricingScope,
    pub pattern: String,
    pub name: PricingName,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct PricingSources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<PricingSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<PricingSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<PricingSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<PricingSource>,
}

/// Effective rates for one request plus the rules they came from.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResolvedPricing {
    pub pricing: ModelPricing,
    pub sources: PricingSources,
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
pub fn validate_table(
    table: &PricingTable,
    context: &str,
    allow_incoming: bool,
) -> Result<(), String> {
    if !allow_incoming && !table.incoming_models.is_empty() {
        return Err(format!(
            "{context}: prices for the model name callers send are only supported in Global pricing"
        ));
    }
    if let Some(pattern) = table
        .models
        .keys()
        .find(|pattern| table.incoming_models.contains_key(*pattern))
    {
        return Err(format!(
            "{context}: model pattern '{pattern}' cannot be priced as both an incoming and an outgoing name"
        ));
    }
    for (namespace, models) in [("", &table.models), ("incoming ", &table.incoming_models)] {
        for (model, pricing) in models {
            if model.trim() != model {
                return Err(format!(
                    "{context}: {namespace}model patterns must not start or end with whitespace"
                ));
            }
            if !crate::routes::valid_model_pattern(model) {
                return Err(format!(
                    "{context}: {namespace}pricing model patterns must be exact IDs or non-empty prefixes ending in one '*'"
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
    validate_table(&table, "Global pricing", true)?;
    Ok(table)
}

pub async fn save(table: &PricingTable) -> Result<(), std::io::Error> {
    crate::storage::write_json_atomic(PRICING_FILE, table).await
}

/// Resolve the effective rates for one request.
///
/// Rates for the model ID Yabane sends to the Provider win: they describe what
/// actually ran, and an Endpoint or Provider override stays a deliberate
/// statement about that connection. A Global rule for the name the caller sent
/// (the public alias) fills whatever is left, so one alias price covers every
/// destination. Each rate field is resolved independently, narrowest scope
/// first: Endpoint, Provider, then Global.
pub fn effective_pricing(
    global: &PricingTable,
    provider: &Provider,
    endpoint: &ApiEndpoint,
    incoming_model: &str,
    outgoing_model: &str,
) -> Option<ResolvedPricing> {
    let candidates = [
        ScopeCandidates {
            scope: PricingScope::Endpoint,
            incoming: endpoint
                .pricing
                .as_ref()
                .and_then(|table| pricing_for_model(&table.incoming_models, incoming_model)),
            outgoing: endpoint
                .pricing
                .as_ref()
                .and_then(|table| pricing_for_model(&table.models, outgoing_model)),
        },
        ScopeCandidates {
            scope: PricingScope::Provider,
            incoming: provider
                .pricing
                .as_ref()
                .and_then(|table| pricing_for_model(&table.incoming_models, incoming_model)),
            outgoing: provider
                .pricing
                .as_ref()
                .and_then(|table| pricing_for_model(&table.models, outgoing_model)),
        },
        ScopeCandidates {
            scope: PricingScope::Global,
            incoming: pricing_for_model(&global.incoming_models, incoming_model),
            outgoing: pricing_for_model(&global.models, outgoing_model),
        },
    ];
    let input = first_rate(&candidates, |pricing| pricing.input_per_million);
    let output = first_rate(&candidates, |pricing| pricing.output_per_million);
    let cache_read = first_rate(&candidates, |pricing| pricing.cache_read_per_million);
    let cache_write = first_rate(&candidates, |pricing| pricing.cache_write_per_million);
    if input.is_none() && output.is_none() && cache_read.is_none() && cache_write.is_none() {
        return None;
    }
    Some(ResolvedPricing {
        pricing: ModelPricing {
            input_per_million: input.as_ref().map(|(value, _)| *value),
            output_per_million: output.as_ref().map(|(value, _)| *value),
            cache_read_per_million: cache_read.as_ref().map(|(value, _)| *value),
            cache_write_per_million: cache_write.as_ref().map(|(value, _)| *value),
        },
        sources: PricingSources {
            input: input.map(|(_, source)| source),
            output: output.map(|(_, source)| source),
            cache_read: cache_read.map(|(_, source)| source),
            cache_write: cache_write.map(|(_, source)| source),
        },
    })
}

struct ScopeCandidates<'a> {
    scope: PricingScope,
    incoming: Option<(&'a str, &'a ModelPricing)>,
    outgoing: Option<(&'a str, &'a ModelPricing)>,
}

fn first_rate(
    candidates: &[ScopeCandidates<'_>],
    rate: impl Fn(&ModelPricing) -> Option<f64>,
) -> Option<(f64, PricingSource)> {
    for candidate in candidates {
        for (name, entry) in [
            (PricingName::Outgoing, candidate.outgoing),
            (PricingName::Incoming, candidate.incoming),
        ] {
            if let Some((pattern, pricing)) = entry
                && let Some(value) = rate(pricing)
            {
                return Some((
                    value,
                    PricingSource {
                        scope: candidate.scope,
                        pattern: pattern.to_owned(),
                        name,
                    },
                ));
            }
        }
    }
    None
}

fn pricing_for_model<'a>(
    models: &'a HashMap<String, ModelPricing>,
    model: &str,
) -> Option<(&'a str, &'a ModelPricing)> {
    if let Some((pattern, pricing)) = models.get_key_value(model) {
        return Some((pattern.as_str(), pricing));
    }
    models
        .iter()
        .filter_map(|(pattern, pricing)| {
            crate::routes::model_pattern_specificity(pattern, model)
                .map(|specificity| (specificity, pattern, pricing))
        })
        .max_by_key(|(specificity, _, _)| *specificity)
        .map(|(_, pattern, pricing)| (pattern.as_str(), pricing))
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
    use super::{
        ModelPricing, PricingName, PricingScope, PricingTable, calculate, effective_pricing,
    };
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
                ..PricingTable::default()
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
                ..PricingTable::default()
            }),
            ..ApiEndpoint::default()
        };
        let pricing = effective_pricing(
            &PricingTable::default(),
            &provider,
            &endpoint,
            "model",
            "model",
        )
        .unwrap()
        .pricing;
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
            api_type: ApiType::Extension("openai_codex"),
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
                "gpt-5.4-mini",
                "gpt-5.4-mini"
            )
            .is_none()
        );
        assert!(
            effective_pricing(
                &PricingTable::default(),
                &provider,
                &qwen_endpoint,
                "qwen3.8-flash",
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
        let pricing = effective_pricing(
            &PricingTable::default(),
            &provider,
            &endpoint,
            "model",
            "model",
        )
        .unwrap()
        .pricing;
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
            ..PricingTable::default()
        };
        let provider = Provider {
            pricing: None,
            ..provider()
        };
        let endpoint = ApiEndpoint::default();

        let exact = effective_pricing(
            &global,
            &provider,
            &endpoint,
            "model-fast-one",
            "model-fast-one",
        )
        .unwrap()
        .pricing;
        assert_eq!(exact.input_per_million, Some(5.0));
        assert_eq!(exact.output_per_million, Some(6.0));

        let longest = effective_pricing(
            &global,
            &provider,
            &endpoint,
            "model-fast-two",
            "model-fast-two",
        )
        .unwrap()
        .pricing;
        assert_eq!(longest.input_per_million, Some(3.0));
        assert_eq!(longest.output_per_million, Some(4.0));
    }

    #[test]
    fn pricing_patterns_allow_only_one_trailing_wildcard() {
        let table = PricingTable {
            models: HashMap::from([("model-*-invalid".to_owned(), ModelPricing::default())]),
            ..PricingTable::default()
        };
        assert!(super::validate_table(&table, "test", true).is_err());
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
            ..PricingTable::default()
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
                ..PricingTable::default()
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
                ..PricingTable::default()
            }),
            ..ApiEndpoint::default()
        };

        let pricing = effective_pricing(&global, &provider, &endpoint, "model", "model")
            .unwrap()
            .pricing;
        assert_eq!(pricing.input_per_million, Some(0.8));
        assert_eq!(pricing.output_per_million, Some(2.0));
        assert_eq!(pricing.cache_read_per_million, Some(0.1));
    }

    #[test]
    fn the_global_alias_price_covers_destinations_without_an_outgoing_price() {
        let global = PricingTable {
            updated_at: 1,
            incoming_models: HashMap::from([(
                "deepseek-flash".to_owned(),
                ModelPricing {
                    input_per_million: Some(0.3),
                    output_per_million: Some(1.2),
                    ..ModelPricing::default()
                },
            )]),
            ..PricingTable::default()
        };
        let provider = Provider {
            pricing: None,
            ..provider()
        };
        let endpoint = ApiEndpoint::default();

        // One alias rule prices a destination whose provider-facing name differs.
        let resolved = effective_pricing(
            &global,
            &provider,
            &endpoint,
            "deepseek-flash",
            "deepseek/deepseek-v4.1-flash",
        )
        .unwrap();
        assert_eq!(resolved.pricing.input_per_million, Some(0.3));
        assert_eq!(resolved.pricing.output_per_million, Some(1.2));
        let source = resolved.sources.input.unwrap();
        assert_eq!(source.scope, PricingScope::Global);
        assert_eq!(source.name, PricingName::Incoming);
        assert_eq!(source.pattern, "deepseek-flash");
    }

    #[test]
    fn outgoing_prices_win_field_by_field_over_the_alias_price() {
        let global = PricingTable {
            updated_at: 1,
            models: HashMap::from([(
                "deepseek/deepseek-v4.1-flash".to_owned(),
                ModelPricing {
                    output_per_million: Some(9.0),
                    ..ModelPricing::default()
                },
            )]),
            incoming_models: HashMap::from([(
                "deepseek-flash".to_owned(),
                ModelPricing {
                    input_per_million: Some(0.3),
                    output_per_million: Some(1.2),
                    ..ModelPricing::default()
                },
            )]),
        };
        let provider = Provider {
            pricing: None,
            ..provider()
        };
        let endpoint = ApiEndpoint::default();

        let resolved = effective_pricing(
            &global,
            &provider,
            &endpoint,
            "deepseek-flash",
            "deepseek/deepseek-v4.1-flash",
        )
        .unwrap();
        assert_eq!(resolved.pricing.output_per_million, Some(9.0));
        // The alias rule fills the field the outgoing rule leaves unset.
        assert_eq!(resolved.pricing.input_per_million, Some(0.3));
        assert_eq!(resolved.sources.output.unwrap().name, PricingName::Outgoing);
        assert_eq!(resolved.sources.input.unwrap().name, PricingName::Incoming);
    }

    #[test]
    fn a_narrower_outgoing_scope_beats_the_global_alias_price() {
        let global = PricingTable {
            updated_at: 1,
            incoming_models: HashMap::from([(
                "deepseek-flash".to_owned(),
                ModelPricing {
                    input_per_million: Some(0.3),
                    ..ModelPricing::default()
                },
            )]),
            ..PricingTable::default()
        };
        let provider = Provider {
            pricing: None,
            ..provider()
        };
        let endpoint = ApiEndpoint {
            pricing: Some(PricingTable {
                updated_at: 2,
                models: HashMap::from([(
                    "deepseek-flash".to_owned(),
                    ModelPricing {
                        input_per_million: Some(7.0),
                        ..ModelPricing::default()
                    },
                )]),
                ..PricingTable::default()
            }),
            ..ApiEndpoint::default()
        };

        let resolved = effective_pricing(
            &global,
            &provider,
            &endpoint,
            "deepseek-flash",
            "deepseek-flash",
        )
        .unwrap();
        assert_eq!(resolved.pricing.input_per_million, Some(7.0));
        assert_eq!(
            resolved.sources.input.unwrap().scope,
            PricingScope::Endpoint
        );
    }

    #[test]
    fn incoming_prices_are_global_only() {
        let table = PricingTable {
            incoming_models: HashMap::from([(
                "model".to_owned(),
                ModelPricing {
                    input_per_million: Some(1.0),
                    ..ModelPricing::default()
                },
            )]),
            ..PricingTable::default()
        };
        assert!(super::validate_table(&table, "Provider 'p'", false).is_err());
        assert!(super::validate_table(&table, "Global pricing", true).is_ok());
    }

    #[test]
    fn one_pattern_cannot_be_priced_as_both_names() {
        let table = PricingTable {
            models: HashMap::from([("model".to_owned(), ModelPricing::default())]),
            incoming_models: HashMap::from([("model".to_owned(), ModelPricing::default())]),
            ..PricingTable::default()
        };
        assert!(super::validate_table(&table, "Global pricing", true).is_err());
    }

    #[test]
    fn patterns_with_surrounding_whitespace_can_never_match_so_they_are_rejected() {
        let table = PricingTable {
            models: HashMap::from([("model ".to_owned(), ModelPricing::default())]),
            ..PricingTable::default()
        };
        assert!(super::validate_table(&table, "Global pricing", true).is_err());
    }
}

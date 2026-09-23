use std::{
    io::ErrorKind,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::Mutex};
use tracing::error;

pub(crate) const ACTIVITY_FILE: &str = "data/activity.jsonl";
pub(crate) const ACTIVITY_SETTINGS_FILE: &str = "data/activity-settings.json";
pub(crate) const ACTIVITY_TRANSACTION_FILE: &str = "data/activity-transaction.json";
const FLUSH_SIZE: usize = 10;
const FLUSH_INTERVAL: Duration = Duration::from_secs(60);
const DEFAULT_RETENTION_DAYS: u64 = 30;
const COST_SCALE: f64 = 1_000_000_000_000.0;

fn cost_to_units(cost: Option<f64>) -> i128 {
    let Some(cost) = cost.filter(|cost| cost.is_finite()) else {
        return 0;
    };
    let scaled = cost * COST_SCALE;
    if scaled >= i128::MAX as f64 {
        i128::MAX
    } else if scaled <= i128::MIN as f64 {
        i128::MIN
    } else {
        scaled.round() as i128
    }
}

fn units_to_cost(units: i128) -> f64 {
    units as f64 / COST_SCALE
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RequestLog {
    pub timestamp: u64,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_instance_id: Option<String>,
    /// Stable identity and safe display metadata for the Yabane Gateway API key.
    /// The credential itself and its hash are never recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_api_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_api_key_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_api_key_prefix: Option<String>,
    pub path: String,
    /// The model string exactly as supplied by the caller.
    pub model: String,
    /// The model ID selected by routing and placed in the upstream request.
    /// Older imported or retained records may not contain this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    pub provider: String,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_protocol: Option<String>,
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RequestFailure>,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_response_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_byte_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_ms: Option<u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    #[serde(default)]
    pub cost: Option<f64>,
    /// `reported` is supplied by the upstream response; `estimated` is
    /// computed from Yabane's effective Global/Provider/Endpoint pricing. Missing
    /// source on old records is interpreted as `reported` when cost is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_source: Option<CostSource>,
    /// Upstream protocol's terminal reason or status. Older records may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    pub streaming: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CostSource {
    Reported,
    Estimated,
}

impl CostSource {
    pub fn for_log(log: &RequestLog) -> Option<Self> {
        log.cost.map(|_| log.cost_source.unwrap_or(Self::Reported))
    }
}

fn cost_counts(log: &RequestLog) -> (usize, usize) {
    match CostSource::for_log(log) {
        Some(CostSource::Reported) => (1, 0),
        Some(CostSource::Estimated) => (0, 1),
        None => (0, 0),
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub enum CostRecalculationResolution {
    Available(crate::pricing::ModelPricing),
    MissingRoute,
    MissingPricing,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct CostRecalculationResult {
    pub candidates: usize,
    pub updated: usize,
    pub filled: usize,
    pub recalculated: usize,
    pub reported_preserved: usize,
    pub skipped_missing_route: usize,
    pub skipped_missing_pricing: usize,
    pub skipped_missing_usage: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RequestFailure {
    pub stage: String,
    pub category: String,
    pub message: String,
}

impl RequestFailure {
    pub fn new(stage: &str, category: &str, message: impl Into<String>) -> Self {
        Self {
            stage: stage.to_owned(),
            category: category.to_owned(),
            message: message.into(),
        }
    }
}

pub const UNATTRIBUTED_API_KEY_FILTER: &str = "__unattributed__";

#[derive(Clone, Copy, Default)]
pub struct ActivityFilters<'a> {
    pub providers: &'a [String],
    pub models: &'a [String],
    pub api_keys: &'a [String],
}

impl ActivityFilters<'_> {
    fn matches(&self, log: &RequestLog) -> bool {
        (self.providers.is_empty() || self.providers.iter().any(|value| value == &log.provider))
            && (self.models.is_empty() || self.models.iter().any(|value| value == &log.model))
            && (self.api_keys.is_empty()
                || self
                    .api_keys
                    .iter()
                    .any(|value| match &log.gateway_api_key_id {
                        Some(id) => value == id,
                        None => value == UNATTRIBUTED_API_KEY_FILTER,
                    }))
    }
}

pub struct ActivityLogQuery<'a> {
    pub since: u64,
    pub until: u64,
    pub filters: ActivityFilters<'a>,
    pub text: Option<&'a str>,
    pub status: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Clone)]
pub struct ActivityStore {
    inner: Arc<Mutex<ActivityData>>,
    flush_lock: Arc<Mutex<()>>,
    instance_id: Arc<String>,
    retention_days: Arc<AtomicU64>,
}

#[derive(Default)]
struct ActivityData {
    persisted: Vec<RequestLog>,
    pending: Vec<RequestLog>,
}

#[derive(Serialize)]
pub struct ActivityExport<'a> {
    pub format: &'static str,
    pub version: u32,
    pub instance_id: &'a str,
    pub exported_at: u64,
    pub records: &'a [RequestLog],
}

#[derive(Deserialize)]
pub struct ActivityImport {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub instance_id: Option<String>,
    pub records: Vec<RequestLog>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ActivitySettings {
    pub retention_days: u64,
}

#[derive(Serialize)]
pub struct ActivitySummary {
    pub records: usize,
    pub estimated_bytes: usize,
    pub oldest_at: Option<u64>,
    pub newest_at: Option<u64>,
    pub retention_days: u64,
}

#[derive(Serialize)]
pub struct ImportResult {
    pub imported: usize,
    pub duplicates: usize,
    pub expired: usize,
    pub total: usize,
    pub oldest_at: Option<u64>,
    pub newest_at: Option<u64>,
}

pub enum ActivityImportError {
    Invalid(String),
    Persist(std::io::Error),
}

#[derive(Serialize)]
pub struct Stats {
    pub requests: usize,
    pub priced_requests: usize,
    pub reported_requests: usize,
    pub estimated_requests: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    pub successful: usize,
    pub streaming: usize,
    pub latency_ms: u64,
    pub by_provider: Vec<DimensionStats>,
    pub by_model: Vec<DimensionStats>,
    pub by_api_key: Vec<ApiKeyStats>,
    pub buckets: Vec<ActivityBucket>,
    pub provider_buckets: Vec<ProviderBuckets>,
    pub filter_options: ActivityFilterOptions,
}

#[derive(Serialize)]
pub struct ActivityFilterOptions {
    pub providers: Vec<String>,
    pub models: Vec<String>,
    pub api_keys: Vec<ActivityFilterApiKey>,
}

#[derive(Serialize)]
pub struct ActivityFilterApiKey {
    pub id: String,
    pub name: String,
    pub prefix: Option<String>,
}

#[derive(Serialize)]
pub struct ProviderBuckets {
    pub name: String,
    pub requests: Vec<usize>,
}

#[derive(Serialize)]
pub struct DimensionStats {
    pub name: String,
    pub requests: usize,
    pub priced_requests: usize,
    pub reported_requests: usize,
    pub estimated_requests: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    #[serde(skip)]
    cost_units: i128,
    pub errors: usize,
    pub latency_ms: u64,
}

#[derive(Serialize)]
pub struct ApiKeyStats {
    pub id: Option<String>,
    pub name: String,
    pub prefix: Option<String>,
    pub requests: usize,
    pub priced_requests: usize,
    pub reported_requests: usize,
    pub estimated_requests: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    #[serde(skip)]
    cost_units: i128,
    pub errors: usize,
    pub latency_ms: u64,
}

#[derive(Serialize)]
pub struct ActivityBucket {
    pub start: u64,
    pub requests: usize,
    pub priced_requests: usize,
    pub reported_requests: usize,
    pub estimated_requests: usize,
    pub tokens: u64,
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub cost: f64,
    #[serde(skip)]
    cost_units: i128,
    pub latency: u64,
    pub samples: usize,
    pub successful: usize,
    pub errors: usize,
}

impl ActivityStore {
    pub async fn load() -> Result<Self, String> {
        let contents = match tokio::fs::read_to_string(ACTIVITY_FILE).await {
            Ok(contents) => contents,
            Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
            Err(err) => return Err(format!("read {ACTIVITY_FILE}: {err}")),
        };
        let retention_days = load_retention_days().await?;
        let cutoff = retention_cutoff(retention_days);
        let persisted = contents
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_str::<RequestLog>(line)
                    .map_err(|err| format!("parse {ACTIVITY_FILE}: {err}"))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|log| log.timestamp >= cutoff)
            .collect();
        Ok(Self {
            inner: Arc::new(Mutex::new(ActivityData {
                persisted,
                pending: Vec::new(),
            })),
            flush_lock: Arc::new(Mutex::new(())),
            instance_id: Arc::new(load_or_create_instance_id().await?),
            retention_days: Arc::new(AtomicU64::new(retention_days)),
        })
    }

    pub async fn record(&self, log: RequestLog) {
        let should_flush = {
            let mut data = self.inner.lock().await;
            data.pending.push(log);
            data.pending.len() >= FLUSH_SIZE
        };
        if should_flush {
            self.flush().await;
        }
    }

    pub async fn logs(
        &self,
        since: u64,
        until: u64,
        filters: ActivityFilters<'_>,
        limit: usize,
    ) -> Vec<RequestLog> {
        let since = since.max(retention_cutoff(self.retention_days()));
        let data = self.inner.lock().await;
        data.persisted
            .iter()
            .chain(&data.pending)
            .filter(|log| log.timestamp >= since && log.timestamp <= until && filters.matches(log))
            .rev()
            .take(limit.min(1000))
            .cloned()
            .collect()
    }

    pub async fn query_logs(&self, query: ActivityLogQuery<'_>) -> (Vec<RequestLog>, usize) {
        let since = query.since.max(retention_cutoff(self.retention_days()));
        let text = query
            .text
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_lowercase);
        let data = self.inner.lock().await;
        let matches = |log: &&RequestLog| {
            log.timestamp >= since
                && log.timestamp <= query.until
                && query.filters.matches(log)
                && query.status.is_none_or(|status| match status {
                    "success" => log.status < 400,
                    "error" => log.status >= 400,
                    _ => true,
                })
                && text.as_ref().is_none_or(|text| {
                    [&log.request_id, &log.model, &log.provider, &log.endpoint]
                        .iter()
                        .any(|value| value.to_lowercase().contains(text))
                        || [
                            log.gateway_api_key_id.as_deref(),
                            log.gateway_api_key_note.as_deref(),
                            log.gateway_api_key_prefix.as_deref(),
                        ]
                        .into_iter()
                        .flatten()
                        .any(|value| value.to_lowercase().contains(text))
                        || log
                            .upstream_model
                            .as_ref()
                            .is_some_and(|model| model.to_lowercase().contains(text))
                })
        };
        let total = data
            .persisted
            .iter()
            .chain(&data.pending)
            .filter(matches)
            .count();
        let logs = data
            .persisted
            .iter()
            .chain(&data.pending)
            .rev()
            .filter(matches)
            .skip(query.offset)
            .take(query.limit.min(100))
            .cloned()
            .collect();
        (logs, total)
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub async fn export_records(&self, since: u64) -> Vec<RequestLog> {
        let since = since.max(retention_cutoff(self.retention_days()));
        let data = self.inner.lock().await;
        let mut records: Vec<_> = data
            .persisted
            .iter()
            .chain(&data.pending)
            .filter(|record| record.timestamp >= since)
            .cloned()
            .collect();
        records.sort_by(|a, b| {
            a.timestamp
                .cmp(&b.timestamp)
                .then_with(|| a.request_id.cmp(&b.request_id))
        });
        records
    }

    pub async fn summary(&self, since: u64) -> ActivitySummary {
        let records = self.export_records(since).await;
        ActivitySummary {
            records: records.len(),
            estimated_bytes: serialize_logs(&records).len(),
            oldest_at: records.first().map(|record| record.timestamp),
            newest_at: records.last().map(|record| record.timestamp),
            retention_days: self.retention_days(),
        }
    }

    pub fn retention_days(&self) -> u64 {
        self.retention_days.load(Ordering::Relaxed)
    }

    pub async fn set_retention_days(&self, days: u64) -> Result<(), String> {
        self.set_retention_days_at(
            days,
            ACTIVITY_SETTINGS_FILE,
            ACTIVITY_FILE,
            ACTIVITY_TRANSACTION_FILE,
        )
        .await
    }

    async fn set_retention_days_at(
        &self,
        days: u64,
        settings_path: &str,
        activity_path: &str,
        transaction_path: &str,
    ) -> Result<(), String> {
        if !(1..=3650).contains(&days) {
            return Err("Activity retention must be between 1 and 3650 days".to_owned());
        }
        let _flush_guard = self.flush_lock.lock().await;
        let mut data = self.inner.lock().await;
        let cutoff = retention_cutoff(days);
        let records: Vec<_> = data
            .persisted
            .iter()
            .chain(&data.pending)
            .filter(|log| log.timestamp >= cutoff)
            .cloned()
            .collect();
        let writes = [
            crate::storage::AtomicWrite::bytes(activity_path, serialize_logs(&records))
                .map_err(|err| format!("prepare compacted activity: {err}"))?,
            crate::storage::AtomicWrite::json(
                settings_path,
                &ActivitySettings {
                    retention_days: days,
                },
            )
            .map_err(|err| format!("prepare activity settings: {err}"))?,
        ];
        crate::storage::write_transaction(transaction_path, &writes)
            .await
            .map_err(|err| format!("save activity retention: {err}"))?;
        data.persisted = records;
        data.pending.clear();
        self.retention_days.store(days, Ordering::Relaxed);
        Ok(())
    }

    pub async fn recalculate_non_reported_costs<F>(
        &self,
        record: Option<(&str, Option<&str>)>,
        pricing_for: F,
    ) -> Result<CostRecalculationResult, std::io::Error>
    where
        F: Fn(&RequestLog) -> CostRecalculationResolution,
    {
        self.recalculate_non_reported_costs_at(ACTIVITY_FILE, record, pricing_for)
            .await
    }

    async fn recalculate_non_reported_costs_at<F>(
        &self,
        activity_path: &str,
        record: Option<(&str, Option<&str>)>,
        pricing_for: F,
    ) -> Result<CostRecalculationResult, std::io::Error>
    where
        F: Fn(&RequestLog) -> CostRecalculationResolution,
    {
        let _flush_guard = self.flush_lock.lock().await;
        let mut data = self.inner.lock().await;
        let mut records: Vec<_> = data
            .persisted
            .iter()
            .chain(&data.pending)
            .cloned()
            .collect();
        let mut result = CostRecalculationResult::default();
        for log in &mut records {
            if record.is_some_and(|(request_id, source_instance_id)| {
                request_id != log.request_id
                    || source_instance_id != log.source_instance_id.as_deref()
            }) {
                continue;
            }
            if matches!(CostSource::for_log(log), Some(CostSource::Reported)) {
                result.reported_preserved += 1;
                continue;
            }
            result.candidates += 1;
            if log.input_tokens == 0 && log.output_tokens == 0 && log.cached_tokens == 0 {
                result.skipped_missing_usage += 1;
                continue;
            }
            let resolution = pricing_for(log);
            let pricing = match resolution {
                CostRecalculationResolution::Available(pricing) => pricing,
                CostRecalculationResolution::MissingRoute => {
                    result.skipped_missing_route += 1;
                    continue;
                }
                CostRecalculationResolution::MissingPricing => {
                    result.skipped_missing_pricing += 1;
                    continue;
                }
            };
            let usage = crate::usage::TokenUsage {
                input: log.input_tokens,
                output: log.output_tokens,
                cached: log.cached_tokens,
                cost: None,
                finish_reason: None,
            };
            let Some(cost) = crate::pricing::calculate(&pricing, &usage) else {
                result.skipped_missing_pricing += 1;
                continue;
            };
            let recalculated = log.cost.is_some();
            log.cost = Some(cost);
            log.cost_source = Some(CostSource::Estimated);
            result.updated += 1;
            if recalculated {
                result.recalculated += 1;
            } else {
                result.filled += 1;
            }
        }
        if result.updated == 0 {
            return Ok(result);
        }
        write_logs_at(activity_path, &records).await?;
        data.persisted = records;
        data.pending.clear();
        Ok(result)
    }

    pub async fn preview_import(&self, import: &ActivityImport) -> Result<ImportResult, String> {
        self.validate_import(import)?;
        let data = self.inner.lock().await;
        Ok(self.classify_import(import, &data).0)
    }

    pub async fn import(
        &self,
        import: ActivityImport,
    ) -> Result<ImportResult, ActivityImportError> {
        self.validate_import(&import)
            .map_err(ActivityImportError::Invalid)?;
        let _flush_guard = self.flush_lock.lock().await;
        let mut data = self.inner.lock().await;
        let (result, imported) = self.classify_import(&import, &data);
        if !imported.is_empty() {
            let cutoff = retention_cutoff(self.retention_days());
            let mut records: Vec<_> = data
                .persisted
                .iter()
                .chain(&data.pending)
                .filter(|log| log.timestamp >= cutoff)
                .cloned()
                .collect();
            records.extend(imported);
            records.sort_by(|a, b| {
                a.timestamp
                    .cmp(&b.timestamp)
                    .then_with(|| a.request_id.cmp(&b.request_id))
            });
            write_logs(&records)
                .await
                .map_err(ActivityImportError::Persist)?;
            data.persisted = records;
            data.pending.clear();
        }
        Ok(result)
    }

    fn validate_import(&self, import: &ActivityImport) -> Result<(), String> {
        if import.format != "yabane-activity" || import.version != 1 {
            return Err("Unsupported activity export format or version".to_owned());
        }
        if import.records.len() > 1_000_000 {
            return Err("Activity import cannot contain more than 1000000 records".to_owned());
        }
        if import
            .records
            .iter()
            .any(|record| record.request_id.is_empty())
        {
            return Err("Activity records must contain a request_id".to_owned());
        }
        Ok(())
    }

    fn classify_import(
        &self,
        import: &ActivityImport,
        data: &ActivityData,
    ) -> (ImportResult, Vec<RequestLog>) {
        let cutoff = retention_cutoff(self.retention_days());
        let source_id = import
            .instance_id
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or("legacy");
        let local_source = self.instance_id.as_str();
        let mut existing: std::collections::HashSet<(String, String)> = data
            .persisted
            .iter()
            .chain(&data.pending)
            .map(|record| {
                (
                    record
                        .source_instance_id
                        .as_deref()
                        .unwrap_or(local_source)
                        .to_owned(),
                    record.request_id.clone(),
                )
            })
            .collect();
        let mut accepted = Vec::new();
        let mut duplicates = 0;
        let mut expired = 0;
        for record in &import.records {
            if record.timestamp < cutoff {
                expired += 1;
                continue;
            }
            let identity = (
                record
                    .source_instance_id
                    .as_deref()
                    .unwrap_or(source_id)
                    .to_owned(),
                record.request_id.clone(),
            );
            if !existing.insert(identity) {
                duplicates += 1;
                continue;
            }
            let mut record = record.clone();
            if record.source_instance_id.is_none() && source_id != local_source {
                record.source_instance_id = Some(source_id.to_owned());
            }
            accepted.push(record);
        }
        (
            ImportResult {
                imported: accepted.len(),
                duplicates,
                expired,
                total: import.records.len(),
                oldest_at: import.records.iter().map(|record| record.timestamp).min(),
                newest_at: import.records.iter().map(|record| record.timestamp).max(),
            },
            accepted,
        )
    }

    pub async fn stats(
        &self,
        since: u64,
        filters: ActivityFilters<'_>,
        bucket_count: usize,
        until: u64,
    ) -> Stats {
        let since = since.max(retention_cutoff(self.retention_days()));
        let data = self.inner.lock().await;
        let range_logs: Vec<_> = data
            .persisted
            .iter()
            .chain(&data.pending)
            .filter(|log| log.timestamp >= since && log.timestamp <= until)
            .collect();
        let logs: Vec<_> = range_logs
            .iter()
            .copied()
            .filter(|log| filters.matches(log))
            .collect();
        let dimensions = |key: fn(&RequestLog) -> &str| {
            let mut values = std::collections::BTreeMap::<String, DimensionStats>::new();
            for log in &logs {
                let name = key(log).to_owned();
                let stats = values.entry(name.clone()).or_insert(DimensionStats {
                    name,
                    requests: 0,
                    priced_requests: 0,
                    reported_requests: 0,
                    estimated_requests: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    cached_tokens: 0,
                    cost: 0.0,
                    cost_units: 0,
                    errors: 0,
                    latency_ms: 0,
                });
                stats.requests += 1;
                stats.priced_requests += usize::from(log.cost.is_some());
                let (reported, estimated) = cost_counts(log);
                stats.reported_requests += reported;
                stats.estimated_requests += estimated;
                stats.input_tokens += log.input_tokens;
                stats.output_tokens += log.output_tokens;
                stats.cached_tokens += log.cached_tokens;
                stats.cost_units = stats.cost_units.saturating_add(cost_to_units(log.cost));
                stats.errors += usize::from(log.status >= 400);
                stats.latency_ms += log.latency_ms;
            }
            let mut values: Vec<_> = values.into_values().collect();
            for value in &mut values {
                value.cost = units_to_cost(value.cost_units);
            }
            values.sort_by(|left, right| {
                right
                    .requests
                    .cmp(&left.requests)
                    .then_with(|| left.name.cmp(&right.name))
            });
            values
        };
        let mut api_keys = std::collections::BTreeMap::<Option<String>, ApiKeyStats>::new();
        for log in &logs {
            let id = log.gateway_api_key_id.clone();
            let stats = api_keys.entry(id.clone()).or_insert_with(|| ApiKeyStats {
                id,
                name: log
                    .gateway_api_key_note
                    .as_deref()
                    .filter(|note| !note.trim().is_empty())
                    .or(log.gateway_api_key_prefix.as_deref())
                    .unwrap_or("Unattributed")
                    .to_owned(),
                prefix: log.gateway_api_key_prefix.clone(),
                requests: 0,
                priced_requests: 0,
                reported_requests: 0,
                estimated_requests: 0,
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
                cost: 0.0,
                cost_units: 0,
                errors: 0,
                latency_ms: 0,
            });
            stats.requests += 1;
            stats.priced_requests += usize::from(log.cost.is_some());
            let (reported, estimated) = cost_counts(log);
            stats.reported_requests += reported;
            stats.estimated_requests += estimated;
            stats.input_tokens += log.input_tokens;
            stats.output_tokens += log.output_tokens;
            stats.cached_tokens += log.cached_tokens;
            stats.cost_units = stats.cost_units.saturating_add(cost_to_units(log.cost));
            stats.errors += usize::from(log.status >= 400);
            stats.latency_ms += log.latency_ms;
        }
        let mut by_api_key: Vec<_> = api_keys.into_values().collect();
        for value in &mut by_api_key {
            value.cost = units_to_cost(value.cost_units);
        }
        by_api_key.sort_by(|left, right| {
            right
                .requests
                .cmp(&left.requests)
                .then_with(|| left.name.cmp(&right.name))
        });
        let bucket_count = bucket_count.clamp(1, 336);
        let width = until
            .saturating_sub(since)
            .max(1)
            .div_ceil(bucket_count as u64);
        let mut buckets: Vec<_> = (0..bucket_count)
            .map(|index| ActivityBucket {
                start: since.saturating_add(index as u64 * width),
                requests: 0,
                priced_requests: 0,
                reported_requests: 0,
                estimated_requests: 0,
                tokens: 0,
                input: 0,
                output: 0,
                cached: 0,
                cost: 0.0,
                cost_units: 0,
                latency: 0,
                samples: 0,
                successful: 0,
                errors: 0,
            })
            .collect();
        let mut provider_buckets = std::collections::BTreeMap::<String, Vec<usize>>::new();
        for log in &logs {
            let index = ((log.timestamp.saturating_sub(since)) / width) as usize;
            let index = index.min(bucket_count - 1);
            if let Some(bucket) = buckets.get_mut(index) {
                bucket.requests += 1;
                bucket.priced_requests += usize::from(log.cost.is_some());
                let (reported, estimated) = cost_counts(log);
                bucket.reported_requests += reported;
                bucket.estimated_requests += estimated;
                bucket.tokens += log.input_tokens + log.output_tokens;
                bucket.input += log.input_tokens;
                bucket.output += log.output_tokens;
                bucket.cached += log.cached_tokens;
                bucket.cost_units = bucket.cost_units.saturating_add(cost_to_units(log.cost));
                bucket.latency += log.latency_ms;
                bucket.samples += 1;
                bucket.successful += usize::from(log.status < 400);
                bucket.errors += usize::from(log.status >= 400);
            }
            provider_buckets
                .entry(log.provider.clone())
                .or_insert_with(|| vec![0; bucket_count])[index] += 1;
        }
        let mut filter_providers: Vec<_> = range_logs
            .iter()
            .map(|log| log.provider.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut filter_models: Vec<_> = range_logs
            .iter()
            .map(|log| log.model.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        filter_providers.sort();
        filter_models.sort();
        let mut filter_api_keys = std::collections::BTreeMap::<String, ActivityFilterApiKey>::new();
        for log in &range_logs {
            let id = log
                .gateway_api_key_id
                .clone()
                .unwrap_or_else(|| UNATTRIBUTED_API_KEY_FILTER.to_owned());
            filter_api_keys
                .entry(id.clone())
                .or_insert_with(|| ActivityFilterApiKey {
                    id,
                    name: log
                        .gateway_api_key_note
                        .as_deref()
                        .filter(|note| !note.trim().is_empty())
                        .or(log.gateway_api_key_prefix.as_deref())
                        .unwrap_or("Unattributed")
                        .to_owned(),
                    prefix: log.gateway_api_key_prefix.clone(),
                });
        }
        let mut filter_api_keys: Vec<_> = filter_api_keys.into_values().collect();
        filter_api_keys.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
        let cost_units = logs.iter().fold(0_i128, |total, log| {
            total.saturating_add(cost_to_units(log.cost))
        });
        for bucket in &mut buckets {
            bucket.cost = units_to_cost(bucket.cost_units);
        }
        Stats {
            requests: logs.len(),
            priced_requests: logs.iter().filter(|log| log.cost.is_some()).count(),
            reported_requests: logs
                .iter()
                .filter(|log| matches!(CostSource::for_log(log), Some(CostSource::Reported)))
                .count(),
            estimated_requests: logs
                .iter()
                .filter(|log| matches!(CostSource::for_log(log), Some(CostSource::Estimated)))
                .count(),
            input_tokens: logs.iter().map(|log| log.input_tokens).sum(),
            output_tokens: logs.iter().map(|log| log.output_tokens).sum(),
            cached_tokens: logs.iter().map(|log| log.cached_tokens).sum(),
            cost: units_to_cost(cost_units),
            successful: logs.iter().filter(|log| log.status < 400).count(),
            streaming: logs.iter().filter(|log| log.streaming).count(),
            latency_ms: logs.iter().map(|log| log.latency_ms).sum(),
            by_provider: dimensions(|log| &log.provider),
            by_model: dimensions(|log| &log.model),
            by_api_key,
            buckets,
            provider_buckets: provider_buckets
                .into_iter()
                .map(|(name, requests)| ProviderBuckets { name, requests })
                .collect(),
            filter_options: ActivityFilterOptions {
                providers: filter_providers,
                models: filter_models,
                api_keys: filter_api_keys,
            },
        }
    }

    pub async fn flush(&self) {
        let _flush_guard = self.flush_lock.lock().await;
        let (pending, retained, needs_compaction) = {
            let mut data = self.inner.lock().await;
            let cutoff = retention_cutoff(self.retention_days());
            let retained: Vec<_> = data
                .persisted
                .iter()
                .filter(|log| log.timestamp >= cutoff)
                .cloned()
                .collect();
            let needs_compaction = retained.len() != data.persisted.len();
            if data.pending.is_empty() && !needs_compaction {
                return;
            }
            (
                std::mem::take(&mut data.pending),
                retained,
                needs_compaction,
            )
        };

        let result = if needs_compaction {
            let mut logs = retained.clone();
            logs.extend(pending.iter().cloned());
            write_logs(&logs).await
        } else {
            append_logs(&pending).await
        };

        let mut data = self.inner.lock().await;
        match result {
            Ok(()) => {
                if needs_compaction {
                    data.persisted = retained;
                }
                data.persisted.extend(pending);
            }
            Err(err) => {
                error!(%err, "failed to flush request activity");
                let newer = std::mem::take(&mut data.pending);
                data.pending = pending;
                data.pending.extend(newer);
            }
        }
    }

    pub fn start_flusher(&self) {
        let store = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval_at(
                tokio::time::Instant::now() + FLUSH_INTERVAL,
                FLUSH_INTERVAL,
            );
            loop {
                interval.tick().await;
                store.flush().await;
            }
        });
    }
}

async fn append_logs(logs: &[RequestLog]) -> Result<(), std::io::Error> {
    tokio::fs::create_dir_all("data").await?;
    let contents = serialize_logs(logs);
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(ACTIVITY_FILE)
        .await?
        .write_all(&contents)
        .await
}

async fn write_logs(logs: &[RequestLog]) -> Result<(), std::io::Error> {
    write_logs_at(ACTIVITY_FILE, logs).await
}

async fn write_logs_at(path: &str, logs: &[RequestLog]) -> Result<(), std::io::Error> {
    crate::storage::write_atomic(path, &serialize_logs(logs)).await
}

fn serialize_logs(logs: &[RequestLog]) -> Vec<u8> {
    let mut contents = Vec::new();
    for log in logs {
        serde_json::to_writer(&mut contents, log).expect("serialize activity");
        contents.push(b'\n');
    }
    contents
}

async fn load_or_create_instance_id() -> Result<String, String> {
    const INSTANCE_ID_FILE: &str = "data/instance-id";
    match tokio::fs::read_to_string(INSTANCE_ID_FILE).await {
        Ok(value) if !value.trim().is_empty() => return Ok(value.trim().to_owned()),
        Ok(_) => return Err(format!("{INSTANCE_ID_FILE} is empty")),
        Err(err) if err.kind() != ErrorKind::NotFound => {
            return Err(format!("read {INSTANCE_ID_FILE}: {err}"));
        }
        Err(_) => {}
    }
    let mut random = [0_u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut random);
    let id = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    crate::storage::write_atomic(INSTANCE_ID_FILE, id.as_bytes())
        .await
        .map_err(|err| format!("create {INSTANCE_ID_FILE}: {err}"))?;
    Ok(id)
}

async fn load_retention_days() -> Result<u64, String> {
    match tokio::fs::read(ACTIVITY_SETTINGS_FILE).await {
        Ok(contents) => {
            let settings: ActivitySettings = serde_json::from_slice(&contents)
                .map_err(|err| format!("parse {ACTIVITY_SETTINGS_FILE}: {err}"))?;
            if !(1..=3650).contains(&settings.retention_days) {
                return Err(format!(
                    "{ACTIVITY_SETTINGS_FILE} retention_days must be between 1 and 3650"
                ));
            }
            Ok(settings.retention_days)
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {
            let days = match std::env::var("YABANE_ACTIVITY_RETENTION_DAYS") {
                Ok(value) => value.parse::<u64>().map_err(|_| {
                    "YABANE_ACTIVITY_RETENTION_DAYS must be an integer between 1 and 3650"
                        .to_owned()
                })?,
                Err(std::env::VarError::NotPresent) => DEFAULT_RETENTION_DAYS,
                Err(err) => return Err(format!("read YABANE_ACTIVITY_RETENTION_DAYS: {err}")),
            };
            if !(1..=3650).contains(&days) {
                return Err("YABANE_ACTIVITY_RETENTION_DAYS must be between 1 and 3650".to_owned());
            }
            Ok(days)
        }
        Err(err) => Err(format!("read {ACTIVITY_SETTINGS_FILE}: {err}")),
    }
}

fn retention_cutoff(days: u64) -> u64 {
    crate::auth::now().saturating_sub(days.saturating_mul(86_400))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(timestamp: u64, id: &str, provider: &str, status: u16) -> RequestLog {
        RequestLog {
            timestamp,
            request_id: id.to_owned(),
            source_instance_id: None,
            gateway_api_key_id: None,
            gateway_api_key_note: None,
            gateway_api_key_prefix: None,
            path: "/v1/responses".to_owned(),
            model: format!("{provider}/model-{id}"),
            upstream_model: Some(format!("model-{id}")),
            provider: provider.to_owned(),
            endpoint: format!("endpoint-{provider}"),
            caller_protocol: None,
            upstream_protocol: None,
            status,
            failure: None,
            latency_ms: 1,
            gateway_ms: None,
            upstream_response_ms: None,
            first_byte_ms: None,
            generation_ms: None,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            cost: None,
            cost_source: None,
            finish_reason: None,
            streaming: false,
        }
    }

    #[test]
    fn legacy_record_without_upstream_model_remains_readable() {
        let encoded = r#"{"timestamp":1,"request_id":"legacy","path":"/v1/responses","model":"alias","provider":"provider","endpoint":"endpoint","status":200,"latency_ms":1,"input_tokens":0,"output_tokens":0,"cached_tokens":0,"streaming":false}"#;
        let decoded: RequestLog = serde_json::from_str(encoded).unwrap();
        assert_eq!(decoded.model, "alias");
        assert_eq!(decoded.upstream_model, None);
        assert_eq!(decoded.finish_reason, None);
        assert_eq!(decoded.gateway_api_key_id, None);
    }

    #[test]
    fn structured_failure_round_trips_without_losing_categories() {
        let mut log = request(1, "structured", "provider", 429);
        log.failure = Some(RequestFailure::new(
            "upstream_response",
            "rate_limited",
            "Upstream returned HTTP 429",
        ));
        let encoded = serde_json::to_vec(&log).unwrap();
        let decoded: RequestLog = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.failure, log.failure);
    }

    fn store(logs: Vec<RequestLog>) -> ActivityStore {
        ActivityStore {
            inner: Arc::new(Mutex::new(ActivityData {
                persisted: logs,
                pending: Vec::new(),
            })),
            flush_lock: Arc::new(Mutex::new(())),
            instance_id: Arc::new("test-instance".to_owned()),
            retention_days: Arc::new(AtomicU64::new(DEFAULT_RETENTION_DAYS)),
        }
    }

    fn test_directory(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "yabane-activity-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ))
    }

    fn complete_pricing() -> crate::pricing::ModelPricing {
        crate::pricing::ModelPricing {
            input_per_million: Some(1.0),
            output_per_million: Some(2.0),
            cache_read_per_million: Some(0.5),
            cache_write_per_million: None,
        }
    }

    #[tokio::test]
    async fn recalculates_estimated_costs_and_preserves_reported_and_legacy_costs() {
        let now = crate::auth::now();
        let mut missing = request(now, "missing", "provider", 200);
        missing.input_tokens = 1_000;
        missing.output_tokens = 1_000;
        let mut estimated = missing.clone();
        estimated.request_id = "estimated".to_owned();
        estimated.cost = Some(99.0);
        estimated.cost_source = Some(CostSource::Estimated);
        let mut reported = missing.clone();
        reported.request_id = "reported".to_owned();
        reported.cost = Some(7.0);
        reported.cost_source = Some(CostSource::Reported);
        let mut legacy = missing.clone();
        legacy.request_id = "legacy-cost".to_owned();
        legacy.cost = Some(8.0);
        let store = store(vec![missing, estimated, reported, legacy]);
        let directory = test_directory("recalculate-success");
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let activity_path = directory.join("activity.jsonl");

        let result = store
            .recalculate_non_reported_costs_at(activity_path.to_str().unwrap(), None, |_| {
                CostRecalculationResolution::Available(complete_pricing())
            })
            .await
            .unwrap();

        assert_eq!(result.candidates, 2);
        assert_eq!(result.updated, 2);
        assert_eq!(result.filled, 1);
        assert_eq!(result.recalculated, 1);
        assert_eq!(result.reported_preserved, 2);
        let records = store.export_records(0).await;
        let record = |id: &str| {
            records
                .iter()
                .find(|record| record.request_id == id)
                .unwrap()
        };
        assert_eq!(record("missing").cost, Some(0.003));
        assert_eq!(record("missing").cost_source, Some(CostSource::Estimated));
        assert_eq!(record("estimated").cost, Some(0.003));
        assert_eq!(record("reported").cost, Some(7.0));
        assert_eq!(record("reported").cost_source, Some(CostSource::Reported));
        assert_eq!(record("legacy-cost").cost, Some(8.0));
        assert_eq!(record("legacy-cost").cost_source, None);
        assert_eq!(
            tokio::fs::read_to_string(&activity_path)
                .await
                .unwrap()
                .lines()
                .count(),
            4
        );
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn recalculates_persisted_and_pending_records_in_one_atomic_snapshot() {
        let now = crate::auth::now();
        let mut persisted = request(now, "persisted", "provider", 200);
        persisted.input_tokens = 1_000;
        let mut pending = request(now, "pending", "provider", 200);
        pending.input_tokens = 1_000;
        pending.cost = Some(5.0);
        pending.cost_source = Some(CostSource::Estimated);
        let store = store(vec![persisted]);
        store.inner.lock().await.pending.push(pending);
        let directory = test_directory("recalculate-pending");
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let activity_path = directory.join("activity.jsonl");

        let result = store
            .recalculate_non_reported_costs_at(activity_path.to_str().unwrap(), None, |_| {
                CostRecalculationResolution::Available(complete_pricing())
            })
            .await
            .unwrap();

        assert_eq!(result.updated, 2);
        assert_eq!(result.filled, 1);
        assert_eq!(result.recalculated, 1);
        let records = store.export_records(0).await;
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| record.cost == Some(0.001)));
        assert!(store.inner.lock().await.pending.is_empty());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn unavailable_pricing_does_not_clear_an_existing_estimate() {
        let now = crate::auth::now();
        let mut estimated = request(now, "unavailable", "provider", 200);
        estimated.input_tokens = 1_000;
        estimated.cost = Some(5.0);
        estimated.cost_source = Some(CostSource::Estimated);
        let store = store(vec![estimated]);
        let directory = test_directory("recalculate-unavailable");
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let activity_path = directory.join("activity.jsonl");

        let result = store
            .recalculate_non_reported_costs_at(activity_path.to_str().unwrap(), None, |_| {
                CostRecalculationResolution::MissingPricing
            })
            .await
            .unwrap();

        assert_eq!(result.updated, 0);
        assert_eq!(result.skipped_missing_pricing, 1);
        let records = store.export_records(0).await;
        assert_eq!(records[0].cost, Some(5.0));
        assert_eq!(records[0].cost_source, Some(CostSource::Estimated));
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
    #[tokio::test]
    async fn failed_cost_recalculation_does_not_publish_memory_changes() {
        let mut log = request(crate::auth::now(), "pending", "provider", 200);
        log.input_tokens = 1_000;
        let store = store(vec![log]);
        let directory = test_directory("recalculate-failure");
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let blocked_parent = directory.join("blocked");
        tokio::fs::write(&blocked_parent, b"blocked").await.unwrap();
        let activity_path = blocked_parent.join("activity.jsonl");

        let result = store
            .recalculate_non_reported_costs_at(activity_path.to_str().unwrap(), None, |_| {
                CostRecalculationResolution::Available(complete_pricing())
            })
            .await;

        assert!(result.is_err());
        let records = store.export_records(0).await;
        assert_eq!(records[0].cost, None);
        assert_eq!(records[0].cost_source, None);
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn single_cost_recalculation_uses_source_and_request_identity() {
        let mut local = request(crate::auth::now(), "shared-id", "provider", 200);
        local.input_tokens = 1_000;
        let mut imported = local.clone();
        imported.source_instance_id = Some("remote-instance".to_owned());
        let store = store(vec![local, imported]);
        let directory = test_directory("recalculate-identity");
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let activity_path = directory.join("activity.jsonl");

        let result = store
            .recalculate_non_reported_costs_at(
                activity_path.to_str().unwrap(),
                Some(("shared-id", Some("remote-instance"))),
                |_| CostRecalculationResolution::Available(complete_pricing()),
            )
            .await
            .unwrap();

        assert_eq!(result.candidates, 1);
        assert_eq!(result.updated, 1);
        let records = store.export_records(0).await;
        let local = records
            .iter()
            .find(|record| record.source_instance_id.is_none())
            .unwrap();
        let imported = records
            .iter()
            .find(|record| record.source_instance_id.as_deref() == Some("remote-instance"))
            .unwrap();
        assert_eq!(local.cost, None);
        assert_eq!(imported.cost, Some(0.001));
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn retention_update_compacts_activity_and_settings_together() {
        let now = crate::auth::now();
        let directory = test_directory("retention-success");
        let activity_path = directory.join("activity.jsonl");
        let settings_path = directory.join("activity-settings.json");
        let transaction_path = directory.join("activity-transaction.json");
        let store = store(vec![
            request(now - 2 * 86_400, "expired", "alpha", 200),
            request(now, "retained", "alpha", 200),
        ]);

        store
            .set_retention_days_at(
                1,
                settings_path.to_str().unwrap(),
                activity_path.to_str().unwrap(),
                transaction_path.to_str().unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(store.retention_days(), 1);
        assert_eq!(store.export_records(0).await.len(), 1);
        let activity = tokio::fs::read_to_string(&activity_path).await.unwrap();
        assert!(activity.contains("retained"));
        assert!(!activity.contains("expired"));
        let settings: ActivitySettings =
            serde_json::from_slice(&tokio::fs::read(&settings_path).await.unwrap()).unwrap();
        assert_eq!(settings.retention_days, 1);
        assert!(!transaction_path.exists());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn failed_retention_transaction_does_not_publish_or_delete_records() {
        let now = crate::auth::now();
        let directory = test_directory("retention-failure");
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let activity_path = directory.join("activity.jsonl");
        let blocked_parent = directory.join("not-a-directory");
        let settings_path = blocked_parent.join("activity-settings.json");
        let transaction_path = directory.join("activity-transaction.json");
        tokio::fs::write(&activity_path, b"existing activity\n")
            .await
            .unwrap();
        tokio::fs::write(&blocked_parent, b"blocked").await.unwrap();
        let store = store(vec![
            request(now - 2 * 86_400, "would-expire", "alpha", 200),
            request(now, "would-remain", "alpha", 200),
        ]);

        let result = store
            .set_retention_days_at(
                1,
                settings_path.to_str().unwrap(),
                activity_path.to_str().unwrap(),
                transaction_path.to_str().unwrap(),
            )
            .await;

        assert!(result.is_err());
        assert_eq!(store.retention_days(), DEFAULT_RETENTION_DAYS);
        assert_eq!(store.export_records(0).await.len(), 2);
        assert_eq!(
            tokio::fs::read(&activity_path).await.unwrap(),
            b"existing activity\n"
        );
        assert!(!transaction_path.exists());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn paginated_logs_filter_before_counting_and_slicing() {
        let now = crate::auth::now();
        let store = store(vec![
            request(now - 4, "old-success", "alpha", 200),
            request(now - 3, "alpha-error", "alpha", 500),
            request(now - 2, "beta-success", "beta", 200),
            request(now - 1, "new-success", "alpha", 201),
        ]);

        let providers = vec!["alpha".to_owned()];
        let (logs, total) = store
            .query_logs(ActivityLogQuery {
                since: now - 10,
                until: now,
                filters: ActivityFilters {
                    providers: &providers,
                    ..ActivityFilters::default()
                },
                text: Some("success"),
                status: Some("success"),
                offset: 1,
                limit: 1,
            })
            .await;

        assert_eq!(total, 2);
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].request_id, "old-success");
    }

    #[tokio::test]
    async fn paginated_logs_search_the_upstream_model() {
        let now = crate::auth::now();
        let mut routed = request(now, "routed", "provider", 200);
        routed.model = "public-alias".to_owned();
        routed.upstream_model = Some("actual-model".to_owned());
        routed.gateway_api_key_id = Some("agent-key".to_owned());
        routed.gateway_api_key_note = Some("Production agent".to_owned());
        let store = store(vec![routed]);

        let (logs, total) = store
            .query_logs(ActivityLogQuery {
                since: now - 1,
                until: now,
                filters: ActivityFilters::default(),
                text: Some("actual-model"),
                status: None,
                offset: 0,
                limit: 10,
            })
            .await;

        assert_eq!(total, 1);
        assert_eq!(logs[0].model, "public-alias");

        let (logs, total) = store
            .query_logs(ActivityLogQuery {
                since: now - 1,
                until: now,
                filters: ActivityFilters::default(),
                text: Some("production agent"),
                status: None,
                offset: 0,
                limit: 10,
            })
            .await;

        assert_eq!(total, 1);
        assert_eq!(logs[0].gateway_api_key_id.as_deref(), Some("agent-key"));
    }

    #[tokio::test]
    async fn paginated_logs_respect_snapshot_upper_bound() {
        let now = crate::auth::now();
        let store = store(vec![
            request(now - 2, "snapshot", "alpha", 200),
            request(now + 1, "newer", "alpha", 200),
        ]);

        let (logs, total) = store
            .query_logs(ActivityLogQuery {
                since: now - 10,
                until: now,
                filters: ActivityFilters::default(),
                text: None,
                status: None,
                offset: 0,
                limit: 100,
            })
            .await;

        assert_eq!(total, 1);
        assert_eq!(logs[0].request_id, "snapshot");
    }

    #[tokio::test]
    async fn activity_filters_use_or_within_groups_and_and_across_groups() {
        let now = crate::auth::now();
        let mut alpha_key = request(now - 4, "alpha-key", "alpha", 200);
        alpha_key.model = "model-a".to_owned();
        alpha_key.gateway_api_key_id = Some("key-1".to_owned());
        alpha_key.gateway_api_key_note = Some("Production".to_owned());
        alpha_key.input_tokens = 100;
        alpha_key.output_tokens = 20;
        let mut alpha_unattributed = request(now - 3, "alpha-unattributed", "alpha", 200);
        alpha_unattributed.model = "model-a".to_owned();
        alpha_unattributed.input_tokens = 50;
        alpha_unattributed.output_tokens = 10;
        let mut alpha_other_model = request(now - 2, "alpha-other-model", "alpha", 200);
        alpha_other_model.model = "model-b".to_owned();
        alpha_other_model.gateway_api_key_id = Some("key-1".to_owned());
        let mut beta = request(now - 1, "beta", "beta", 200);
        beta.model = "model-a".to_owned();
        beta.gateway_api_key_id = Some("key-1".to_owned());
        let store = store(vec![alpha_key, alpha_unattributed, alpha_other_model, beta]);
        let providers = vec!["alpha".to_owned()];
        let models = vec!["model-a".to_owned()];
        let api_keys = vec!["key-1".to_owned(), UNATTRIBUTED_API_KEY_FILTER.to_owned()];
        let filters = ActivityFilters {
            providers: &providers,
            models: &models,
            api_keys: &api_keys,
        };

        let (logs, total) = store
            .query_logs(ActivityLogQuery {
                since: now - 10,
                until: now,
                filters,
                text: None,
                status: None,
                offset: 0,
                limit: 100,
            })
            .await;
        assert_eq!(total, 2);
        assert_eq!(logs.len(), 2);
        assert!(
            logs.iter()
                .all(|log| log.provider == "alpha" && log.model == "model-a")
        );

        let stats = store.stats(now - 10, filters, 2, now).await;

        assert_eq!(stats.requests, 2);
        assert_eq!(
            stats.buckets.iter().map(|bucket| bucket.input).sum::<u64>(),
            150
        );
        assert_eq!(
            stats
                .buckets
                .iter()
                .map(|bucket| bucket.output)
                .sum::<u64>(),
            30
        );
        assert_eq!(stats.filter_options.providers, vec!["alpha", "beta"]);
        assert_eq!(stats.filter_options.models, vec!["model-a", "model-b"]);
        assert!(
            stats
                .filter_options
                .api_keys
                .iter()
                .any(|key| key.id == "key-1")
        );
        assert!(
            stats
                .filter_options
                .api_keys
                .iter()
                .any(|key| key.id == UNATTRIBUTED_API_KEY_FILTER)
        );
    }

    #[tokio::test]
    async fn stats_distinguish_reported_estimated_and_missing_cost() {
        let now = crate::auth::now();
        let mut reported = request(now - 3, "reported", "alpha", 200);
        reported.cost = Some(0.0);
        reported.input_tokens = 100;
        reported.cached_tokens = 80;
        let mut estimated = request(now - 2, "estimated", "alpha", 200);
        estimated.cost = Some(0.25);
        estimated.cost_source = Some(CostSource::Estimated);
        let missing = request(now - 1, "missing", "alpha", 200);
        let stats = store(vec![reported, estimated, missing])
            .stats(now - 10, ActivityFilters::default(), 2, now)
            .await;

        assert_eq!(stats.requests, 3);
        assert_eq!(stats.priced_requests, 2);
        assert_eq!(stats.reported_requests, 1);
        assert_eq!(stats.estimated_requests, 1);
        for dimension in stats.by_provider.iter().chain(&stats.by_model).map(|item| {
            (
                item.priced_requests,
                item.reported_requests,
                item.estimated_requests,
            )
        }) {
            assert_eq!(dimension.0, dimension.1 + dimension.2);
        }
        assert_eq!(stats.by_api_key[0].priced_requests, 2);
        assert_eq!(stats.by_api_key[0].reported_requests, 1);
        assert_eq!(stats.by_api_key[0].estimated_requests, 1);
        assert_eq!(
            stats
                .buckets
                .iter()
                .map(|bucket| bucket.priced_requests)
                .sum::<usize>(),
            2
        );
        assert_eq!(
            stats
                .buckets
                .iter()
                .map(|bucket| bucket.reported_requests)
                .sum::<usize>(),
            1
        );
        assert_eq!(
            stats
                .buckets
                .iter()
                .map(|bucket| bucket.estimated_requests)
                .sum::<usize>(),
            1
        );
        assert_eq!(stats.cost, 0.25);
    }

    #[tokio::test]
    async fn stats_sum_costs_in_fixed_point_units() {
        let now = crate::auth::now();
        let mut first = request(now - 2, "first", "alpha", 200);
        first.cost = Some(0.1);
        let mut second = request(now - 1, "second", "alpha", 200);
        second.cost = Some(0.2);
        let stats = store(vec![first, second])
            .stats(now - 10, ActivityFilters::default(), 1, now)
            .await;

        assert_eq!(stats.cost, 0.3);
        assert_eq!(stats.by_provider[0].cost, 0.3);
        assert_eq!(stats.buckets[0].cost, 0.3);
    }

    #[tokio::test]
    async fn stats_include_request_buckets_for_each_provider() {
        let now = crate::auth::now();
        let stats = store(vec![
            request(now - 9, "alpha-early", "alpha", 200),
            request(now - 1, "alpha-late", "alpha", 200),
            request(now - 1, "beta-late", "beta", 200),
        ])
        .stats(now - 10, ActivityFilters::default(), 2, now)
        .await;

        assert_eq!(stats.provider_buckets.len(), 2);
        assert_eq!(stats.provider_buckets[0].name, "alpha");
        assert_eq!(stats.provider_buckets[0].requests, vec![1, 1]);
        assert_eq!(stats.provider_buckets[1].name, "beta");
        assert_eq!(stats.provider_buckets[1].requests, vec![0, 1]);
    }

    #[tokio::test]
    async fn stats_group_requests_by_safe_gateway_key_identity() {
        let now = crate::auth::now();
        let mut attributed = request(now - 2, "attributed", "alpha", 200);
        attributed.gateway_api_key_id = Some("key-1".to_owned());
        attributed.gateway_api_key_note = Some("Production agent".to_owned());
        attributed.gateway_api_key_prefix = Some("sk-…1234".to_owned());
        let legacy = request(now - 1, "legacy", "alpha", 500);

        let stats = store(vec![attributed, legacy])
            .stats(now - 10, ActivityFilters::default(), 2, now)
            .await;

        assert_eq!(stats.by_api_key.len(), 2);
        let key = stats
            .by_api_key
            .iter()
            .find(|item| item.id.as_deref() == Some("key-1"))
            .unwrap();
        assert_eq!(key.name, "Production agent");
        assert_eq!(key.prefix.as_deref(), Some("sk-…1234"));
        assert!(stats.by_api_key.iter().any(|item| item.id.is_none()));
    }
}

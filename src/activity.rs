use std::{
    collections::{BTreeMap, BTreeSet},
    io::ErrorKind,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::Mutex};
use tracing::{error, warn};

/// Activity lives in one JSON Lines file per UTC day. A normal flush only appends
/// to the day it belongs to, and a day that leaves the retention window is removed
/// as a whole file, so retaining history never rewrites the records that stay.
pub(crate) const ACTIVITY_DIRECTORY: &str = "data/activity/";
pub(crate) const ACTIVITY_SETTINGS_FILE: &str = "data/activity-settings.json";
pub(crate) const ACTIVITY_TRANSACTION_FILE: &str = "data/activity-transaction.json";

/// Files an Activity transaction may replace. Tests point the same logic at a
/// temporary directory instead of the process working directory.
#[derive(Clone, Copy)]
pub(crate) struct ActivityPaths<'a> {
    pub directory: &'a str,
    pub settings: &'a str,
    pub transaction: &'a str,
}

const ACTIVITY_PATHS: ActivityPaths<'static> = ActivityPaths {
    directory: ACTIVITY_DIRECTORY,
    settings: ACTIVITY_SETTINGS_FILE,
    transaction: ACTIVITY_TRANSACTION_FILE,
};
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
    /// Stable ID of the identity the request left with, when its Endpoint uses
    /// one. The credential itself is never recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_credential_id: Option<String>,
    /// Name that identity had when it carried the request, so the record stays
    /// readable after the credential is deleted or when it is read on an
    /// instance that does not configure it. Display metadata only: the
    /// credential and the account behind it are never recorded. Older imported
    /// or retained records may not contain this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_credential_name: Option<String>,
    /// True when the identity that carried the request was itself cooling down,
    /// which happens when the Endpoint had no eligible identity left or when a
    /// model route pinned an exhausted one, so a `429` here is explainable
    /// instead of surprising. Older imported or retained records read as false.
    #[serde(default)]
    pub credential_cooling: bool,
    /// Selection mode of the model route that chose this destination: `weighted`
    /// for a route that splits by configured share, `failover` for one that uses
    /// priority groups. A request that named its Provider directly, and every
    /// record written before routes carried modes, has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_mode: Option<String>,
    /// Priority group the chosen destination belongs to. Only a failover route
    /// stores one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_priority: Option<u32>,
    /// True when the destination was chosen while a lower-numbered group could
    /// not serve because every destination in it was cooling down. Older
    /// imported or retained records read as false.
    #[serde(default)]
    pub route_failover: bool,
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
    /// Which configured rules supplied the estimated rates, so an estimate stays
    /// explainable after pricing changes. Only written for estimated costs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_sources: Option<crate::pricing::PricingSources>,
    /// Upstream protocol's terminal reason or status. Older records may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    pub streaming: bool,
    /// Whether the Provider response itself streamed. Only a streamed response
    /// has a generation phase to time, so throughput and first-token latency are
    /// aggregated from these records instead of being inferred from the caller's
    /// streaming preference.
    #[serde(default)]
    pub upstream_streaming: bool,
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
    Available(Box<crate::pricing::ResolvedPricing>),
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
    /// Records a running flush has taken out of `pending` but has not appended
    /// yet. They stay readable here so a query never observes a gap between
    /// "no longer pending" and "not yet persisted".
    flushing: Vec<RequestLog>,
    pending: Vec<RequestLog>,
    /// UTC days present in `persisted`, so a periodic flush decides which day
    /// files expired without scanning the retained history.
    days: BTreeSet<i64>,
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

#[derive(Debug)]
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

/// Which of a request's two model names the Activity model analysis groups by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelDimension {
    /// The model name the caller sent, including a public route alias.
    #[default]
    Incoming,
    /// The model ID Yabane sent to the Provider after routing.
    Outgoing,
}

fn model_dimension_key(dimension: ModelDimension) -> fn(&RequestLog) -> &str {
    match dimension {
        ModelDimension::Incoming => |log| &log.model,
        // Legacy records without a recorded Provider model remain visible as one unnamed group.
        ModelDimension::Outgoing => |log| log.upstream_model.as_deref().unwrap_or_default(),
    }
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
    /// Sum of first-byte latency over the samples that measured it and how many
    /// those were; only responses the Provider streamed have a first byte.
    pub first_byte: u64,
    pub first_byte_samples: usize,
    /// Sum of generation time, of the output tokens produced during it, and the
    /// matching sample count, so throughput stays a measured ratio instead of an
    /// average of per-request rates.
    pub generation: u64,
    pub generation_tokens: u64,
    pub generation_samples: usize,
    pub successful: usize,
    pub errors: usize,
}

impl ActivityStore {
    pub async fn load() -> Result<Self, String> {
        let retention_days = load_retention_days().await?;
        let cutoff = retention_cutoff(retention_days);
        let persisted = load_day_files(ACTIVITY_DIRECTORY, cutoff).await?;
        Ok(Self {
            inner: Arc::new(Mutex::new(ActivityData {
                days: persisted.iter().map(|log| day_of(log.timestamp)).collect(),
                persisted,
                flushing: Vec::new(),
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
            .chain(&data.flushing)
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
            .chain(&data.flushing)
            .chain(&data.pending)
            .filter(matches)
            .count();
        let logs = data
            .persisted
            .iter()
            .chain(&data.flushing)
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
            .chain(&data.flushing)
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
        self.set_retention_days_at(days, ACTIVITY_PATHS).await
    }

    async fn set_retention_days_at(
        &self,
        days: u64,
        paths: ActivityPaths<'_>,
    ) -> Result<(), String> {
        if !(1..=3650).contains(&days) {
            return Err("Activity retention must be between 1 and 3650 days".to_owned());
        }
        let _flush_guard = self.flush_lock.lock().await;
        let cutoff = retention_cutoff(days);
        // The persisted setting is the policy. Removing the day files it excludes is
        // cleanup that the next flush can retry, so neither step can leave a
        // half-published retention window behind.
        crate::storage::write_json_atomic(
            paths.settings,
            &ActivitySettings {
                retention_days: days,
            },
        )
        .await
        .map_err(|err| format!("save activity retention: {err}"))?;
        let cutoff_day = day_of(cutoff);
        let remaining = match prune_day_files(paths.directory, cutoff_day).await {
            Ok(remaining) => remaining,
            Err(err) => {
                warn!(%err, "failed to prune expired activity");
                self.retention_days.store(days, Ordering::Relaxed);
                return Ok(());
            }
        };
        let mut data = self.inner.lock().await;
        data.days.retain(|day| remaining.contains(day));
        data.persisted
            .retain(|log| remaining.contains(&day_of(log.timestamp)));
        data.flushing
            .retain(|log| day_of(log.timestamp) >= cutoff_day);
        data.pending
            .retain(|log| day_of(log.timestamp) >= cutoff_day);
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
        self.recalculate_non_reported_costs_at(ACTIVITY_PATHS, record, pricing_for)
            .await
    }

    async fn recalculate_non_reported_costs_at<F>(
        &self,
        paths: ActivityPaths<'_>,
        record: Option<(&str, Option<&str>)>,
        pricing_for: F,
    ) -> Result<CostRecalculationResult, std::io::Error>
    where
        F: Fn(&RequestLog) -> CostRecalculationResolution,
    {
        let _flush_guard = self.flush_lock.lock().await;
        // The snapshot is prepared under the in-memory lock, but the day files are
        // rewritten without it, so a long recalculation cannot stall request
        // recording or console queries. Only `record` can add to `pending` while the
        // write runs, and `included` remembers which in-flight records the snapshot
        // already carries so publishing cannot drop a newer one.
        let (records, result, updated_days, included) = {
            let data = self.inner.lock().await;
            let mut records: Vec<_> = data
                .persisted
                .iter()
                .chain(&data.flushing)
                .chain(&data.pending)
                .cloned()
                .collect();
            let included: std::collections::HashSet<(String, Option<String>)> = data
                .flushing
                .iter()
                .chain(&data.pending)
                .map(record_identity)
                .collect();
            let mut result = CostRecalculationResult::default();
            let mut updated_days = BTreeSet::new();
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
                let resolved = match resolution {
                    CostRecalculationResolution::Available(resolved) => *resolved,
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
                let Some(cost) = crate::pricing::calculate(&resolved.pricing, &usage) else {
                    result.skipped_missing_pricing += 1;
                    continue;
                };
                let recalculated = log.cost.is_some();
                log.cost = Some(cost);
                log.cost_source = Some(CostSource::Estimated);
                log.pricing_sources = Some(resolved.sources);
                updated_days.insert(day_of(log.timestamp));
                result.updated += 1;
                if recalculated {
                    result.recalculated += 1;
                } else {
                    result.filled += 1;
                }
            }
            // Every day a pending record belongs to is rewritten, because the
            // snapshot takes those records to disk as part of this update.
            updated_days.extend(data.flushing.iter().map(|log| day_of(log.timestamp)));
            updated_days.extend(data.pending.iter().map(|log| day_of(log.timestamp)));
            (records, result, updated_days, included)
        };
        if result.updated == 0 {
            return Ok(result);
        }
        let writes = day_writes(paths.directory, &records, &updated_days)?;
        crate::storage::write_transaction(paths.transaction, &writes).await?;
        let mut data = self.inner.lock().await;
        data.days = records.iter().map(|log| day_of(log.timestamp)).collect();
        data.persisted = records;
        data.flushing
            .retain(|log| !included.contains(&record_identity(log)));
        data.pending
            .retain(|log| !included.contains(&record_identity(log)));
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
        self.import_at(ACTIVITY_PATHS, import).await
    }

    async fn import_at(
        &self,
        paths: ActivityPaths<'_>,
        import: ActivityImport,
    ) -> Result<ImportResult, ActivityImportError> {
        self.validate_import(&import)
            .map_err(ActivityImportError::Invalid)?;
        let _flush_guard = self.flush_lock.lock().await;
        // The merged snapshot is prepared under the in-memory lock, but the day
        // files are replaced without it, so a large import cannot stall request
        // recording or console queries. Only `record` can add to `pending` while
        // the write runs, and `included` keeps those newer records pending.
        let (result, records, days, included) = {
            let data = self.inner.lock().await;
            let (result, imported) = self.classify_import(&import, &data);
            if imported.is_empty() {
                return Ok(result);
            }
            // Memory already mirrors the day files exactly, so an import only has to
            // merge the accepted records into the days it touches.
            let mut records: Vec<_> = data
                .persisted
                .iter()
                .chain(&data.flushing)
                .chain(&data.pending)
                .cloned()
                .collect();
            let included: std::collections::HashSet<(String, Option<String>)> = data
                .flushing
                .iter()
                .chain(&data.pending)
                .map(record_identity)
                .collect();
            // Imported records join the day files they belong to, and every in-flight
            // record shares that write so publishing cannot lose anything.
            let mut days: BTreeSet<_> = imported.iter().map(|log| day_of(log.timestamp)).collect();
            days.extend(data.flushing.iter().map(|log| day_of(log.timestamp)));
            days.extend(data.pending.iter().map(|log| day_of(log.timestamp)));
            records.extend(imported);
            records.sort_by(|a, b| {
                a.timestamp
                    .cmp(&b.timestamp)
                    .then_with(|| a.request_id.cmp(&b.request_id))
            });
            (result, records, days, included)
        };
        let writes =
            day_writes(paths.directory, &records, &days).map_err(ActivityImportError::Persist)?;
        crate::storage::write_transaction(paths.transaction, &writes)
            .await
            .map_err(ActivityImportError::Persist)?;
        let mut data = self.inner.lock().await;
        data.days = records.iter().map(|log| day_of(log.timestamp)).collect();
        data.persisted = records;
        data.flushing
            .retain(|log| !included.contains(&record_identity(log)));
        data.pending
            .retain(|log| !included.contains(&record_identity(log)));
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
            .chain(&data.flushing)
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
        model_dimension: ModelDimension,
    ) -> Stats {
        let since = since.max(retention_cutoff(self.retention_days()));
        let data = self.inner.lock().await;
        let range_logs: Vec<_> = data
            .persisted
            .iter()
            .chain(&data.flushing)
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
                first_byte: 0,
                first_byte_samples: 0,
                generation: 0,
                generation_tokens: 0,
                generation_samples: 0,
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
                // Generation quality is only meaningful for a response the Provider
                // streamed and completed, so a stalled or failed stream cannot dilute
                // the measured throughput. Failures stay visible in the error rate.
                if log.upstream_streaming && (200..300).contains(&log.status) {
                    if let Some(first_byte_ms) = log.first_byte_ms {
                        bucket.first_byte = bucket.first_byte.saturating_add(first_byte_ms);
                        bucket.first_byte_samples += 1;
                    }
                    if let Some(generation_ms) =
                        log.generation_ms.filter(|generation| *generation > 0)
                    {
                        bucket.generation = bucket.generation.saturating_add(generation_ms);
                        bucket.generation_tokens =
                            bucket.generation_tokens.saturating_add(log.output_tokens);
                        bucket.generation_samples += 1;
                    }
                }
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
            by_model: dimensions(model_dimension_key(model_dimension)),
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
        self.flush_at(ACTIVITY_PATHS).await;
    }

    async fn flush_at(&self, paths: ActivityPaths<'_>) {
        let _flush_guard = self.flush_lock.lock().await;
        // An idle periodic flush only reads memory: nothing is written unless records
        // are pending or a whole day has left the retention window.
        let (batch, expired_days) = {
            let mut data = self.inner.lock().await;
            let cutoff_day = day_of(retention_cutoff(self.retention_days()));
            data.pending
                .retain(|log| day_of(log.timestamp) >= cutoff_day);
            let expired_days: BTreeSet<_> = data.days.range(..cutoff_day).copied().collect();
            if data.pending.is_empty() && expired_days.is_empty() {
                return;
            }
            // The batch stays visible in `flushing` while it is appended, so a query
            // issued during the write sees the same records as before it.
            let batch = std::mem::take(&mut data.pending);
            data.flushing = batch.clone();
            (batch, expired_days)
        };

        // A day outside the window leaves as a whole file, and its records leave
        // memory with it. This is what keeps retention from rewriting what stays.
        let mut removed_days = BTreeSet::new();
        for day in &expired_days {
            let path = day_path(paths.directory, *day);
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {
                    removed_days.insert(*day);
                }
                Err(err) if err.kind() == ErrorKind::NotFound => {
                    removed_days.insert(*day);
                }
                Err(err) => error!(%err, %path, "failed to remove expired activity"),
            }
        }

        // Pending records are appended to their own day file. A batch that spans a
        // day boundary is still a pair of appends instead of a rewrite.
        let mut appended = Vec::new();
        let mut appended_days = Vec::new();
        for (day, records) in group_by_day(batch) {
            match append_day_records(paths.directory, day, &records).await {
                Ok(()) => {
                    appended_days.push(day);
                    appended.extend(records);
                }
                Err(err) => {
                    error!(%err, day = %day_file_name(day), "failed to flush request activity");
                }
            }
        }

        let mut data = self.inner.lock().await;
        data.days.retain(|day| !removed_days.contains(day));
        data.days.extend(appended_days);
        data.persisted
            .retain(|log| !removed_days.contains(&day_of(log.timestamp)));
        let appended_keys: std::collections::HashSet<(String, Option<String>)> =
            appended.iter().map(record_identity).collect();
        data.persisted.extend(appended);
        // What failed the append returns to `pending` ahead of records that arrived
        // while the write ran, so retrying keeps their original order and the batch
        // is never both persisted and pending.
        data.flushing
            .retain(|log| !appended_keys.contains(&record_identity(log)));
        let failed = std::mem::take(&mut data.flushing);
        let newer = std::mem::take(&mut data.pending);
        data.pending = failed;
        data.pending.extend(newer);
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

/// Test-only fault injection. Like the storage hold, injected appends are
/// registered per path prefix under one lock, so tests that run in parallel
/// cannot consume or observe each other's injection.
#[cfg(test)]
#[derive(Clone)]
struct AppendFaults {
    prefix: String,
    partial_bytes: usize,
    hold_ms: u64,
    reached: bool,
}

#[cfg(test)]
static APPEND_FAULTS: std::sync::Mutex<Vec<AppendFaults>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn append_faults() -> std::sync::MutexGuard<'static, Vec<AppendFaults>> {
    APPEND_FAULTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
pub(crate) fn arm_append_faults(prefix: &str, partial_bytes: usize, hold_ms: u64) {
    let mut faults = append_faults();
    faults.retain(|fault| fault.prefix != prefix);
    faults.push(AppendFaults {
        prefix: prefix.to_owned(),
        partial_bytes,
        hold_ms,
        reached: false,
    });
}

/// Whether the hold armed for this prefix was reached. It stays true until the
/// prefix is armed again.
#[cfg(test)]
pub(crate) fn append_hold_reached(prefix: &str) -> bool {
    append_faults()
        .iter()
        .any(|fault| fault.prefix == prefix && fault.reached)
}

#[cfg(test)]
fn take_append_hold(path: &Path) -> Option<u64> {
    let mut faults = append_faults();
    let fault = faults
        .iter_mut()
        .find(|fault| path.to_string_lossy().starts_with(&fault.prefix))?;
    if fault.hold_ms == 0 {
        return None;
    }
    fault.reached = true;
    Some(std::mem::take(&mut fault.hold_ms))
}

#[cfg(test)]
fn take_partial_append(path: &Path) -> usize {
    append_faults()
        .iter_mut()
        .find(|fault| path.to_string_lossy().starts_with(&fault.prefix))
        .map_or(0, |fault| std::mem::take(&mut fault.partial_bytes))
}

#[cfg(not(test))]
fn take_append_hold(_path: &Path) -> Option<u64> {
    None
}

#[cfg(not(test))]
fn take_partial_append(_path: &Path) -> usize {
    0
}

async fn append_day_records(
    directory: &str,
    day: i64,
    records: &[RequestLog],
) -> Result<(), std::io::Error> {
    if records.is_empty() {
        return Ok(());
    }
    tokio::fs::create_dir_all(directory).await?;
    let contents = serialize_logs(records);
    append_bytes(Path::new(&day_path(directory, day)), &contents).await
}

/// Appends one batch and makes it durable. A failed append is truncated back to
/// its previous length, because retrying a partial write would otherwise
/// duplicate the prefix that already reached the file.
async fn append_bytes(path: &Path, contents: &[u8]) -> Result<(), std::io::Error> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    let previous_len = file.metadata().await?.len();
    if let Some(hold_ms) = take_append_hold(path) {
        tokio::time::sleep(Duration::from_millis(hold_ms)).await;
    }
    let partial = take_partial_append(path);
    let result = async {
        write_batch(&mut file, contents, partial).await?;
        file.sync_data().await
    }
    .await;
    if let Err(error) = result {
        if let Err(rollback) = rollback_append(&mut file, previous_len).await {
            return Err(std::io::Error::new(
                rollback.kind(),
                format!("{error}; could not truncate the partial append: {rollback}"),
            ));
        }
        return Err(error);
    }
    Ok(())
}

async fn rollback_append(
    file: &mut tokio::fs::File,
    previous_len: u64,
) -> Result<(), std::io::Error> {
    // Flush first so the truncation also discards whatever the failed write buffered.
    file.flush().await?;
    file.set_len(previous_len).await?;
    file.sync_data().await
}

#[cfg(not(test))]
async fn write_batch(
    file: &mut tokio::fs::File,
    contents: &[u8],
    _partial: usize,
) -> Result<(), std::io::Error> {
    file.write_all(contents).await
}

/// Writes only `partial` bytes when a test armed a partial append, which is the
/// shape of a disk-full or interrupted write.
#[cfg(test)]
async fn write_batch(
    file: &mut tokio::fs::File,
    contents: &[u8],
    partial: usize,
) -> Result<(), std::io::Error> {
    if partial == 0 || partial >= contents.len() {
        return file.write_all(contents).await;
    }
    file.write_all(&contents[..partial]).await?;
    Err(std::io::Error::other("injected partial append"))
}

fn record_identity(log: &RequestLog) -> (String, Option<String>) {
    (log.request_id.clone(), log.source_instance_id.clone())
}

fn group_by_day(records: Vec<RequestLog>) -> Vec<(i64, Vec<RequestLog>)> {
    let mut days = BTreeMap::<i64, Vec<RequestLog>>::new();
    for record in records {
        days.entry(day_of(record.timestamp))
            .or_default()
            .push(record);
    }
    days.into_iter().collect()
}

fn day_writes(
    directory: &str,
    records: &[RequestLog],
    days: &BTreeSet<i64>,
) -> std::io::Result<Vec<crate::storage::AtomicWrite>> {
    days.iter()
        .map(|day| {
            let selected: Vec<_> = records
                .iter()
                .filter(|log| day_of(log.timestamp) == *day)
                .cloned()
                .collect();
            crate::storage::AtomicWrite::bytes(day_path(directory, *day), serialize_logs(&selected))
        })
        .collect()
}

async fn load_day_files(directory: &str, cutoff: u64) -> Result<Vec<RequestLog>, String> {
    let cutoff_day = day_of(cutoff);
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("read {directory}: {err}")),
    };
    let mut retained = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|err| format!("read {directory}: {err}"))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(day) = parse_day_file_name(&name) else {
            continue;
        };
        let path = day_path(directory, day);
        if day < cutoff_day {
            if let Err(err) = tokio::fs::remove_file(&path).await
                && err.kind() != ErrorKind::NotFound
            {
                warn!(%err, %path, "failed to remove expired activity");
            }
            continue;
        }
        retained.push((day, path));
    }
    retained.sort_by_key(|(day, _)| *day);
    let mut persisted = Vec::new();
    for (_, path) in retained {
        let contents = tokio::fs::read(&path)
            .await
            .map_err(|err| format!("read {path}: {err}"))?;
        persisted.extend(read_day_records(&path, &contents).await?);
    }
    Ok(persisted)
}

/// Reads one day file. A JSON Lines file whose final record is cut short is what
/// a crash during an append produces: the complete prefix stays, the fragment
/// moves to a `.truncated-*` sibling, and startup continues with the records that
/// were whole. Damage anywhere else is not a crash artifact, so loading fails
/// with the exact file and line instead of guessing at a repair.
async fn read_day_records(path: &str, contents: &[u8]) -> Result<Vec<RequestLog>, String> {
    let complete = contents.is_empty() || contents.ends_with(b"\n");
    let lines: Vec<&[u8]> = contents
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    let mut records = Vec::with_capacity(lines.len());
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        match serde_json::from_slice::<RequestLog>(line) {
            Ok(record) => records.push(record),
            Err(error) if index + 1 == lines.len() && !complete => {
                let fragment_start = contents.len() - raw.len();
                let quarantine = format!("{path}.truncated-{}", crate::auth::now());
                crate::storage::write_atomic(&quarantine, raw)
                    .await
                    .map_err(|err| format!("quarantine {path}: {err}"))?;
                let mut options = OpenOptions::new();
                options.write(true);
                let file = options
                    .open(path)
                    .await
                    .map_err(|err| format!("repair {path}: {err}"))?;
                file.set_len(fragment_start as u64)
                    .await
                    .map_err(|err| format!("repair {path}: {err}"))?;
                file.sync_data()
                    .await
                    .map_err(|err| format!("repair {path}: {err}"))?;
                error!(
                    %path,
                    %quarantine,
                    %error,
                    bytes = raw.len(),
                    "quarantined a truncated Activity record from an interrupted append"
                );
                return Ok(records);
            }
            Err(error) => {
                return Err(format!("parse {path} line {}: {error}", index + 1));
            }
        }
    }
    Ok(records)
}

/// Removes the day files outside the window and reports which days remain, so
/// memory can keep exactly what is still on disk.
async fn prune_day_files(directory: &str, cutoff_day: i64) -> Result<BTreeSet<i64>, String> {
    let mut remaining = BTreeSet::new();
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(remaining),
        Err(err) => return Err(format!("read {directory}: {err}")),
    };
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|err| format!("read {directory}: {err}"))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(day) = parse_day_file_name(&name) else {
            continue;
        };
        if day >= cutoff_day {
            remaining.insert(day);
            continue;
        }
        let path = day_path(directory, day);
        if let Err(err) = tokio::fs::remove_file(&path).await
            && err.kind() != ErrorKind::NotFound
        {
            warn!(%err, %path, "failed to remove expired activity");
            remaining.insert(day);
        }
    }
    Ok(remaining)
}

/// Activity day files are named by UTC calendar day, which keeps the retention
/// window a whole-file decision and keeps file names sorted chronologically.
fn day_of(timestamp: u64) -> i64 {
    (timestamp / 86_400) as i64
}

fn day_file_name(day: i64) -> String {
    let (year, month, day_of_month) = civil_from_days(day);
    format!("{year:04}-{month:02}-{day_of_month:02}.jsonl")
}

fn day_path(directory: &str, day: i64) -> String {
    format!("{directory}{}", day_file_name(day))
}

fn parse_day_file_name(name: &str) -> Option<i64> {
    let date = name.strip_suffix(".jsonl")?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day_of_month: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day_of_month) {
        return None;
    }
    let day = days_from_civil(year, month, day_of_month);
    (civil_from_days(day) == (year, month, day_of_month)).then_some(day)
}

/// Howard Hinnant's civil calendar algorithms keep whole UTC days free of a date
/// dependency: Activity only ever needs to know which day a second belongs to.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_position + 2) / 5 + 1;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let month_position = if month > 2 {
        (month - 3) as i64
    } else {
        (month + 9) as i64
    };
    let day_of_year = (153 * month_position + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
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
            upstream_credential_id: None,
            upstream_credential_name: None,
            credential_cooling: false,
            route_mode: None,
            route_priority: None,
            route_failover: false,
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
            pricing_sources: None,
            finish_reason: None,
            streaming: false,
            upstream_streaming: false,
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
        assert_eq!(decoded.pricing_sources, None);
    }

    #[test]
    fn structured_failure_round_trips_without_losing_categories() {
        let mut log = request(1, "structured", "provider", 429);
        log.failure = Some(RequestFailure::new(
            "upstream_response",
            "rate_limited",
            "Provider returned HTTP 429",
        ));
        let encoded = serde_json::to_vec(&log).unwrap();
        let decoded: RequestLog = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.failure, log.failure);
    }

    fn store(logs: Vec<RequestLog>) -> ActivityStore {
        ActivityStore {
            inner: Arc::new(Mutex::new(ActivityData {
                days: logs.iter().map(|log| day_of(log.timestamp)).collect(),
                persisted: logs,
                flushing: Vec::new(),
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

    /// Points the storage logic at a temporary directory instead of `data/`.
    #[derive(Clone)]
    struct TestPaths {
        directory: String,
        settings: String,
        transaction: String,
    }

    impl TestPaths {
        fn new(directory: &std::path::Path) -> Self {
            let directory = format!("{}/", directory.display());
            Self {
                settings: format!("{directory}activity-settings.json"),
                transaction: format!("{directory}activity-transaction.json"),
                directory,
            }
        }

        fn as_paths(&self) -> ActivityPaths<'_> {
            ActivityPaths {
                directory: &self.directory,
                settings: &self.settings,
                transaction: &self.transaction,
            }
        }

        fn day_file(&self, timestamp: u64) -> String {
            day_path(&self.directory, day_of(timestamp))
        }
    }

    #[test]
    fn day_file_names_follow_utc_calendar_days() {
        assert_eq!(day_file_name(day_of(0)), "1970-01-01.jsonl");
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        for day in [-1, 0, 1, 19_782, 20_000, 30_000] {
            let (year, month, day_of_month) = civil_from_days(day);
            assert_eq!(days_from_civil(year, month, day_of_month), day);
            assert_eq!(
                parse_day_file_name(&day_file_name(day)),
                Some(day),
                "{year:04}-{month:02}-{day_of_month:02} must round-trip"
            );
        }
        for invalid in [
            "activity.jsonl",
            "2024-02-30.jsonl",
            "2026-13-01.jsonl",
            "2024-02.jsonl",
            "2024-02-29.txt",
            "notes",
        ] {
            assert_eq!(parse_day_file_name(invalid), None, "{invalid}");
        }
    }

    /// Fault injection in `append_bytes` writes only a prefix and fails, which is
    /// the shape of a disk-full or interrupted append. The retry must not leave a
    /// duplicated or unparsable prefix behind, and the batch must stay queryable.
    #[tokio::test]
    async fn a_failed_append_leaves_no_partial_record_and_the_batch_stays_visible() {
        let now = crate::auth::now();
        let directory = test_directory("append-rollback");
        let paths = TestPaths::new(&directory);
        let store = store(Vec::new());
        store.record(request(now, "first", "alpha", 200)).await;

        arm_append_faults(directory.to_str().unwrap(), 64, 0);
        store.flush_at(paths.as_paths()).await;

        let today = paths.day_file(now);
        let partial = tokio::fs::read_to_string(&today).await.unwrap();
        assert!(
            partial.is_empty(),
            "a failed append must not leave a fragment: {partial:?}"
        );
        assert_eq!(
            store.export_records(0).await.len(),
            1,
            "the record stays queryable after a failed append"
        );
        assert_eq!(store.inner.lock().await.pending.len(), 1);

        store.flush_at(paths.as_paths()).await;
        let contents = tokio::fs::read_to_string(&today).await.unwrap();
        assert_eq!(contents.lines().count(), 1);
        assert!(contents.ends_with('\n'));
        assert_eq!(store.export_records(0).await.len(), 1);
        assert!(store.inner.lock().await.pending.is_empty());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    /// A record being appended must stay visible, so a console query issued during
    /// a flush never observes a gap between `pending` and `persisted`.
    #[tokio::test]
    async fn records_stay_queryable_while_a_flush_is_in_flight() {
        let now = crate::auth::now();
        let directory = test_directory("flush-visibility");
        let paths = TestPaths::new(&directory);
        let store = store(Vec::new());
        store.record(request(now, "inflight", "alpha", 200)).await;

        arm_append_faults(directory.to_str().unwrap(), 0, 1_000);
        let flushing = {
            let store = store.clone();
            let paths = paths.clone();
            tokio::spawn(async move { store.flush_at(paths.as_paths()).await })
        };
        while !append_hold_reached(directory.to_str().unwrap()) {
            tokio::task::yield_now().await;
        }

        let visible = store
            .logs(0, u64::MAX, ActivityFilters::default(), 10)
            .await;
        assert_eq!(
            visible.len(),
            1,
            "the record being appended is still visible"
        );
        assert_eq!(visible[0].request_id, "inflight");
        flushing.await.unwrap();

        // After the append publishes it, the same record is visible exactly once.
        let after = store.export_records(0).await;
        assert_eq!(after.len(), 1);
        let contents = tokio::fs::read_to_string(&paths.day_file(now))
            .await
            .unwrap();
        assert_eq!(contents.lines().count(), 1);
        assert!(store.inner.lock().await.flushing.is_empty());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    /// A crash during an append leaves a cut-off final record: the complete prefix
    /// is kept, the fragment is quarantined, and loading continues instead of
    /// failing the whole process.
    #[tokio::test]
    async fn a_truncated_tail_is_quarantined_and_the_complete_prefix_is_kept() {
        let now = crate::auth::now();
        let directory = test_directory("tail-quarantine");
        let directory_path = format!("{}/", directory.display());
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let path = day_path(&directory_path, day_of(now));
        let prefix = serialize_logs(&[request(now, "kept", "alpha", 200)]);
        let fragment = br#"{"timestamp":1,"request_id":"interrupted","path":"/v1/responses","model":"alpha/x","provider":"alpha","endpoint":"e","status":200,"latency_ms":1,"input_tokens":0,"output_tokens":0,"cached_tokens":0,"streaming":fal"#;
        let mut truncated = prefix.clone();
        truncated.extend_from_slice(fragment);
        tokio::fs::write(&path, &truncated).await.unwrap();

        let persisted = load_day_files(&directory_path, now - 30 * 86_400)
            .await
            .expect("a truncated tail is repaired instead of failing startup");
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].request_id, "kept");
        assert_eq!(tokio::fs::read(&path).await.unwrap(), prefix);

        let quarantined: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|entry| entry.to_string_lossy().contains(".truncated-"))
            .collect();
        assert_eq!(
            quarantined.len(),
            1,
            "the fragment is preserved for inspection"
        );
        assert_eq!(std::fs::read(&quarantined[0]).unwrap(), fragment);
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    /// Damage that no append can produce must fail loudly with the file and the
    /// line, because treating it as a truncated tail would silently drop history.
    #[tokio::test]
    async fn mid_file_corruption_fails_the_load_with_the_file_and_line() {
        let now = crate::auth::now();
        let directory = test_directory("mid-file-corruption");
        let directory_path = format!("{}/", directory.display());
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let path = day_path(&directory_path, day_of(now));
        let mut contents = serialize_logs(&[request(now, "first", "alpha", 200)]);
        contents.extend_from_slice(b"{not json}\n");
        contents.extend_from_slice(&serialize_logs(&[request(now, "second", "alpha", 200)]));
        tokio::fs::write(&path, &contents).await.unwrap();

        let error = load_day_files(&directory_path, now - 30 * 86_400)
            .await
            .expect_err("mid-file corruption must not be repaired silently");
        assert!(
            error.contains(&path),
            "the failing file must be named: {error}"
        );
        assert!(
            error.contains("line 2"),
            "the failing line must be named: {error}"
        );
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    /// A malformed line that the writer did finish (the file ends with a newline)
    /// is corruption, not a cut-off append.
    #[tokio::test]
    async fn a_malformed_complete_last_line_is_not_treated_as_a_truncated_tail() {
        let now = crate::auth::now();
        let directory = test_directory("complete-corruption");
        let directory_path = format!("{}/", directory.display());
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let path = day_path(&directory_path, day_of(now));
        let mut contents = serialize_logs(&[request(now, "first", "alpha", 200)]);
        contents.extend_from_slice(b"{not json}\n");
        tokio::fs::write(&path, &contents).await.unwrap();

        let error = load_day_files(&directory_path, now - 30 * 86_400)
            .await
            .expect_err("a completed malformed line must fail the load");
        assert!(error.contains("line 2"), "{error}");
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn flush_appends_without_rewriting_a_retained_day_file() {
        let now = crate::auth::now();
        let directory = test_directory("flush-append");
        let paths = TestPaths::new(&directory);
        let store = store(Vec::new());

        store.record(request(now, "first", "alpha", 200)).await;
        store.flush_at(paths.as_paths()).await;
        let today = paths.day_file(now);
        let first = tokio::fs::read_to_string(&today).await.unwrap();
        assert!(first.contains("\"first\""));

        store.record(request(now + 1, "second", "alpha", 200)).await;
        store.flush_at(paths.as_paths()).await;
        let second = tokio::fs::read_to_string(&today).await.unwrap();
        assert!(
            second.starts_with(&first),
            "the retained prefix must stay byte for byte"
        );
        assert!(second.contains("\"second\""));
        assert_eq!(second.lines().count(), 2);
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn flush_removes_a_day_file_once_it_leaves_the_retention_window() {
        let now = crate::auth::now();
        let directory = test_directory("flush-expired");
        let paths = TestPaths::new(&directory);
        let expired_at = now - 31 * 86_400;
        let retained_at = now - 10 * 86_400;
        let store = store(vec![
            request(expired_at, "expired", "alpha", 200),
            request(retained_at, "retained", "alpha", 200),
        ]);
        tokio::fs::create_dir_all(&paths.directory).await.unwrap();
        let expired = paths.day_file(expired_at);
        let retained = paths.day_file(retained_at);
        tokio::fs::write(
            &expired,
            serialize_logs(&[request(expired_at, "expired", "alpha", 200)]),
        )
        .await
        .unwrap();
        tokio::fs::write(
            &retained,
            serialize_logs(&[request(retained_at, "retained", "alpha", 200)]),
        )
        .await
        .unwrap();
        let retained_before = tokio::fs::read(&retained).await.unwrap();

        store.record(request(now, "current", "alpha", 200)).await;
        store.flush_at(paths.as_paths()).await;

        assert!(!std::path::Path::new(&expired).exists());
        assert!(std::path::Path::new(&paths.day_file(now)).exists());
        // Removing an expired day rewrites nothing: the retained history is the
        // same bytes it was, which is the whole point of day files.
        assert_eq!(tokio::fs::read(&retained).await.unwrap(), retained_before);
        let records = store.export_records(0).await;
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|record| record.request_id == "retained"));
        assert!(records.iter().any(|record| record.request_id == "current"));
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn flush_keeps_a_day_file_that_is_only_partially_expired() {
        let now = crate::auth::now();
        let directory = test_directory("flush-partial");
        let paths = TestPaths::new(&directory);
        let cutoff = now - 30 * 86_400;
        // The retention boundary falls inside a UTC day, so that day file still
        // carries a retained record and must stay whole instead of being trimmed.
        let day_start = day_of(cutoff) as u64 * 86_400;
        let expired_at = day_start;
        let retained_at = day_start + 86_400 - 1;
        let store = store(vec![
            request(expired_at, "expired", "alpha", 200),
            request(retained_at, "kept", "alpha", 200),
        ]);
        tokio::fs::create_dir_all(&paths.directory).await.unwrap();
        let file = paths.day_file(retained_at);
        tokio::fs::write(
            &file,
            serialize_logs(&[
                request(expired_at, "expired", "alpha", 200),
                request(retained_at, "kept", "alpha", 200),
            ]),
        )
        .await
        .unwrap();

        store.flush_at(paths.as_paths()).await;

        assert!(std::path::Path::new(&file).exists());
        let records = store.export_records(0).await;
        assert!(records.iter().all(|record| record.timestamp >= cutoff));
        assert!(records.iter().any(|record| record.request_id == "kept"));
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn load_day_files_reads_retained_days_and_removes_expired_ones() {
        let now = crate::auth::now();
        let directory = test_directory("load-days");
        let directory_path = format!("{}/", directory.display());
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let expired_at = now - 40 * 86_400;
        tokio::fs::write(
            day_path(&directory_path, day_of(now)),
            serialize_logs(&[request(now, "kept", "alpha", 200)]),
        )
        .await
        .unwrap();
        tokio::fs::write(
            day_path(&directory_path, day_of(expired_at)),
            serialize_logs(&[request(expired_at, "expired", "alpha", 200)]),
        )
        .await
        .unwrap();
        let unrelated = format!("{directory_path}notes.txt");
        tokio::fs::write(&unrelated, "not activity").await.unwrap();

        let persisted = load_day_files(&directory_path, now - 30 * 86_400)
            .await
            .unwrap();

        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].request_id, "kept");
        assert!(!std::path::Path::new(&day_path(&directory_path, day_of(expired_at))).exists());
        assert!(std::path::Path::new(&unrelated).exists());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn import_writes_only_the_day_files_it_touches() {
        let now = crate::auth::now();
        let directory = test_directory("import-days");
        let paths = TestPaths::new(&directory);
        let store = store(vec![request(now, "existing", "alpha", 200)]);
        tokio::fs::create_dir_all(&paths.directory).await.unwrap();
        let today = paths.day_file(now);
        tokio::fs::write(
            &today,
            serialize_logs(&[request(now, "existing", "alpha", 200)]),
        )
        .await
        .unwrap();
        let imported_at = now - 2 * 86_400;
        let import = ActivityImport {
            format: "yabane-activity".to_owned(),
            version: 1,
            instance_id: Some("remote".to_owned()),
            records: vec![request(imported_at, "imported", "alpha", 200)],
        };

        let result = store.import_at(paths.as_paths(), import).await.unwrap();

        assert_eq!(result.imported, 1);
        let today_contents = tokio::fs::read_to_string(&today).await.unwrap();
        assert_eq!(today_contents.lines().count(), 1);
        assert!(!today_contents.contains("imported"));
        let imported_contents = tokio::fs::read_to_string(paths.day_file(imported_at))
            .await
            .unwrap();
        assert_eq!(imported_contents.lines().count(), 1);
        assert!(imported_contents.contains("imported"));
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    fn complete_pricing() -> Box<crate::pricing::ResolvedPricing> {
        Box::new(crate::pricing::ResolvedPricing {
            pricing: crate::pricing::ModelPricing {
                input_per_million: Some(1.0),
                output_per_million: Some(2.0),
                cache_read_per_million: Some(0.5),
                cache_write_per_million: None,
            },
            sources: crate::pricing::PricingSources::default(),
        })
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
        let paths = TestPaths::new(&directory);

        let result = store
            .recalculate_non_reported_costs_at(paths.as_paths(), None, |_| {
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
            tokio::fs::read_to_string(paths.day_file(now))
                .await
                .unwrap()
                .lines()
                .count(),
            4
        );
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    /// A large import rewrites day files without holding the in-memory lock, so
    /// request recording and console queries keep working while it runs, and a
    /// record that arrives during the write survives the publish.
    #[tokio::test]
    async fn a_running_import_does_not_block_recording_or_queries() {
        let now = crate::auth::now();
        let directory = test_directory("import-concurrency");
        let paths = TestPaths::new(&directory);
        let store = store(Vec::new());
        let import = ActivityImport {
            format: "yabane-activity".to_owned(),
            version: 1,
            instance_id: Some("remote-instance".to_owned()),
            records: vec![request(now, "imported", "alpha", 200)],
        };

        crate::storage::arm_write_hold(directory.to_str().unwrap(), 1, 1_000);
        let importing = {
            let store = store.clone();
            let paths = paths.clone();
            tokio::spawn(async move { store.import_at(paths.as_paths(), import).await })
        };
        while !crate::storage::write_hold_reached(directory.to_str().unwrap()) {
            tokio::task::yield_now().await;
        }

        tokio::time::timeout(
            Duration::from_secs(5),
            store.record(request(now, "during", "alpha", 200)),
        )
        .await
        .expect("recording must not wait for an import to finish");
        let visible = tokio::time::timeout(Duration::from_secs(5), store.export_records(0))
            .await
            .expect("queries must not wait for an import to finish");
        assert_eq!(
            visible.len(),
            1,
            "the record added during the write is queryable"
        );
        assert_eq!(visible[0].request_id, "during");

        importing.await.unwrap().expect("import succeeds");
        let after = store.export_records(0).await;
        assert_eq!(after.len(), 2);
        assert!(after.iter().any(|record| record.request_id == "imported"));
        assert!(
            after.iter().any(|record| record.request_id == "during"),
            "a record added during the import must not be lost by publishing the snapshot"
        );
        // The record added after the snapshot was taken is still pending, and a
        // later flush persists it without duplicating the imported record.
        let pending: Vec<_> = store
            .inner
            .lock()
            .await
            .pending
            .iter()
            .map(|record| record.request_id.clone())
            .collect();
        assert_eq!(pending, vec!["during".to_owned()]);
        store.flush_at(paths.as_paths()).await;
        let day = tokio::fs::read_to_string(paths.day_file(now))
            .await
            .unwrap();
        assert_eq!(day.lines().count(), 2);
        assert!(store.inner.lock().await.pending.is_empty());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    /// The same guarantee holds for a cost recalculation, which can rewrite every
    /// day file that has an estimate.
    #[tokio::test]
    async fn a_running_cost_recalculation_does_not_block_recording_or_queries() {
        let now = crate::auth::now();
        let mut record = request(now, "priced", "provider", 200);
        record.input_tokens = 1_000;
        let store = store(vec![record]);
        let directory = test_directory("recalculate-concurrency");
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let paths = TestPaths::new(&directory);

        crate::storage::arm_write_hold(directory.to_str().unwrap(), 1, 1_000);
        let recalculating = {
            let store = store.clone();
            let paths = paths.clone();
            tokio::spawn(async move {
                store
                    .recalculate_non_reported_costs_at(paths.as_paths(), None, |_| {
                        CostRecalculationResolution::Available(complete_pricing())
                    })
                    .await
            })
        };
        while !crate::storage::write_hold_reached(directory.to_str().unwrap()) {
            tokio::task::yield_now().await;
        }

        tokio::time::timeout(
            Duration::from_secs(5),
            store.record(request(now, "during", "provider", 200)),
        )
        .await
        .expect("recording must not wait for a recalculation to finish");
        let visible = tokio::time::timeout(Duration::from_secs(5), store.export_records(0))
            .await
            .expect("queries must not wait for a recalculation to finish");
        assert_eq!(visible.len(), 2);

        let result = recalculating
            .await
            .unwrap()
            .expect("recalculation succeeds");
        assert_eq!(result.updated, 1);
        let after = store.export_records(0).await;
        assert_eq!(after.len(), 2);
        let priced = after
            .iter()
            .find(|record| record.request_id == "priced")
            .unwrap();
        assert_eq!(priced.cost, Some(0.001));
        assert!(after.iter().any(|record| record.request_id == "during"));
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
        let paths = TestPaths::new(&directory);

        let result = store
            .recalculate_non_reported_costs_at(paths.as_paths(), None, |_| {
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
        assert_eq!(
            tokio::fs::read_to_string(paths.day_file(now))
                .await
                .unwrap()
                .lines()
                .count(),
            2
        );
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
        let paths = TestPaths::new(&directory);

        let result = store
            .recalculate_non_reported_costs_at(paths.as_paths(), None, |_| {
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
        let paths = TestPaths::new(&blocked_parent.join("activity"));

        let result = store
            .recalculate_non_reported_costs_at(paths.as_paths(), None, |_| {
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
        let paths = TestPaths::new(&directory);

        let result = store
            .recalculate_non_reported_costs_at(
                paths.as_paths(),
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
    async fn retention_update_removes_expired_day_files_and_persists_settings() {
        let now = crate::auth::now();
        let directory = test_directory("retention-success");
        let paths = TestPaths::new(&directory);
        let expired_at = now - 2 * 86_400;
        let store = store(vec![
            request(expired_at, "expired", "alpha", 200),
            request(now, "retained", "alpha", 200),
        ]);
        tokio::fs::create_dir_all(&paths.directory).await.unwrap();
        let expired = paths.day_file(expired_at);
        let retained = paths.day_file(now);
        tokio::fs::write(
            &expired,
            serialize_logs(&[request(expired_at, "expired", "alpha", 200)]),
        )
        .await
        .unwrap();
        tokio::fs::write(
            &retained,
            serialize_logs(&[request(now, "retained", "alpha", 200)]),
        )
        .await
        .unwrap();

        store
            .set_retention_days_at(1, paths.as_paths())
            .await
            .unwrap();

        assert_eq!(store.retention_days(), 1);
        assert_eq!(store.export_records(0).await.len(), 1);
        assert!(!std::path::Path::new(&expired).exists());
        assert!(std::path::Path::new(&retained).exists());
        let settings: ActivitySettings =
            serde_json::from_slice(&tokio::fs::read(&paths.settings).await.unwrap()).unwrap();
        assert_eq!(settings.retention_days, 1);
        assert!(!std::path::Path::new(&paths.transaction).exists());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn failed_retention_update_does_not_publish_or_delete_records() {
        let now = crate::auth::now();
        let directory = test_directory("retention-failure");
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let blocked_parent = directory.join("not-a-directory");
        tokio::fs::write(&blocked_parent, b"blocked").await.unwrap();
        let paths = TestPaths::new(&blocked_parent.join("activity"));
        let store = store(vec![
            request(now - 2 * 86_400, "would-expire", "alpha", 200),
            request(now, "would-remain", "alpha", 200),
        ]);

        let result = store.set_retention_days_at(1, paths.as_paths()).await;

        assert!(result.is_err());
        assert_eq!(store.retention_days(), DEFAULT_RETENTION_DAYS);
        assert_eq!(store.export_records(0).await.len(), 2);
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

        let stats = store
            .stats(now - 10, filters, 2, now, ModelDimension::Incoming)
            .await;

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
            .stats(
                now - 10,
                ActivityFilters::default(),
                2,
                now,
                ModelDimension::Incoming,
            )
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
            .stats(
                now - 10,
                ActivityFilters::default(),
                1,
                now,
                ModelDimension::Incoming,
            )
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
        .stats(
            now - 10,
            ActivityFilters::default(),
            2,
            now,
            ModelDimension::Incoming,
        )
        .await;

        assert_eq!(stats.provider_buckets.len(), 2);
        assert_eq!(stats.provider_buckets[0].name, "alpha");
        assert_eq!(stats.provider_buckets[0].requests, vec![1, 1]);
        assert_eq!(stats.provider_buckets[1].name, "beta");
        assert_eq!(stats.provider_buckets[1].requests, vec![0, 1]);
    }

    #[tokio::test]
    async fn stats_group_the_model_analysis_by_either_model_name() {
        let now = crate::auth::now();
        let mut aliased = request(now - 3, "aliased", "alpha", 200);
        aliased.model = "public-alias".to_owned();
        aliased.upstream_model = Some("vendor/model-v2".to_owned());
        let mut unreported = request(now - 2, "unreported", "alpha", 200);
        unreported.model = "direct-model".to_owned();
        unreported.upstream_model = None;

        let incoming = store(vec![aliased.clone(), unreported.clone()])
            .stats(
                now - 10,
                ActivityFilters::default(),
                2,
                now,
                ModelDimension::Incoming,
            )
            .await;
        assert_eq!(
            incoming
                .by_model
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["direct-model", "public-alias"]
        );

        let outgoing = store(vec![aliased, unreported])
            .stats(
                now - 10,
                ActivityFilters::default(),
                2,
                now,
                ModelDimension::Outgoing,
            )
            .await;
        // The alias and the model it routes to share one row, and a record without a
        // recorded Provider model stays visible as one unnamed group instead of disappearing.
        assert_eq!(
            outgoing
                .by_model
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["", "vendor/model-v2"]
        );
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
            .stats(
                now - 10,
                ActivityFilters::default(),
                2,
                now,
                ModelDimension::Incoming,
            )
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

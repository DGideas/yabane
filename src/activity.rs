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

const ACTIVITY_FILE: &str = "data/activity.jsonl";
const ACTIVITY_SETTINGS_FILE: &str = "data/activity-settings.json";
const FLUSH_SIZE: usize = 10;
const FLUSH_INTERVAL: Duration = Duration::from_secs(60);
const DEFAULT_RETENTION_DAYS: u64 = 30;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RequestLog {
    pub timestamp: u64,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_instance_id: Option<String>,
    pub path: String,
    pub model: String,
    pub provider: String,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_protocol: Option<String>,
    pub status: u16,
    pub latency_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    #[serde(default)]
    pub cost: Option<f64>,
    pub streaming: bool,
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

#[derive(Serialize)]
pub struct Stats {
    pub requests: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    pub by_provider: Vec<ProviderStats>,
}

#[derive(Serialize)]
pub struct ProviderStats {
    pub provider: String,
    pub requests: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
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

    pub async fn logs(&self, since: u64, limit: usize) -> Vec<RequestLog> {
        let since = since.max(retention_cutoff(self.retention_days()));
        let data = self.inner.lock().await;
        data.persisted
            .iter()
            .chain(&data.pending)
            .filter(|log| log.timestamp >= since)
            .rev()
            .take(limit.min(1000))
            .cloned()
            .collect()
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
        if !(1..=3650).contains(&days) {
            return Err("Activity retention must be between 1 and 3650 days".to_owned());
        }
        let body = serde_json::to_vec_pretty(&ActivitySettings {
            retention_days: days,
        })
        .expect("serialize activity settings");
        crate::storage::write_atomic(ACTIVITY_SETTINGS_FILE, &body)
            .await
            .map_err(|err| format!("save activity settings: {err}"))?;
        self.retention_days.store(days, Ordering::Relaxed);
        self.flush().await;
        Ok(())
    }

    pub async fn preview_import(&self, import: &ActivityImport) -> Result<ImportResult, String> {
        self.validate_import(import)?;
        let data = self.inner.lock().await;
        Ok(self.classify_import(import, &data).0)
    }

    pub async fn import(&self, import: ActivityImport) -> Result<ImportResult, String> {
        self.validate_import(&import)?;
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
                .map_err(|err| format!("persist imported activity: {err}"))?;
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

    pub async fn stats(&self, since: u64) -> Stats {
        let since = since.max(retention_cutoff(self.retention_days()));
        let data = self.inner.lock().await;
        let logs: Vec<_> = data
            .persisted
            .iter()
            .chain(&data.pending)
            .filter(|log| log.timestamp >= since)
            .collect();
        let mut providers = std::collections::BTreeMap::<String, ProviderStats>::new();
        for log in &logs {
            let stats = providers
                .entry(log.provider.clone())
                .or_insert_with(|| ProviderStats {
                    provider: log.provider.clone(),
                    requests: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    cached_tokens: 0,
                    cost: 0.0,
                });
            stats.requests += 1;
            stats.input_tokens += log.input_tokens;
            stats.output_tokens += log.output_tokens;
            stats.cached_tokens += log.cached_tokens;
            stats.cost += log.cost.unwrap_or(0.0);
        }
        Stats {
            requests: logs.len(),
            input_tokens: logs.iter().map(|log| log.input_tokens).sum(),
            output_tokens: logs.iter().map(|log| log.output_tokens).sum(),
            cached_tokens: logs.iter().map(|log| log.cached_tokens).sum(),
            cost: logs.iter().filter_map(|log| log.cost).sum(),
            by_provider: providers.into_values().collect(),
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
    crate::storage::write_atomic(ACTIVITY_FILE, &serialize_logs(logs)).await
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

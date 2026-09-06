use std::{io::ErrorKind, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::Mutex};
use tracing::error;

const ACTIVITY_FILE: &str = "data/activity.jsonl";
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

#[derive(Serialize)]
pub struct ImportResult {
    pub imported: usize,
    pub duplicates: usize,
    pub expired: usize,
    pub total: usize,
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
        let cutoff = retention_cutoff();
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

    pub async fn export_records(&self) -> Vec<RequestLog> {
        let data = self.inner.lock().await;
        let mut records: Vec<_> = data
            .persisted
            .iter()
            .chain(&data.pending)
            .cloned()
            .collect();
        records.sort_by(|a, b| {
            a.timestamp
                .cmp(&b.timestamp)
                .then_with(|| a.request_id.cmp(&b.request_id))
        });
        records
    }

    pub async fn import(&self, import: ActivityImport) -> Result<ImportResult, String> {
        if import.format != "yabane-activity" || import.version != 1 {
            return Err("Unsupported activity export format or version".to_owned());
        }
        const MAX_IMPORT_RECORDS: usize = 1_000_000;
        if import.records.len() > MAX_IMPORT_RECORDS {
            return Err(format!(
                "Activity import cannot contain more than {MAX_IMPORT_RECORDS} records"
            ));
        }
        let total = import.records.len();
        let cutoff = retention_cutoff();
        let _flush_guard = self.flush_lock.lock().await;
        let mut data = self.inner.lock().await;
        let source_id = import
            .instance_id
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or("legacy");
        let local_source = self.instance_id.as_str();
        let imported_identity = |record: &RequestLog| {
            (
                record
                    .source_instance_id
                    .as_deref()
                    .unwrap_or(source_id)
                    .to_owned(),
                record.request_id.clone(),
            )
        };
        let stored_identity = |record: &RequestLog| {
            (
                record
                    .source_instance_id
                    .as_deref()
                    .unwrap_or(local_source)
                    .to_owned(),
                record.request_id.clone(),
            )
        };
        let mut existing: std::collections::HashSet<(String, String)> = data
            .persisted
            .iter()
            .chain(&data.pending)
            .map(stored_identity)
            .collect();
        let mut imported = Vec::new();
        let mut duplicates = 0;
        let mut expired = 0;
        for record in import.records {
            if record.request_id.is_empty() {
                return Err("Activity records must contain a request_id".to_owned());
            }
            if record.timestamp < cutoff {
                expired += 1;
            } else {
                let identity = imported_identity(&record);
                if !existing.insert(identity) {
                    duplicates += 1;
                    continue;
                }
                let mut record = record;
                if record.source_instance_id.is_none() && source_id != local_source {
                    record.source_instance_id = Some(source_id.to_owned());
                }
                imported.push(record);
            }
        }

        if !imported.is_empty() {
            let mut records: Vec<_> = data
                .persisted
                .iter()
                .chain(&data.pending)
                .filter(|log| log.timestamp >= cutoff)
                .cloned()
                .collect();
            records.extend(imported.iter().cloned());
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
        Ok(ImportResult {
            imported: imported.len(),
            duplicates,
            expired,
            total,
        })
    }

    pub async fn stats(&self, since: u64) -> Stats {
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
            let cutoff = retention_cutoff();
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

fn retention_cutoff() -> u64 {
    let days = std::env::var("YABANE_ACTIVITY_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_RETENTION_DAYS);
    crate::auth::now().saturating_sub(days * 86_400)
}

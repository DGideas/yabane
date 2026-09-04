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

#[derive(Clone, Default)]
pub struct ActivityStore {
    inner: Arc<Mutex<ActivityData>>,
    flush_lock: Arc<Mutex<()>>,
}

#[derive(Default)]
struct ActivityData {
    persisted: Vec<RequestLog>,
    pending: Vec<RequestLog>,
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

fn retention_cutoff() -> u64 {
    let days = std::env::var("YABANE_ACTIVITY_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_RETENTION_DAYS);
    crate::auth::now().saturating_sub(days * 86_400)
}

use std::{
    collections::HashSet,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use yabane_extension_api::{
    EXTENSION_API_VERSION, ExchangeOutcome, Extension, ExtensionHook, HookStage, ObservedHeader,
    ObservedUpstreamRequest, ObservedUpstreamResponseHead, Protocol, RequestContext,
    UpstreamExchangeHook, UpstreamExchangeObserver,
};

pub const ID: &str = "traffic-capture";
const CONFIG_FILE: &str = "data/extensions/traffic-capture/config.json";
const CAPTURES_FILE: &str = "data/extensions/traffic-capture/captures.json";
const DEFAULT_BODY_LIMIT: usize = 1024 * 1024;
const DEFAULT_RETENTION_DAYS: u32 = 1;
const MAX_CAPTURES: usize = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CaptureConfig {
    pub active: bool,
    pub remaining: u32,
    pub expires_at: Option<u64>,
    pub provider_id: String,
    pub endpoint_id: String,
    pub model: String,
    pub body_limit: usize,
    pub retention_days: u32,
    pub redacted_headers: Vec<String>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            active: false,
            remaining: 0,
            expires_at: None,
            provider_id: String::new(),
            endpoint_id: String::new(),
            model: String::new(),
            body_limit: DEFAULT_BODY_LIMIT,
            retention_days: DEFAULT_RETENTION_DAYS,
            redacted_headers: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CaptureRecord {
    pub request_id: String,
    pub timestamp: u64,
    pub expires_at: u64,
    pub public_model: String,
    pub upstream_model: String,
    pub provider_id: String,
    pub endpoint_id: String,
    pub caller_protocol: String,
    pub upstream_protocol: String,
    pub streaming: bool,
    pub request_headers: Vec<ObservedHeaderValue>,
    pub request_body: Vec<u8>,
    pub request_truncated: bool,
    pub status: Option<u16>,
    pub response_headers: Vec<ObservedHeaderValue>,
    pub response_body: Vec<u8>,
    pub response_truncated: bool,
    pub outcome: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ObservedHeaderValue {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CaptureStatus {
    pub config: CaptureConfig,
    pub retained: usize,
    pub dropped: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CaptureSummary {
    pub request_id: String,
    pub timestamp: u64,
    pub expires_at: u64,
    pub public_model: String,
    pub provider_id: String,
    pub endpoint_id: String,
    pub status: Option<u16>,
    pub bytes: usize,
    pub truncated: bool,
    pub outcome: String,
}

pub struct TrafficCapture {
    config: Arc<std::sync::RwLock<CaptureConfig>>,
    captures: Arc<RwLock<Vec<CaptureRecord>>>,
    sender: tokio::sync::mpsc::Sender<CaptureMessage>,
    dropped: Arc<std::sync::atomic::AtomicU64>,
    config_revision: Arc<std::sync::atomic::AtomicU64>,
    config_persistence: Arc<tokio::sync::Mutex<()>>,
    config_path: PathBuf,
    captures_path: PathBuf,
}

impl TrafficCapture {
    pub async fn load() -> Result<Self, String> {
        Self::load_from(CONFIG_FILE.into(), CAPTURES_FILE.into()).await
    }

    async fn load_from(config_path: PathBuf, captures_path: PathBuf) -> Result<Self, String> {
        let mut config: CaptureConfig = load_json(&config_path).await?.unwrap_or_default();
        if config.expires_at.is_some_and(|expiry| expiry <= now()) {
            config.active = false;
            config.remaining = 0;
            persist_json(&config_path, &config).await?;
        }
        let mut captures: Vec<CaptureRecord> = load_json(&captures_path).await?.unwrap_or_default();
        let retained_before_expiry = captures.len();
        captures.retain(|capture| capture.expires_at > now());
        if captures.len() != retained_before_expiry {
            persist_json(&captures_path, &captures).await?;
        }
        let config = Arc::new(std::sync::RwLock::new(config));
        let captures = Arc::new(RwLock::new(captures));
        let dropped = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let config_revision = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let config_persistence = Arc::new(tokio::sync::Mutex::new(()));
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<CaptureMessage>(32);
        let writer_captures = captures.clone();
        let writer_path = captures_path.clone();
        let writer_dropped = dropped.clone();
        tokio::spawn(async move {
            while let Some(message) = receiver.recv().await {
                match message {
                    CaptureMessage::Record(record) => {
                        let mut records = writer_captures.write().await;
                        let mut updated = records.clone();
                        updated.retain(|capture| capture.expires_at > now());
                        updated.push(*record);
                        if updated.len() > MAX_CAPTURES {
                            let remove = updated.len() - MAX_CAPTURES;
                            updated.drain(..remove);
                        }
                        if persist_json(&writer_path, &updated).await.is_ok() {
                            *records = updated;
                        } else {
                            writer_dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    CaptureMessage::Flush(completed) => {
                        let _ = completed.send(());
                    }
                }
            }
        });
        Ok(Self {
            config,
            captures,
            sender,
            dropped,
            config_revision,
            config_persistence,
            config_path,
            captures_path,
        })
    }

    pub async fn status(&self) -> CaptureStatus {
        self.expire_session(now());
        self.compact().await;
        let config = self
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let retained = self.captures.read().await.len();
        CaptureStatus {
            config,
            retained,
            dropped: self.dropped.load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    pub async fn configure(&self, mut config: CaptureConfig) -> Result<CaptureStatus, String> {
        if config.active && config.remaining == 0 {
            return Err("Capture count must be at least one".to_owned());
        }
        if config.active && config.expires_at.is_none_or(|expiry| expiry <= now()) {
            return Err("Active capture expiry must be in the future".to_owned());
        }
        if !(1..=30).contains(&config.retention_days) {
            return Err("Capture retention must be between 1 and 30 days".to_owned());
        }
        if !(1024..=8 * 1024 * 1024).contains(&config.body_limit) {
            return Err("Body limit must be between 1 KiB and 8 MiB".to_owned());
        }
        config.provider_id = config.provider_id.trim().to_owned();
        config.endpoint_id = config.endpoint_id.trim().to_owned();
        config.model = config.model.trim().to_owned();
        if config.active && (config.provider_id.is_empty() || config.endpoint_id.is_empty()) {
            return Err("Active capture requires a Provider and Endpoint".to_owned());
        }
        config.redacted_headers = config
            .redacted_headers
            .into_iter()
            .map(|name| name.trim().to_ascii_lowercase())
            .filter(|name| !name.is_empty())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        config.redacted_headers.sort();
        let _persistence = self.config_persistence.lock().await;
        persist_json(&self.config_path, &config).await?;
        *self
            .config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = config;
        self.config_revision
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        drop(_persistence);
        Ok(self.status().await)
    }

    pub async fn stop(&self) -> Result<CaptureStatus, String> {
        let mut config = self
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        config.active = false;
        config.remaining = 0;
        self.configure(config).await
    }

    pub async fn list(&self) -> Vec<CaptureSummary> {
        self.compact().await;
        self.captures
            .read()
            .await
            .iter()
            .rev()
            .map(|capture| CaptureSummary {
                request_id: capture.request_id.clone(),
                timestamp: capture.timestamp,
                expires_at: capture.expires_at,
                public_model: capture.public_model.clone(),
                provider_id: capture.provider_id.clone(),
                endpoint_id: capture.endpoint_id.clone(),
                status: capture.status,
                bytes: capture.request_body.len() + capture.response_body.len(),
                truncated: capture.request_truncated || capture.response_truncated,
                outcome: capture.outcome.clone(),
            })
            .collect()
    }

    pub async fn get(&self, request_id: &str) -> Option<CaptureRecord> {
        self.compact().await;
        self.captures
            .read()
            .await
            .iter()
            .find(|capture| capture.request_id == request_id)
            .cloned()
    }

    pub async fn delete(&self, request_id: &str) -> Result<bool, String> {
        self.flush_pending().await?;
        let mut captures = self.captures.write().await;
        let Some(index) = captures
            .iter()
            .position(|capture| capture.request_id == request_id)
        else {
            return Ok(false);
        };
        let mut updated = captures.clone();
        updated.remove(index);
        persist_json(&self.captures_path, &updated).await?;
        *captures = updated;
        Ok(true)
    }

    pub async fn delete_all(&self) -> Result<(), String> {
        self.flush_pending().await?;
        let mut captures = self.captures.write().await;
        persist_json(&self.captures_path, &Vec::<CaptureRecord>::new()).await?;
        captures.clear();
        Ok(())
    }

    pub async fn flush(&self) -> Result<(), String> {
        self.flush_pending().await?;
        let _persistence = self.config_persistence.lock().await;
        let config = self
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        persist_json(&self.config_path, &config).await
    }

    async fn flush_pending(&self) -> Result<(), String> {
        let (completed, flushed) = tokio::sync::oneshot::channel();
        self.sender
            .send(CaptureMessage::Flush(completed))
            .await
            .map_err(|_| "Traffic Capture writer stopped before flush".to_owned())?;
        flushed
            .await
            .map_err(|_| "Traffic Capture writer stopped during flush".to_owned())
    }

    fn expire_session(&self, current: u64) {
        let stopped = {
            let mut config = self
                .config
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if config.active && config.expires_at.is_some_and(|expiry| expiry <= current) {
                config.active = false;
                config.remaining = 0;
                Some(config.clone())
            } else {
                None
            }
        };
        if let Some(config) = stopped {
            self.persist_config_in_background(config);
        }
    }

    fn persist_config_in_background(&self, config: CaptureConfig) {
        let revision = self
            .config_revision
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let config_revision = self.config_revision.clone();
        let config_persistence = self.config_persistence.clone();
        let config_path = self.config_path.clone();
        let dropped = self.dropped.clone();
        tokio::spawn(async move {
            let _persistence = config_persistence.lock().await;
            if config_revision.load(std::sync::atomic::Ordering::Relaxed) == revision
                && persist_json(&config_path, &config).await.is_err()
            {
                dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });
    }

    async fn compact(&self) {
        let mut captures = self.captures.write().await;
        if !captures.iter().any(|capture| capture.expires_at <= now()) {
            return;
        }
        let mut updated = captures.clone();
        updated.retain(|capture| capture.expires_at > now());
        if persist_json(&self.captures_path, &updated).await.is_ok() {
            *captures = updated;
        } else {
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl ExtensionHook for TrafficCapture {
    fn extension_id(&self) -> &'static str {
        ID
    }
}

impl UpstreamExchangeHook for TrafficCapture {
    fn is_interested(&self, context: &RequestContext<'_>) -> bool {
        let current = now();
        self.expire_session(current);
        let config = self
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        config.active
            && config.remaining > 0
            && config.expires_at.is_some_and(|expiry| expiry > current)
            && config.provider_id == context.provider_id
            && config.endpoint_id == context.endpoint_id
            && (config.model.is_empty() || config.model == context.public_model)
    }

    fn begin(
        &self,
        context: &RequestContext<'_>,
        request: ObservedUpstreamRequest<'_>,
    ) -> Option<Box<dyn UpstreamExchangeObserver>> {
        let current = now();
        self.expire_session(current);
        let mut config = self
            .config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !config.active
            || config.remaining == 0
            || (!config.provider_id.is_empty() && config.provider_id != context.provider_id)
            || (!config.endpoint_id.is_empty() && config.endpoint_id != context.endpoint_id)
            || (!config.model.is_empty() && config.model != context.public_model)
        {
            return None;
        }
        config.remaining -= 1;
        if config.remaining == 0 {
            config.active = false;
        }
        let config_snapshot = config.clone();
        drop(config);
        self.persist_config_in_background(config_snapshot.clone());
        let redacted = config_snapshot
            .redacted_headers
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        let (request_body, request_truncated) = bounded(request.body, config_snapshot.body_limit);
        let record = CaptureRecord {
            request_id: context.request_id.to_owned(),
            timestamp: current,
            expires_at: current + u64::from(config_snapshot.retention_days) * 86_400,
            public_model: context.public_model.to_owned(),
            upstream_model: context.upstream_model.to_owned(),
            provider_id: context.provider_id.to_owned(),
            endpoint_id: context.endpoint_id.to_owned(),
            caller_protocol: protocol_name(context.caller_protocol).to_owned(),
            upstream_protocol: protocol_name(context.upstream_protocol).to_owned(),
            streaming: context.requested_streaming,
            request_headers: redact(request.headers, &redacted),
            request_body,
            request_truncated,
            status: None,
            response_headers: Vec::new(),
            response_body: Vec::new(),
            response_truncated: false,
            outcome: "capturing".to_owned(),
        };
        Some(Box::new(CaptureObserver {
            record: Mutex::new(Some(record)),
            sender: self.sender.clone(),
            body_limit: config_snapshot.body_limit,
            redacted,
            dropped: self.dropped.clone(),
        }))
    }
}

enum CaptureMessage {
    Record(Box<CaptureRecord>),
    Flush(tokio::sync::oneshot::Sender<()>),
}

struct CaptureObserver {
    record: Mutex<Option<CaptureRecord>>,
    sender: tokio::sync::mpsc::Sender<CaptureMessage>,
    body_limit: usize,
    redacted: HashSet<String>,
    dropped: Arc<std::sync::atomic::AtomicU64>,
}

impl UpstreamExchangeObserver for CaptureObserver {
    fn on_response_head(&mut self, response: ObservedUpstreamResponseHead<'_>) {
        if let Some(record) = self.record.get_mut().expect("capture observer lock") {
            record.status = Some(response.status.as_u16());
            record.response_headers = redact(response.headers, &self.redacted);
        }
    }

    fn on_response_chunk(&mut self, chunk: &Bytes) {
        if let Some(record) = self.record.get_mut().expect("capture observer lock") {
            let remaining = self.body_limit.saturating_sub(record.response_body.len());
            record
                .response_body
                .extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            record.response_truncated |= chunk.len() > remaining;
        }
    }

    fn on_complete(&mut self, outcome: ExchangeOutcome) {
        let Some(mut record) = self.record.get_mut().expect("capture observer lock").take() else {
            return;
        };
        record.outcome = match outcome {
            ExchangeOutcome::Complete => "complete",
            ExchangeOutcome::TransportError => "transport_error",
            ExchangeOutcome::ResponseReadError => "response_read_error",
            ExchangeOutcome::Interrupted => "interrupted",
        }
        .to_owned();
        if self
            .sender
            .try_send(CaptureMessage::Record(Box::new(record)))
            .is_err()
        {
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl Drop for CaptureObserver {
    fn drop(&mut self) {
        self.on_complete(ExchangeOutcome::Interrupted);
    }
}

fn bounded(value: &[u8], limit: usize) -> (Vec<u8>, bool) {
    (
        value[..value.len().min(limit)].to_vec(),
        value.len() > limit,
    )
}

fn redact(headers: &[ObservedHeader], extra: &HashSet<String>) -> Vec<ObservedHeaderValue> {
    headers
        .iter()
        .map(|header| ObservedHeaderValue {
            name: header.name.clone(),
            value: if extra.contains(&header.name.to_ascii_lowercase()) {
                "[REDACTED]".to_owned()
            } else {
                header.value.clone()
            },
        })
        .collect()
}

fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenAiChatCompletions => "openai_chat_completions",
        Protocol::OpenAiResponses => "openai_responses",
        Protocol::AnthropicMessages => "anthropic_messages",
    }
}

async fn load_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("parse {}: {error}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("read {}: {error}", path.display())),
    }
}

async fn persist_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    write_private_atomic(path, &bytes).await
}

async fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("capture"),
        unique_suffix()
    ));
    let result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&temporary)
            .await
            .map_err(|error| format!("create {}: {error}", temporary.display()))?;
        use tokio::io::AsyncWriteExt;
        file.write_all(bytes)
            .await
            .map_err(|error| format!("write {}: {error}", temporary.display()))?;
        file.sync_all()
            .await
            .map_err(|error| format!("sync {}: {error}", temporary.display()))?;
        drop(file);
        tokio::fs::rename(&temporary, path)
            .await
            .map_err(|error| format!("replace {}: {error}", path.display()))
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        now(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn metadata() -> Extension {
    Extension {
        id: ID,
        name: "Traffic Capture",
        version: env!("CARGO_PKG_VERSION"),
        api_version: EXTENSION_API_VERSION,
        description: "Temporarily captures credential-redacted upstream request and response exchanges for troubleshooting.",
        hooks: &[HookStage::UpstreamExchange],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_is_bounded() {
        assert_eq!(bounded(b"abcdef", 3), (b"abc".to_vec(), true));
    }

    #[test]
    fn configured_headers_are_redacted() {
        let headers = [ObservedHeader {
            name: "x-private".into(),
            value: "secret".into(),
        }];
        let values = redact(&headers, &HashSet::from(["x-private".to_owned()]));
        assert_eq!(values[0].value, "[REDACTED]");
    }

    #[tokio::test]
    async fn active_capture_requires_scope_and_future_expiry() {
        let directory = std::env::temp_dir().join(format!(
            "yabane-traffic-capture-validation-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let capture = TrafficCapture::load_from(
            directory.join("config.json"),
            directory.join("captures.json"),
        )
        .await
        .unwrap();
        let mut config = CaptureConfig {
            active: true,
            remaining: 1,
            expires_at: Some(now() + 60),
            ..CaptureConfig::default()
        };
        assert!(capture.configure(config.clone()).await.is_err());
        config.provider_id = "provider".to_owned();
        config.endpoint_id = "endpoint".to_owned();
        config.expires_at = Some(now());
        assert!(capture.configure(config).await.is_err());
        let _ = tokio::fs::remove_dir_all(directory).await;
    }

    #[tokio::test]
    async fn matching_is_scoped_and_consumes_exactly_next_n_requests() {
        let directory = std::env::temp_dir().join(format!(
            "yabane-traffic-capture-matching-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let capture = TrafficCapture::load_from(
            directory.join("config.json"),
            directory.join("captures.json"),
        )
        .await
        .unwrap();
        capture
            .configure(CaptureConfig {
                active: true,
                remaining: 2,
                expires_at: Some(now() + 60),
                provider_id: "provider".to_owned(),
                endpoint_id: "endpoint".to_owned(),
                model: "provider/model".to_owned(),
                ..CaptureConfig::default()
            })
            .await
            .unwrap();
        let matching = RequestContext {
            request_id: "one",
            public_model: "provider/model",
            upstream_model: "model",
            provider_id: "provider",
            endpoint_id: "endpoint",
            caller_protocol: Protocol::OpenAiResponses,
            upstream_protocol: Protocol::OpenAiResponses,
            requested_streaming: false,
        };
        let nonmatching = RequestContext {
            request_id: "other",
            public_model: "provider/model",
            upstream_model: "model",
            provider_id: "provider",
            endpoint_id: "other",
            caller_protocol: Protocol::OpenAiResponses,
            upstream_protocol: Protocol::OpenAiResponses,
            requested_streaming: false,
        };
        assert!(!capture.is_interested(&nonmatching));
        assert!(
            capture
                .begin(
                    &nonmatching,
                    ObservedUpstreamRequest {
                        headers: &[],
                        body: &[]
                    }
                )
                .is_none()
        );
        assert_eq!(capture.status().await.config.remaining, 2);

        let observer = capture
            .begin(
                &matching,
                ObservedUpstreamRequest {
                    headers: &[],
                    body: &[],
                },
            )
            .unwrap();
        drop(observer);
        let mut second = capture
            .begin(
                &RequestContext {
                    request_id: "two",
                    ..matching
                },
                ObservedUpstreamRequest {
                    headers: &[],
                    body: &[],
                },
            )
            .unwrap();
        second.on_complete(ExchangeOutcome::Complete);
        assert!(!capture.is_interested(&matching));
        assert!(
            capture
                .begin(
                    &matching,
                    ObservedUpstreamRequest {
                        headers: &[],
                        body: &[]
                    }
                )
                .is_none()
        );
        capture.flush_pending().await.unwrap();
        let status = capture.status().await;
        assert!(!status.config.active);
        assert_eq!(status.config.remaining, 0);
        assert_eq!(status.retained, 2);
        assert!(
            capture
                .list()
                .await
                .iter()
                .any(|record| record.outcome == "interrupted")
        );
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn expired_session_is_not_interested_and_is_stopped() {
        let directory = std::env::temp_dir().join(format!(
            "yabane-traffic-capture-expiry-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let capture = TrafficCapture::load_from(
            directory.join("config.json"),
            directory.join("captures.json"),
        )
        .await
        .unwrap();
        *capture
            .config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = CaptureConfig {
            active: true,
            remaining: 1,
            expires_at: Some(now().saturating_sub(1)),
            provider_id: "provider".to_owned(),
            endpoint_id: "endpoint".to_owned(),
            ..CaptureConfig::default()
        };
        let context = RequestContext {
            request_id: "expired",
            public_model: "provider/model",
            upstream_model: "model",
            provider_id: "provider",
            endpoint_id: "endpoint",
            caller_protocol: Protocol::OpenAiResponses,
            upstream_protocol: Protocol::OpenAiResponses,
            requested_streaming: false,
        };
        assert!(!capture.is_interested(&context));
        assert!(
            capture
                .begin(
                    &context,
                    ObservedUpstreamRequest {
                        headers: &[],
                        body: &[]
                    }
                )
                .is_none()
        );
        let status = capture.status().await;
        assert!(!status.config.active);
        assert_eq!(status.config.remaining, 0);
        let _ = tokio::fs::remove_dir_all(directory).await;
    }

    #[test]
    fn observer_bounds_streaming_chunks_and_completes_once() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        let dropped = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut observer = CaptureObserver {
            record: Mutex::new(Some(test_record("request"))),
            sender,
            body_limit: 3,
            redacted: HashSet::new(),
            dropped,
        };
        observer.on_response_chunk(&Bytes::from_static(b"ab"));
        observer.on_response_chunk(&Bytes::from_static(b"cdef"));
        observer.on_complete(ExchangeOutcome::ResponseReadError);
        observer.on_complete(ExchangeOutcome::Complete);
        drop(observer);
        let CaptureMessage::Record(record) = receiver.try_recv().unwrap() else {
            panic!("expected a captured record");
        };
        assert_eq!(record.response_body, b"abc");
        assert!(record.response_truncated);
        assert_eq!(record.outcome, "response_read_error");
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn failed_capture_persistence_does_not_publish_memory_only_record() {
        let directory = std::env::temp_dir().join(format!(
            "yabane-traffic-capture-failure-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let captures_path = directory.join("captures.json");
        let capture =
            TrafficCapture::load_from(directory.join("config.json"), captures_path.clone())
                .await
                .unwrap();
        capture
            .configure(CaptureConfig {
                active: true,
                remaining: 1,
                expires_at: Some(now() + 60),
                provider_id: "provider".to_owned(),
                endpoint_id: "endpoint".to_owned(),
                ..CaptureConfig::default()
            })
            .await
            .unwrap();
        tokio::fs::create_dir(&captures_path).await.unwrap();

        let context = RequestContext {
            request_id: "request",
            public_model: "provider/model",
            upstream_model: "model",
            provider_id: "provider",
            endpoint_id: "endpoint",
            caller_protocol: Protocol::OpenAiResponses,
            upstream_protocol: Protocol::OpenAiResponses,
            requested_streaming: false,
        };
        let mut observer = capture
            .begin(
                &context,
                ObservedUpstreamRequest {
                    headers: &[],
                    body: b"request body",
                },
            )
            .unwrap();
        observer.on_complete(ExchangeOutcome::Complete);
        for _ in 0..50 {
            if capture.dropped.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let status = capture.status().await;
        assert_eq!(status.retained, 0);
        assert_eq!(status.dropped, 1);
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn delete_all_waits_for_queued_records() {
        let directory = std::env::temp_dir().join(format!(
            "yabane-traffic-capture-delete-all-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let capture = TrafficCapture::load_from(
            directory.join("config.json"),
            directory.join("captures.json"),
        )
        .await
        .unwrap();
        capture
            .sender
            .send(CaptureMessage::Record(Box::new(test_record("request"))))
            .await
            .unwrap();

        capture.delete_all().await.unwrap();
        assert!(capture.list().await.is_empty());
        let persisted: Vec<CaptureRecord> = load_json(&directory.join("captures.json"))
            .await
            .unwrap()
            .unwrap();
        assert!(persisted.is_empty());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    fn test_record(request_id: &str) -> CaptureRecord {
        CaptureRecord {
            request_id: request_id.to_owned(),
            timestamp: now(),
            expires_at: now() + 60,
            public_model: "provider/model".to_owned(),
            upstream_model: "model".to_owned(),
            provider_id: "provider".to_owned(),
            endpoint_id: "endpoint".to_owned(),
            caller_protocol: "openai_responses".to_owned(),
            upstream_protocol: "openai_responses".to_owned(),
            streaming: false,
            request_headers: Vec::new(),
            request_body: Vec::new(),
            request_truncated: false,
            status: Some(200),
            response_headers: Vec::new(),
            response_body: Vec::new(),
            response_truncated: false,
            outcome: "complete".to_owned(),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn capture_files_are_private_and_atomic() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!(
            "yabane-traffic-capture-storage-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let path = directory.join("captures.json");
        persist_json(&path, &vec!["sensitive prompt"])
            .await
            .unwrap();
        persist_json(&path, &vec!["sensitive response"])
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::metadata(&path)
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let mut entries = tokio::fs::read_dir(&directory).await.unwrap();
        assert_eq!(entries.next_entry().await.unwrap().unwrap().path(), path);
        assert!(entries.next_entry().await.unwrap().is_none());
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
}

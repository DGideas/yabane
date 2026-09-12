use std::{
    collections::{HashMap, HashSet},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use axum::http::{HeaderMap, StatusCode};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
#[cfg(any(feature = "extension-request-defaults", test))]
use yabane_extension_api::{EXTENSION_API_VERSION, Extension};
use yabane_extension_api::{
    ExtensionError, HookOutcome, RequestContext, UpstreamHeadersHook, UpstreamRequestHook,
};

pub const EXTENSIONS_FILE: &str = "data/extensions.json";

#[derive(Clone, Debug, Serialize)]
pub struct ExtensionView {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub api_version: u32,
    pub description: &'static str,
    pub implementation: &'static str,
    pub included: bool,
    pub enabled: bool,
    pub runtime_configurable: bool,
    pub hooks: Vec<&'static str>,
}

#[derive(Clone, Debug)]
struct ExtensionInfo {
    id: &'static str,
    name: &'static str,
    version: &'static str,
    api_version: u32,
    description: &'static str,
    hooks: Vec<&'static str>,
}

struct ExtensionEntry {
    info: ExtensionInfo,
    enabled: AtomicBool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ExtensionSettings {
    #[serde(default)]
    enabled: HashMap<String, bool>,
}

#[derive(Debug)]
pub enum UpdateError {
    NotFound,
    DisabledByCli,
    Persist(std::io::Error),
}

#[derive(Debug)]
pub enum DispatchOutcome<T> {
    Continue(T),
    Reject(HookRejection),
}

#[derive(Debug)]
pub struct HookRejection {
    extension_id: &'static str,
    instance_id: String,
    rejection: yabane_extension_api::ExtensionRejection,
}

#[derive(Default)]
pub struct RequestHooks<'a> {
    pub upstream_request: Vec<&'a dyn UpstreamRequestHook>,
    pub upstream_headers: Vec<&'a dyn UpstreamHeadersHook>,
}

pub struct ExtensionRegistry {
    extensions: Vec<ExtensionEntry>,
    settings: Mutex<ExtensionSettings>,
    settings_path: PathBuf,
    disabled_by_cli: bool,
}

impl ExtensionRegistry {
    pub async fn built_in(disabled_by_cli: bool) -> Result<Self, String> {
        let infos: Vec<ExtensionInfo> = vec![
            #[cfg(feature = "extension-request-defaults")]
            extension_info(yabane_extension_request_defaults::metadata())?,
        ];
        let settings = load_settings(EXTENSIONS_FILE).await?;
        Self::new(infos, settings, EXTENSIONS_FILE.into(), disabled_by_cli)
    }

    fn new(
        infos: Vec<ExtensionInfo>,
        settings: ExtensionSettings,
        settings_path: PathBuf,
        disabled_by_cli: bool,
    ) -> Result<Self, String> {
        let mut ids = HashSet::new();
        for extension in &infos {
            if !ids.insert(extension.id) {
                return Err(format!("duplicate extension ID '{}'", extension.id));
            }
        }
        let extensions = infos
            .into_iter()
            .map(|info| ExtensionEntry {
                enabled: AtomicBool::new(settings.enabled.get(info.id).copied().unwrap_or(true)),
                info,
            })
            .collect();
        Ok(Self {
            extensions,
            settings: Mutex::new(settings),
            settings_path,
            disabled_by_cli,
        })
    }

    pub fn views(&self) -> Vec<ExtensionView> {
        self.extensions
            .iter()
            .map(|extension| ExtensionView {
                id: extension.info.id,
                name: extension.info.name,
                version: extension.info.version,
                api_version: extension.info.api_version,
                description: extension.info.description,
                implementation: "native_rust",
                included: true,
                enabled: !self.disabled_by_cli && extension.enabled.load(Ordering::Acquire),
                runtime_configurable: !self.disabled_by_cli,
                hooks: extension.info.hooks.clone(),
            })
            .collect()
    }

    #[cfg_attr(not(feature = "extension-request-defaults"), allow(dead_code))]
    pub fn is_enabled(&self, id: &str) -> bool {
        !self.disabled_by_cli
            && self
                .extensions
                .iter()
                .find(|extension| extension.info.id == id)
                .is_some_and(|extension| extension.enabled.load(Ordering::Acquire))
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<ExtensionView, UpdateError> {
        if self.disabled_by_cli {
            return Err(UpdateError::DisabledByCli);
        }
        let extension = self
            .extensions
            .iter()
            .find(|extension| extension.info.id == id)
            .ok_or(UpdateError::NotFound)?;
        let mut settings = self.settings.lock().await;
        let mut updated = settings.clone();
        updated.enabled.insert(id.to_owned(), enabled);
        crate::storage::write_json_atomic(&self.settings_path, &updated)
            .await
            .map_err(UpdateError::Persist)?;
        *settings = updated;
        extension.enabled.store(enabled, Ordering::Release);
        Ok(self
            .views()
            .into_iter()
            .find(|view| view.id == id)
            .expect("updated extension remains registered"))
    }

    pub fn run_upstream_request(
        &self,
        context: &RequestContext<'_>,
        mut body: Bytes,
        hooks: &[&dyn UpstreamRequestHook],
    ) -> Result<DispatchOutcome<Bytes>, HookFailure> {
        for hook in hooks {
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook.call(context, body)))
                    .map_err(|_| HookFailure::panicked(hook.extension_id(), hook.instance_id()))?
                    .map_err(|source| {
                        HookFailure::from_error(hook.extension_id(), hook.instance_id(), source)
                    })?;
            body = match outcome {
                HookOutcome::Continue(body) => body,
                HookOutcome::Reject(rejection) => {
                    return Ok(DispatchOutcome::Reject(HookRejection {
                        extension_id: hook.extension_id(),
                        instance_id: hook.instance_id().to_owned(),
                        rejection,
                    }));
                }
            };
        }
        Ok(DispatchOutcome::Continue(body))
    }

    pub fn run_upstream_headers(
        &self,
        context: &RequestContext<'_>,
        hooks: &[&dyn UpstreamHeadersHook],
    ) -> Result<DispatchOutcome<HeaderMap>, HookFailure> {
        let mut overlay = HeaderMap::new();
        for hook in hooks {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                hook.call(context, &mut overlay)
            }))
            .map_err(|_| HookFailure::panicked(hook.extension_id(), hook.instance_id()))?
            .map_err(|source| {
                HookFailure::from_error(hook.extension_id(), hook.instance_id(), source)
            })?;
            match outcome {
                HookOutcome::Continue(()) => {}
                HookOutcome::Reject(rejection) => {
                    return Ok(DispatchOutcome::Reject(HookRejection {
                        extension_id: hook.extension_id(),
                        instance_id: hook.instance_id().to_owned(),
                        rejection,
                    }));
                }
            }
            validate_header_overlay(&overlay).map_err(|source| {
                HookFailure::from_error(hook.extension_id(), hook.instance_id(), source)
            })?;
        }
        Ok(DispatchOutcome::Continue(overlay))
    }
}

async fn load_settings(path: impl AsRef<Path>) -> Result<ExtensionSettings, String> {
    match tokio::fs::read(path).await {
        Ok(contents) => serde_json::from_slice(&contents)
            .map_err(|error| format!("parse extension settings: {error}")),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(ExtensionSettings::default()),
        Err(error) => Err(format!("read extension settings: {error}")),
    }
}

#[cfg(any(feature = "extension-request-defaults", test))]
fn extension_info(extension: Extension) -> Result<ExtensionInfo, String> {
    if extension.id.is_empty() {
        return Err("extension ID cannot be empty".to_owned());
    }
    if extension.api_version != EXTENSION_API_VERSION {
        return Err(format!(
            "extension '{}' requires API v{}, but this build supports v{}",
            extension.id, extension.api_version, EXTENSION_API_VERSION
        ));
    }
    let mut hooks = Vec::with_capacity(extension.hooks.len());
    for hook in extension.hooks {
        let hook = hook.as_str();
        if hooks.contains(&hook) {
            return Err(format!(
                "extension '{}' declares duplicate Hook '{hook}'",
                extension.id
            ));
        }
        hooks.push(hook);
    }
    Ok(ExtensionInfo {
        id: extension.id,
        name: extension.name,
        version: extension.version,
        api_version: extension.api_version,
        description: extension.description,
        hooks,
    })
}

fn validate_header_overlay(headers: &HeaderMap) -> Result<(), ExtensionError> {
    if let Some(name) = headers.keys().find(|name| reserved_header(name.as_str())) {
        return Err(ExtensionError::new(
            "reserved_header",
            format!("extension attempted to modify Core-managed header '{name}'"),
        ));
    }
    Ok(())
}

fn reserved_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "authorization"
            | "x-api-key"
            | "cookie"
            | "set-cookie"
            | "content-length"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "chatgpt-account-id"
            | "originator"
    )
}

#[derive(Debug)]
pub struct HookFailure {
    extension_id: &'static str,
    instance_id: String,
    source: ExtensionError,
}

impl HookFailure {
    fn from_error(extension_id: &'static str, instance_id: &str, source: ExtensionError) -> Self {
        Self {
            extension_id,
            instance_id: instance_id.to_owned(),
            source,
        }
    }

    fn panicked(extension_id: &'static str, instance_id: &str) -> Self {
        Self::from_error(
            extension_id,
            instance_id,
            ExtensionError::new("hook_panicked", "Hook panicked"),
        )
    }
}

impl std::fmt::Display for HookFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "extension '{}' instance '{}' failed: {}",
            self.extension_id, self.instance_id, self.source
        )
    }
}

pub fn rejection_response(rejection: HookRejection) -> axum::response::Response {
    tracing::info!(
        extension = rejection.extension_id,
        instance = %rejection.instance_id,
        rejection_code = rejection.rejection.code,
        "request rejected by extension"
    );
    crate::error::api_error_with_type(
        rejection.rejection.status,
        rejection.rejection.message,
        rejection.rejection.code,
    )
}

pub fn execution_error(failure: HookFailure) -> axum::response::Response {
    tracing::error!(
        extension = failure.extension_id,
        instance = %failure.instance_id,
        error_code = failure.source.code,
        "request extension failed"
    );
    crate::error::api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Request extension '{}' failed", failure.extension_id),
    )
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use yabane_extension_api::{
        EXTENSION_API_VERSION, Extension, ExtensionError, ExtensionHook, ExtensionRejection,
        HookOutcome, HookStage, Protocol, RequestContext, UpstreamHeadersHook, UpstreamRequestHook,
    };

    use super::{
        DispatchOutcome, ExtensionRegistry, ExtensionSettings, RequestHooks, extension_info,
    };

    struct Append {
        id: &'static str,
        value: &'static [u8],
    }

    impl ExtensionHook for Append {
        fn extension_id(&self) -> &'static str {
            self.id
        }
    }

    impl UpstreamRequestHook for Append {
        fn call(
            &self,
            _context: &RequestContext<'_>,
            mut body: Bytes,
        ) -> Result<HookOutcome<Bytes>, ExtensionError> {
            let mut value = body.to_vec();
            value.extend_from_slice(self.value);
            body = Bytes::from(value);
            Ok(HookOutcome::Continue(body))
        }
    }

    struct Reject;

    impl ExtensionHook for Reject {
        fn extension_id(&self) -> &'static str {
            "reject"
        }
    }

    impl UpstreamRequestHook for Reject {
        fn call(
            &self,
            _context: &RequestContext<'_>,
            _body: Bytes,
        ) -> Result<HookOutcome<Bytes>, ExtensionError> {
            Ok(HookOutcome::Reject(ExtensionRejection::new(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "test_rejection",
                "blocked",
            )))
        }
    }

    struct Panic;

    impl ExtensionHook for Panic {
        fn extension_id(&self) -> &'static str {
            "panic"
        }
    }

    impl UpstreamRequestHook for Panic {
        fn call(
            &self,
            _context: &RequestContext<'_>,
            _body: Bytes,
        ) -> Result<HookOutcome<Bytes>, ExtensionError> {
            panic!("sensitive panic detail")
        }
    }

    struct ReservedHeader;

    impl ExtensionHook for ReservedHeader {
        fn extension_id(&self) -> &'static str {
            "reserved-header"
        }
    }

    impl UpstreamHeadersHook for ReservedHeader {
        fn call(
            &self,
            _context: &RequestContext<'_>,
            headers: &mut axum::http::HeaderMap,
        ) -> Result<HookOutcome<()>, ExtensionError> {
            headers.insert(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_static("Bearer leaked"),
            );
            Ok(HookOutcome::Continue(()))
        }
    }

    fn registry() -> ExtensionRegistry {
        ExtensionRegistry::new(
            Vec::new(),
            ExtensionSettings::default(),
            std::env::temp_dir().join("unused-extension-settings.json"),
            false,
        )
        .unwrap()
    }

    fn context() -> RequestContext<'static> {
        RequestContext {
            request_id: "req-test",
            public_model: "provider/model",
            upstream_model: "model",
            provider_id: "provider",
            endpoint_id: "endpoint",
            caller_protocol: Protocol::OpenAiChatCompletions,
            upstream_protocol: Protocol::OpenAiChatCompletions,
            requested_streaming: false,
        }
    }

    #[test]
    fn request_hooks_chain_in_declared_order() {
        let first = Append {
            id: "first",
            value: b"-first",
        };
        let second = Append {
            id: "second",
            value: b"-second",
        };
        let DispatchOutcome::Continue(body) = registry()
            .run_upstream_request(&context(), Bytes::from_static(b"body"), &[&first, &second])
            .unwrap()
        else {
            panic!("request unexpectedly rejected")
        };
        assert_eq!(body, "body-first-second");
    }

    #[test]
    fn rejection_stops_later_hooks() {
        let reject = Reject;
        let later = Append {
            id: "later",
            value: b"-must-not-run",
        };
        let outcome = registry()
            .run_upstream_request(&context(), Bytes::from_static(b"body"), &[&reject, &later])
            .unwrap();
        assert!(matches!(outcome, DispatchOutcome::Reject(_)));
    }

    #[test]
    fn hook_panic_is_contained_and_attributed() {
        let failure = registry()
            .run_upstream_request(&context(), Bytes::from_static(b"body"), &[&Panic])
            .unwrap_err();
        assert_eq!(failure.extension_id, "panic");
        assert_eq!(failure.instance_id, "panic");
        assert_eq!(failure.source.code, "hook_panicked");
        assert!(!failure.source.message.contains("sensitive"));
    }

    #[test]
    fn reserved_header_failure_is_attributed_to_the_hook() {
        let failure = registry()
            .run_upstream_headers(&context(), &[&ReservedHeader])
            .unwrap_err();
        assert_eq!(failure.extension_id, "reserved-header");
        assert_eq!(failure.instance_id, "reserved-header");
        assert_eq!(failure.source.code, "reserved_header");
    }

    #[test]
    fn metadata_drives_declared_hook_stages() {
        let extension = extension_info(Extension {
            id: "test",
            name: "Test",
            version: "1.0.0",
            api_version: EXTENSION_API_VERSION,
            description: "test extension",
            hooks: &[HookStage::UpstreamRequest, HookStage::UpstreamHeaders],
        })
        .unwrap();
        assert_eq!(extension.hooks, ["upstream_request", "upstream_headers"]);
    }

    #[test]
    fn incompatible_api_version_fails_loading() {
        let error = extension_info(Extension {
            id: "test",
            name: "Test",
            version: "1.0.0",
            api_version: EXTENSION_API_VERSION + 1,
            description: "test extension",
            hooks: &[],
        })
        .unwrap_err();
        assert!(error.contains("requires API v"));
    }

    #[test]
    fn duplicate_hook_declarations_fail_loading() {
        let error = extension_info(Extension {
            id: "test",
            name: "Test",
            version: "1.0.0",
            api_version: EXTENSION_API_VERSION,
            description: "test extension",
            hooks: &[HookStage::UpstreamRequest, HookStage::UpstreamRequest],
        })
        .unwrap_err();
        assert!(error.contains("duplicate Hook"));
    }

    #[test]
    fn persisted_and_cli_states_control_effective_enablement() {
        let info = extension_info(Extension {
            id: "test",
            name: "Test",
            version: "1.0.0",
            api_version: EXTENSION_API_VERSION,
            description: "test extension",
            hooks: &[],
        })
        .unwrap();
        let settings = ExtensionSettings {
            enabled: [("test".to_owned(), false)].into_iter().collect(),
        };
        let persisted_disabled =
            ExtensionRegistry::new(vec![info.clone()], settings, "unused.json".into(), false)
                .unwrap();
        assert!(!persisted_disabled.is_enabled("test"));

        let cli_disabled = ExtensionRegistry::new(
            vec![info],
            ExtensionSettings::default(),
            "unused.json".into(),
            true,
        )
        .unwrap();
        assert!(!cli_disabled.is_enabled("test"));
        assert!(!cli_disabled.views()[0].runtime_configurable);
    }

    #[tokio::test]
    async fn enabling_state_is_persisted_before_becoming_effective() {
        let path = std::env::temp_dir().join(format!(
            "yabane-extension-settings-{}.json",
            std::process::id()
        ));
        let _ = tokio::fs::remove_file(&path).await;
        let info = extension_info(Extension {
            id: "test",
            name: "Test",
            version: "1.0.0",
            api_version: EXTENSION_API_VERSION,
            description: "test extension",
            hooks: &[],
        })
        .unwrap();
        let registry = ExtensionRegistry::new(
            vec![info],
            ExtensionSettings::default(),
            path.clone(),
            false,
        )
        .unwrap();

        registry.set_enabled("test", false).await.unwrap();
        assert!(!registry.is_enabled("test"));
        let stored: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(stored["enabled"]["test"], false);
        tokio::fs::remove_file(path).await.unwrap();
    }

    #[test]
    fn zero_hook_snapshot_is_empty() {
        let hooks = RequestHooks::default();
        assert!(hooks.upstream_request.is_empty());
        assert!(hooks.upstream_headers.is_empty());
    }
}

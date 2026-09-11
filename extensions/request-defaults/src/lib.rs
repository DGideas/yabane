use std::collections::HashMap;

use bytes::Bytes;
use yabane_extension_api::{
    EXTENSION_API_VERSION, Extension, ExtensionError, ExtensionHook, HookOutcome, HookStage,
    RequestContext, UpstreamHeadersHook, UpstreamRequestHook,
};

pub const ID: &str = "request-defaults";

pub struct RequestDefaults<'a> {
    provider_headers: &'a HashMap<String, String>,
    endpoint_headers: &'a HashMap<String, String>,
    provider_body: &'a serde_json::Map<String, serde_json::Value>,
    endpoint_body: &'a serde_json::Map<String, serde_json::Value>,
}

impl<'a> RequestDefaults<'a> {
    pub fn new(
        provider_headers: &'a HashMap<String, String>,
        endpoint_headers: &'a HashMap<String, String>,
        provider_body: &'a serde_json::Map<String, serde_json::Value>,
        endpoint_body: &'a serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self {
            provider_headers,
            endpoint_headers,
            provider_body,
            endpoint_body,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.provider_headers.is_empty()
            && self.endpoint_headers.is_empty()
            && self.provider_body.is_empty()
            && self.endpoint_body.is_empty()
    }
}

impl ExtensionHook for RequestDefaults<'_> {
    fn extension_id(&self) -> &'static str {
        ID
    }
}

impl UpstreamHeadersHook for RequestDefaults<'_> {
    fn call(
        &self,
        _context: &RequestContext<'_>,
        headers: &mut http::HeaderMap,
    ) -> Result<HookOutcome<()>, ExtensionError> {
        for (name, value) in self
            .provider_headers
            .iter()
            .chain(self.endpoint_headers.iter())
        {
            let name = http::HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
                ExtensionError::new(
                    "invalid_header_name",
                    format!("invalid configured header '{name}': {error}"),
                )
            })?;
            let value = http::HeaderValue::from_str(value).map_err(|error| {
                ExtensionError::new(
                    "invalid_header_value",
                    format!("invalid configured value for '{name}': {error}"),
                )
            })?;
            headers.insert(name, value);
        }
        Ok(HookOutcome::Continue(()))
    }
}

impl UpstreamRequestHook for RequestDefaults<'_> {
    fn call(
        &self,
        _context: &RequestContext<'_>,
        body: Bytes,
    ) -> Result<HookOutcome<Bytes>, ExtensionError> {
        if self.provider_body.is_empty() && self.endpoint_body.is_empty() {
            return Ok(HookOutcome::Continue(body));
        }
        let mut value: serde_json::Value = serde_json::from_slice(&body).map_err(|error| {
            ExtensionError::new(
                "invalid_request_json",
                format!("request body is not valid JSON: {error}"),
            )
        })?;
        let object = value.as_object_mut().ok_or_else(|| {
            ExtensionError::new("invalid_request_json", "request body must be a JSON object")
        })?;
        for (key, value) in self.provider_body.iter().chain(self.endpoint_body.iter()) {
            object.insert(key.clone(), value.clone());
        }
        let body = serde_json::to_vec(&value)
            .map(Bytes::from)
            .map_err(|error| {
                ExtensionError::new(
                    "request_serialization_failed",
                    format!("could not serialize modified request: {error}"),
                )
            })?;
        Ok(HookOutcome::Continue(body))
    }
}

pub fn metadata() -> Extension {
    Extension {
        id: ID,
        name: "Request Defaults",
        version: env!("CARGO_PKG_VERSION"),
        api_version: EXTENSION_API_VERSION,
        description: "Adds explicitly configured Provider and Endpoint headers and JSON body fields after routing.",
        hooks: &[HookStage::UpstreamRequest, HookStage::UpstreamHeaders],
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bytes::Bytes;
    use yabane_extension_api::{
        HookOutcome, Protocol, RequestContext, UpstreamHeadersHook, UpstreamRequestHook,
    };

    use super::RequestDefaults;

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
    fn endpoint_values_override_provider_values() {
        let provider_headers = HashMap::from([("x-region".to_owned(), "global".to_owned())]);
        let endpoint_headers = HashMap::from([("x-region".to_owned(), "endpoint".to_owned())]);
        let provider_body =
            serde_json::Map::from_iter([("temperature".to_owned(), serde_json::json!(0.2))]);
        let endpoint_body =
            serde_json::Map::from_iter([("temperature".to_owned(), serde_json::json!(0.7))]);
        let extension = RequestDefaults::new(
            &provider_headers,
            &endpoint_headers,
            &provider_body,
            &endpoint_body,
        );
        let mut headers = http::HeaderMap::new();
        assert!(matches!(
            UpstreamHeadersHook::call(&extension, &context(), &mut headers).unwrap(),
            HookOutcome::Continue(())
        ));
        assert_eq!(headers["x-region"], "endpoint");

        let HookOutcome::Continue(body) = UpstreamRequestHook::call(
            &extension,
            &context(),
            Bytes::from_static(br#"{"model":"model"}"#),
        )
        .unwrap() else {
            panic!("request unexpectedly rejected")
        };
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["temperature"], 0.7);
    }
}

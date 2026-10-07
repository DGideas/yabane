use crate::protocol::Protocol;

const MAX_JSON_USAGE_BODY: usize = 1024 * 1024;
/// Usage observation must not grow with traffic it cannot parse: a line this long
/// is dropped instead of being buffered until the Provider ends the response.
const MAX_PENDING_USAGE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub cost: Option<f64>,
    pub finish_reason: Option<String>,
}

pub struct UsageTracker {
    protocol: Protocol,
    payload: Payload,
    usage: TokenUsage,
    protocol_failed: bool,
}

enum Payload {
    Json { body: Vec<u8>, overflowed: bool },
    EventStream(EventStreamDecoder),
}

#[derive(Default)]
struct EventStreamDecoder {
    pending: Vec<u8>,
    data: Vec<u8>,
    discard_event: bool,
    /// True while skipping the remainder of a line that exceeded the cap.
    discarding_line: bool,
}

impl UsageTracker {
    pub fn new(protocol: Protocol, event_stream: bool) -> Self {
        Self {
            protocol,
            payload: if event_stream {
                Payload::EventStream(EventStreamDecoder::default())
            } else {
                Payload::Json {
                    body: Vec::new(),
                    overflowed: false,
                }
            },
            usage: TokenUsage::default(),
            protocol_failed: false,
        }
    }

    pub fn observe(&mut self, chunk: &[u8]) {
        match &mut self.payload {
            Payload::Json { body, overflowed } => {
                if !*overflowed && body.len().saturating_add(chunk.len()) <= MAX_JSON_USAGE_BODY {
                    body.extend_from_slice(chunk);
                } else {
                    body.clear();
                    *overflowed = true;
                }
            }
            Payload::EventStream(decoder) => {
                for event in decoder.push(chunk) {
                    self.protocol_failed |= event_reports_failure(self.protocol, &event);
                    merge_event_usage(self.protocol, &event, &mut self.usage);
                }
            }
        }
    }

    /// Finishes observation and returns what was seen. Taking `&mut self` lets the
    /// proxy record a caller that disconnected mid-stream from a drop handler just
    /// like a completed exchange.
    pub fn finish(&mut self) -> (TokenUsage, bool) {
        let payload = std::mem::replace(
            &mut self.payload,
            Payload::Json {
                body: Vec::new(),
                overflowed: false,
            },
        );
        match payload {
            Payload::Json { body, overflowed } => {
                if !overflowed {
                    self.protocol_failed |= event_reports_failure(self.protocol, &body);
                    merge_event_usage(self.protocol, &body, &mut self.usage);
                }
            }
            Payload::EventStream(mut decoder) => {
                for event in decoder.finish() {
                    self.protocol_failed |= event_reports_failure(self.protocol, &event);
                    merge_event_usage(self.protocol, &event, &mut self.usage);
                }
            }
        }
        (std::mem::take(&mut self.usage), self.protocol_failed)
    }
}

impl EventStreamDecoder {
    fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(chunk);
        self.consume_lines(false)
    }

    fn finish(&mut self) -> Vec<Vec<u8>> {
        self.consume_lines(true)
    }

    fn consume_lines(&mut self, finish: bool) -> Vec<Vec<u8>> {
        let mut events = Vec::new();
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            if self.discarding_line {
                // The over-long line just ended; drop it and keep reading normally.
                // Its event is still discarded: the data already collected for it
                // was cleared when the line was dropped.
                self.pending.drain(..=newline);
                self.discarding_line = false;
                self.discard_event = false;
                continue;
            }
            let mut line = self.pending.drain(..=newline).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.observe_line(&line, &mut events);
        }
        if finish {
            if !self.pending.is_empty() {
                let mut line = std::mem::take(&mut self.pending);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.observe_line(&line, &mut events);
            }
            self.dispatch(&mut events);
        }
        self.trim_pending();
        events
    }

    /// A line longer than the cap cannot belong to a usage event, so drop it and
    /// the event it belongs to rather than buffering traffic that cannot be read.
    fn trim_pending(&mut self) {
        if self.pending.len() > MAX_PENDING_USAGE_BYTES {
            self.pending.clear();
            self.discarding_line = true;
            self.data.clear();
            self.discard_event = true;
        }
    }

    fn observe_line(&mut self, line: &[u8], events: &mut Vec<Vec<u8>>) {
        if line.is_empty() {
            self.dispatch(events);
            return;
        }
        let Some(value) = line.strip_prefix(b"data:") else {
            return;
        };
        let value = value.strip_prefix(b" ").unwrap_or(value);
        let separator = usize::from(!self.data.is_empty());
        if self
            .data
            .len()
            .saturating_add(separator)
            .saturating_add(value.len())
            > MAX_JSON_USAGE_BODY
        {
            self.data.clear();
            self.discard_event = true;
            return;
        }
        if !self.data.is_empty() {
            self.data.push(b'\n');
        }
        self.data.extend_from_slice(value);
    }

    fn dispatch(&mut self, events: &mut Vec<Vec<u8>>) {
        if !self.discard_event && !self.data.is_empty() && self.data != b"[DONE]" {
            events.push(std::mem::take(&mut self.data));
        } else {
            self.data.clear();
        }
        self.discard_event = false;
    }
}

fn event_reports_failure(protocol: Protocol, bytes: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    match protocol {
        Protocol::OpenAiChat | Protocol::OpenAiResponses => {
            // A streaming event carries the response under `response`, while a
            // non-streaming body is the response object itself, so its terminal
            // status is a top-level field. Both are the same protocol failure.
            // PROXY-48: incomplete is a generation outcome (e.g. token limit),
            // not a Provider failure; preserve its reason without forcing 502.
            let response_status = value
                .pointer("/response/status")
                .or_else(|| value.get("status"))
                .and_then(serde_json::Value::as_str);
            matches!(
                value.get("type").and_then(serde_json::Value::as_str),
                Some("error" | "response.failed")
            ) || response_status == Some("failed")
                || value.get("error").is_some_and(|error| !error.is_null())
        }
        Protocol::AnthropicMessages => {
            value.get("type").and_then(serde_json::Value::as_str) == Some("error")
        }
    }
}

fn merge_event_usage(protocol: Protocol, bytes: &[u8], combined: &mut TokenUsage) {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return;
    };
    if combined.finish_reason.is_none() {
        combined.finish_reason = extract_finish_reason(protocol, &value);
    }
    if let Some(cost) = first_f64(
        &value,
        &[
            &["response", "usage", "cost"],
            &["response", "usage", "total_cost"],
            &["usage", "cost"],
            &["usage", "total_cost"],
            &["cost"],
        ],
    ) {
        combined.cost = Some(combined.cost.unwrap_or(0.0).max(cost));
    }
    let usage = match protocol {
        Protocol::OpenAiChat | Protocol::OpenAiResponses => value
            .pointer("/response/usage")
            .or_else(|| value.get("usage")),
        Protocol::AnthropicMessages => value
            .pointer("/message/usage")
            .or_else(|| value.get("usage")),
    };
    let Some(usage) = usage else {
        return;
    };

    let input = first_u64(usage, &[&["input_tokens"], &["prompt_tokens"]]);
    let output = first_u64(usage, &[&["output_tokens"], &["completion_tokens"]]);
    let cached = first_u64(
        usage,
        &[
            &["cache_read_input_tokens"],
            &["cached_tokens"],
            &["prompt_tokens_details", "cached_tokens"],
            &["input_tokens_details", "cached_tokens"],
        ],
    );
    let normalized_input = match protocol {
        Protocol::AnthropicMessages => input.saturating_add(cached),
        _ => input,
    };
    combined.input = combined.input.max(normalized_input);
    combined.output = combined.output.max(output);
    combined.cached = combined.cached.max(cached);
}

fn extract_finish_reason(protocol: Protocol, value: &serde_json::Value) -> Option<String> {
    let chat_reason = || {
        value
            .get("choices")
            .and_then(serde_json::Value::as_array)
            .and_then(|choices| {
                choices.iter().find_map(|choice| {
                    choice
                        .get("finish_reason")
                        .and_then(serde_json::Value::as_str)
                })
            })
    };
    let responses_reason = || {
        let response = value.get("response").unwrap_or(value);
        response
            .get("stop_reason")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                response
                    .pointer("/incomplete_details/reason")
                    .and_then(serde_json::Value::as_str)
            })
            .or_else(|| {
                response
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .filter(|status| matches!(*status, "completed" | "incomplete" | "failed"))
            })
            .or_else(|| {
                value
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|event_type| event_type.strip_prefix("response."))
                    .filter(|status| matches!(*status, "completed" | "incomplete" | "failed"))
            })
    };
    let reason = match protocol {
        Protocol::OpenAiChat => chat_reason(),
        Protocol::OpenAiResponses => responses_reason(),
        Protocol::AnthropicMessages => value
            .pointer("/delta/stop_reason")
            .or_else(|| value.get("stop_reason"))
            .and_then(serde_json::Value::as_str),
    }?;
    (!reason.is_empty() && reason.len() <= 128 && !reason.chars().any(char::is_control))
        .then(|| reason.to_owned())
}

fn first_f64(value: &serde_json::Value, paths: &[&[&str]]) -> Option<f64> {
    paths.iter().find_map(|path| {
        let value = path
            .iter()
            .try_fold(value, |current, segment| current.get(segment))?;
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
            .filter(|value| value.is_finite() && *value >= 0.0)
    })
}

fn first_u64(value: &serde_json::Value, paths: &[&[&str]]) -> u64 {
    paths
        .iter()
        .find_map(|path| {
            path.iter()
                .try_fold(value, |current, segment| current.get(segment))
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{TokenUsage, UsageTracker};
    use crate::protocol::Protocol;

    /// Usage observation follows a stream that may be arbitrarily long, so a line
    /// longer than the cap is dropped instead of buffered, and later events are
    /// still read normally.
    #[test]
    fn an_overlong_streaming_line_is_dropped_without_losing_later_usage() {
        let mut tracker = UsageTracker::new(Protocol::OpenAiChat, true);
        tracker.observe(&vec![b'x'; super::MAX_PENDING_USAGE_BYTES + 1]);
        if let super::Payload::EventStream(decoder) = &tracker.payload {
            assert!(
                decoder.pending.is_empty(),
                "the over-long line is not buffered"
            );
            assert!(decoder.discarding_line);
        } else {
            panic!("expected an event-stream decoder");
        }
        tracker.observe(b"\n");
        tracker.observe(b"data: {\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}\n\n");
        let (usage, failed) = tracker.finish();
        assert_eq!(usage.input, 7);
        assert_eq!(usage.output, 3);
        assert!(!failed);
    }

    #[test]
    fn extracts_openai_chat_json_usage() {
        let mut tracker = UsageTracker::new(Protocol::OpenAiChat, false);
        tracker.observe(br#"{"usage":{"prompt_tokens":12,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":3}}}"#);
        assert_eq!(
            tracker.finish().0,
            TokenUsage {
                input: 12,
                output: 4,
                cached: 3,
                cost: None,
                finish_reason: None,
            }
        );
    }

    #[test]
    fn extracts_anthropic_usage_with_cache_as_total_input() {
        let mut tracker = UsageTracker::new(Protocol::AnthropicMessages, false);
        tracker.observe(br#"{"usage":{"input_tokens":1835,"output_tokens":2976,"cache_read_input_tokens":23552}}"#);
        assert_eq!(
            tracker.finish().0,
            TokenUsage {
                input: 25387,
                output: 2976,
                cached: 23552,
                cost: None,
                finish_reason: None,
            }
        );
    }

    #[test]
    fn extracts_numeric_or_string_upstream_cost() {
        let mut tracker = UsageTracker::new(Protocol::OpenAiChat, false);
        tracker
            .observe(br#"{"usage":{"prompt_tokens":12,"completion_tokens":4,"cost":"0.00125"}}"#);
        assert_eq!(tracker.finish().0.cost, Some(0.00125));
    }

    #[test]
    fn extracts_protocol_specific_finish_reasons() {
        let mut chat = UsageTracker::new(Protocol::OpenAiChat, false);
        chat.observe(br#"{"choices":[{"finish_reason":"length"}]}"#);
        assert_eq!(chat.finish().0.finish_reason.as_deref(), Some("length"));

        let mut anthropic = UsageTracker::new(Protocol::AnthropicMessages, true);
        anthropic.observe(
            b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
        );
        assert_eq!(
            anthropic.finish().0.finish_reason.as_deref(),
            Some("tool_use")
        );

        let mut responses = UsageTracker::new(Protocol::OpenAiResponses, false);
        responses.observe(
            br#"{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}"#,
        );
        assert_eq!(
            responses.finish().0.finish_reason.as_deref(),
            Some("max_output_tokens")
        );
    }

    #[test]
    fn incomplete_responses_are_not_provider_failures() {
        // PROXY-48: JSON and SSE limits keep usage and their original reason.
        for reason in ["max_output_tokens", "content_filter", "future_reason"] {
            let response = serde_json::json!({"status":"incomplete", "incomplete_details":{"reason":reason}, "usage":{"input_tokens":10,"output_tokens":5}});
            for event_stream in [false, true] {
                let body = if event_stream {
                    format!(
                        "data: {}\n\n",
                        serde_json::json!({"type":"response.incomplete","response":response})
                    )
                } else {
                    response.to_string()
                };
                let mut tracker = UsageTracker::new(Protocol::OpenAiResponses, event_stream);
                tracker.observe(body.as_bytes());
                let (usage, failed) = tracker.finish();
                assert!(!failed);
                assert_eq!(usage.finish_reason.as_deref(), Some(reason));
                assert_eq!((usage.input, usage.output), (10, 5));
            }
        }
        let mut tracker = UsageTracker::new(Protocol::OpenAiChat, true);
        tracker.observe(b"data: {\"error\":{\"message\":\"fixture\"}}\n\ndata: [DONE]\n\n");
        assert!(tracker.finish().1);
    }

    #[test]
    fn extracts_nested_responses_usage_from_split_crlf_sse() {
        let mut tracker = UsageTracker::new(Protocol::OpenAiResponses, true);
        tracker.observe(b": keepalive\r\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":20,");
        tracker.observe(b"\"output_tokens\":8,\"input_tokens_details\":{\"cached_tokens\":7}}}}\r\n\r\ndata: [DONE]\r\n\r\n");
        assert_eq!(
            tracker.finish().0,
            TokenUsage {
                input: 20,
                output: 8,
                cached: 7,
                cost: None,
                finish_reason: Some("completed".to_owned()),
            }
        );
    }

    #[test]
    fn detects_protocol_failures_without_retaining_upstream_error_text() {
        let mut responses = UsageTracker::new(Protocol::OpenAiResponses, true);
        responses.observe(b"data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"message\":\"private upstream detail\"}}}\n\n");
        assert!(responses.finish().1);

        // The same failure delivered as a non-streaming body instead of an event.
        let mut non_stream = UsageTracker::new(Protocol::OpenAiResponses, false);
        non_stream.observe(b"{\"id\":\"resp_failed\",\"object\":\"response\",\"status\":\"failed\",\"error\":{\"code\":\"server_error\",\"message\":\"private upstream detail\"},\"output\":[]}");
        assert!(non_stream.finish().1);

        // A completed response and a chat completion body are not failures.
        let mut completed = UsageTracker::new(Protocol::OpenAiResponses, false);
        completed.observe(b"{\"id\":\"resp_ok\",\"object\":\"response\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":4}}");
        assert!(!completed.finish().1);
        let mut chat = UsageTracker::new(Protocol::OpenAiChat, false);
        chat.observe(b"{\"id\":\"chatcmpl_ok\",\"object\":\"chat.completion\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"hi\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4}}");
        assert!(!chat.finish().1);

        let mut anthropic = UsageTracker::new(Protocol::AnthropicMessages, true);
        anthropic.observe(
            b"data: {\"type\":\"error\",\"error\":{\"message\":\"private upstream detail\"}}\n\n",
        );
        assert!(anthropic.finish().1);
    }

    #[test]
    fn combines_anthropic_usage_across_stream_events() {
        let mut tracker = UsageTracker::new(Protocol::AnthropicMessages, true);
        tracker.observe(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":30,\"output_tokens\":1,\"cache_read_input_tokens\":9}}}\n\n");
        tracker.observe(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":11}}\n\n");
        assert_eq!(
            tracker.finish().0,
            TokenUsage {
                input: 39,
                output: 11,
                cached: 9,
                cost: None,
                finish_reason: Some("end_turn".to_owned()),
            }
        );
    }

    #[test]
    fn supports_multiline_sse_data_fields() {
        let mut tracker = UsageTracker::new(Protocol::OpenAiChat, true);
        tracker.observe(
            b"data: {\"usage\":{\"prompt_tokens\":2,\ndata: \"completion_tokens\":1}}\n\n",
        );
        assert_eq!(
            tracker.finish().0,
            TokenUsage {
                input: 2,
                output: 1,
                cached: 0,
                cost: None,
                finish_reason: None,
            }
        );
    }
}

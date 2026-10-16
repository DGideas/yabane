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
    stream_ended: bool,
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
    /// CR is a line ending itself; ignore a following LF, even across chunks.
    skip_lf: bool,
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
            stream_ended: false,
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
                    self.stream_ended |= event_ends_stream(self.protocol, &event);
                    merge_event_usage(self.protocol, &event, &mut self.usage);
                }
            }
        }
    }

    pub fn stream_ended(&self) -> bool {
        self.stream_ended
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
        let mut events = Vec::new();
        for &byte in chunk {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if matches!(byte, b'\r' | b'\n') {
                self.skip_lf = byte == b'\r';
                if self.discarding_line {
                    self.discarding_line = false;
                    continue;
                }
                let line = std::mem::take(&mut self.pending);
                self.observe_line(&line, &mut events);
            } else if !self.discarding_line {
                if self.pending.len() == MAX_PENDING_USAGE_BYTES {
                    self.pending.clear();
                    self.discarding_line = true;
                    self.data.clear();
                    self.discard_event = true;
                } else {
                    self.pending.push(byte);
                }
            }
        }
        events
    }

    fn finish(&mut self) -> Vec<Vec<u8>> {
        let mut events = Vec::new();
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.observe_line(&line, &mut events);
        }
        self.dispatch(&mut events);
        events
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
        if !self.discard_event && !self.data.is_empty() {
            events.push(std::mem::take(&mut self.data));
        } else {
            self.data.clear();
        }
        self.discard_event = false;
    }
}

/// Only complete SSE frames observed during forwarding prove protocol completion.
/// Flushing a partial frame from a Drop handler must never invent that proof.
fn event_ends_stream(protocol: Protocol, bytes: &[u8]) -> bool {
    if protocol == Protocol::OpenAiChat && bytes == b"[DONE]" {
        return true;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    match protocol {
        Protocol::OpenAiChat => value.get("error").is_some_and(|error| !error.is_null()),
        Protocol::OpenAiResponses => matches!(
            value.get("type").and_then(serde_json::Value::as_str),
            Some("response.completed" | "response.incomplete" | "response.failed" | "error")
        ),
        Protocol::AnthropicMessages => matches!(
            value.get("type").and_then(serde_json::Value::as_str),
            Some("message_stop" | "error")
        ),
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
    // ACTIVITY-16: cache reads have no single place across upstreams. DeepSeek
    // reports prompt_cache_hit_tokens, OpenAI reports it inside the prompt-token
    // details, Anthropic reports cache_read_input_tokens, and Kimi uses a
    // top-level cached_tokens. Names take precedence over details; a top-level
    // cached_tokens is a last resort because DeepSeek-derived responses carry a
    // permanent zero there that would otherwise shadow the hit count. A field
    // that reports zero counts as unreported, so such a placeholder cannot hide
    // a later field that carries the value either.
    let cached = first_u64_nonzero(
        usage,
        &[
            &["prompt_cache_hit_tokens"],
            &["prompt_tokens_details", "cached_tokens"],
            &["input_tokens_details", "cached_tokens"],
            &["cache_read_input_tokens"],
            &["cached_tokens"],
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

/// Reads the first path a Provider response actually reports. A field that is
/// present but zero is treated as unreported so that a placeholder cannot hide a
/// later field that carries the value; a response with no cached tokens reaches
/// zero through the final fallback.
fn first_u64_nonzero(value: &serde_json::Value, paths: &[&[&str]]) -> u64 {
    paths
        .iter()
        .find_map(|path| {
            path.iter()
                .try_fold(value, |current, segment| current.get(segment))
                .and_then(serde_json::Value::as_u64)
                .filter(|value| *value > 0)
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
        tracker.observe(b"\n\n");
        tracker.observe(b"data: {\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}\n\n");
        let (usage, failed) = tracker.finish();
        assert_eq!(usage.input, 7);
        assert_eq!(usage.output, 3);
        assert!(!failed);
    }

    // PROXY-45 / PROXY-50: recognition is bounded, incremental, protocol-specific,
    // and independent of both generation finish and HTTP EOF.
    #[test]
    fn terminal_frames_survive_every_chunk_split_and_sse_line_ending() {
        for (protocol, data) in [
            (Protocol::OpenAiChat, "[DONE]"),
            (
                Protocol::OpenAiResponses,
                r#"{"type":"response.completed"}"#,
            ),
            (
                Protocol::OpenAiResponses,
                r#"{"type":"response.incomplete"}"#,
            ),
            (Protocol::AnthropicMessages, r#"{"type":"message_stop"}"#),
        ] {
            for ending in ["\n\n", "\r\n\r\n", "\r\r", "\r\n\n", "\n\r\n"] {
                let frame = format!("data: {data}{ending}");
                for split in 0..=frame.len() {
                    let mut tracker = UsageTracker::new(protocol, true);
                    tracker.observe(&frame.as_bytes()[..split]);
                    tracker.observe(&frame.as_bytes()[split..]);
                    assert!(
                        tracker.stream_ended(),
                        "{protocol:?}: {ending:?} split {split}"
                    );
                }
            }
        }
    }

    #[test]
    fn partial_frames_finish_reasons_and_foreign_markers_do_not_prove_completion() {
        for (protocol, data) in [
            (Protocol::OpenAiChat, "data: [DONE]\n"),
            (
                Protocol::OpenAiChat,
                "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
            ),
            (
                Protocol::OpenAiChat,
                "data: {\"choices\":[{\"delta\":{\"content\":\"[DONE]\"}}]}\n\n",
            ),
            (Protocol::OpenAiResponses, "data: [DONE]\n\n"),
            (
                Protocol::AnthropicMessages,
                "data: {\"type\":\"response.completed\"}\n\n",
            ),
        ] {
            let mut tracker = UsageTracker::new(protocol, true);
            tracker.observe(data.as_bytes());
            assert!(!tracker.stream_ended(), "{data}");
            tracker.finish();
            assert!(
                !tracker.stream_ended(),
                "finish must not complete a partial frame: {data}"
            );
        }
    }

    #[test]
    fn terminal_marker_never_hides_an_observed_protocol_failure() {
        for data in [
            "data: {\"error\":{\"message\":\"fixture\"}}\n\ndata: [DONE]\n\n",
            "data: [DONE]\n\ndata: {\"error\":{\"message\":\"fixture\"}}\n\n",
        ] {
            let mut tracker = UsageTracker::new(Protocol::OpenAiChat, true);
            tracker.observe(data.as_bytes());
            assert!(tracker.stream_ended());
            assert!(tracker.finish().1);
        }
    }

    #[test]
    fn overlong_event_cannot_turn_its_tail_into_a_terminal_marker() {
        let mut tracker = UsageTracker::new(Protocol::OpenAiChat, true);
        tracker.observe(b"data: ");
        tracker.observe(&vec![b'x'; super::MAX_PENDING_USAGE_BYTES + 1]);
        tracker.observe(b"\ndata: [DONE]\n\n");
        assert!(!tracker.stream_ended());
        tracker.observe(b"data: [DONE]\n\n");
        assert!(tracker.stream_ended());
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

    // ACTIVITY-16: a Provider that ships a permanent zero next to a real hit
    // count must not read as a cache miss.
    #[test]
    fn prefers_a_reported_cache_hit_count_over_a_deepseek_zero_placeholder() {
        let mut tracker = UsageTracker::new(Protocol::OpenAiChat, true);
        tracker.observe(b"data: {\"usage\":{\"prompt_tokens\":5202,\"completion_tokens\":4,\"prompt_tokens_details\":{\"cached_tokens\":4352},\"prompt_cache_hit_tokens\":4352,\"cached_tokens\":0,\"cache_read_input_tokens\":0}}\n\n");
        assert_eq!(tracker.finish().0.cached, 4352);

        // The same response without the mirrored detail: the named field still wins.
        let mut named_only = UsageTracker::new(Protocol::OpenAiChat, true);
        named_only.observe(b"data: {\"usage\":{\"prompt_tokens\":5202,\"completion_tokens\":4,\"prompt_cache_hit_tokens\":4480,\"cached_tokens\":0}}\n\n");
        assert_eq!(named_only.finish().0.cached, 4480);

        // A zero placeholder in front of a populated detail field falls through too.
        let mut placeholder_first = UsageTracker::new(Protocol::OpenAiChat, false);
        placeholder_first.observe(br#"{"usage":{"prompt_tokens":10,"completion_tokens":1,"prompt_cache_hit_tokens":0,"prompt_tokens_details":{"cached_tokens":7}}}"#);
        assert_eq!(placeholder_first.finish().0.cached, 7);

        // A top-level cached_tokens remains the only source for Providers that
        // report nothing else, including when it is zero.
        let mut top_level_only = UsageTracker::new(Protocol::OpenAiChat, false);
        top_level_only
            .observe(br#"{"usage":{"prompt_tokens":9,"completion_tokens":2,"cached_tokens":512}}"#);
        assert_eq!(top_level_only.finish().0.cached, 512);
        let mut no_cache_reported = UsageTracker::new(Protocol::OpenAiChat, false);
        no_cache_reported
            .observe(br#"{"usage":{"prompt_tokens":5,"completion_tokens":1,"cached_tokens":0}}"#);
        assert_eq!(no_cache_reported.finish().0.cached, 0);
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

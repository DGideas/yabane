use crate::config::ApiType;

const MAX_JSON_USAGE_BODY: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub cost: Option<f64>,
}

pub struct UsageTracker {
    api_type: ApiType,
    payload: Payload,
    usage: TokenUsage,
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
}

impl UsageTracker {
    pub fn new(api_type: ApiType, event_stream: bool) -> Self {
        Self {
            api_type,
            payload: if event_stream {
                Payload::EventStream(EventStreamDecoder::default())
            } else {
                Payload::Json {
                    body: Vec::new(),
                    overflowed: false,
                }
            },
            usage: TokenUsage::default(),
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
                    merge_event_usage(self.api_type, &event, &mut self.usage);
                }
            }
        }
    }

    pub fn finish(mut self) -> TokenUsage {
        match &mut self.payload {
            Payload::Json { body, overflowed } => {
                if !*overflowed {
                    merge_event_usage(self.api_type, body, &mut self.usage);
                }
            }
            Payload::EventStream(decoder) => {
                for event in decoder.finish() {
                    merge_event_usage(self.api_type, &event, &mut self.usage);
                }
            }
        }
        self.usage
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
        if !self.discard_event && !self.data.is_empty() && self.data != b"[DONE]" {
            events.push(std::mem::take(&mut self.data));
        } else {
            self.data.clear();
        }
        self.discard_event = false;
    }
}

fn merge_event_usage(api_type: ApiType, bytes: &[u8], combined: &mut TokenUsage) {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return;
    };
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
    let usage = match api_type {
        ApiType::OpenaiCompatible
        | ApiType::OpenaiChatCompletions
        | ApiType::OpenaiResponses
        | ApiType::OpenaiCodex => value
            .pointer("/response/usage")
            .or_else(|| value.get("usage")),
        ApiType::Anthropic => value
            .pointer("/message/usage")
            .or_else(|| value.get("usage")),
    };
    let Some(usage) = usage else {
        return;
    };

    combined.input = combined
        .input
        .max(first_u64(usage, &[&["input_tokens"], &["prompt_tokens"]]));
    combined.output = combined.output.max(first_u64(
        usage,
        &[&["output_tokens"], &["completion_tokens"]],
    ));
    combined.cached = combined.cached.max(first_u64(
        usage,
        &[
            &["cache_read_input_tokens"],
            &["cached_tokens"],
            &["prompt_tokens_details", "cached_tokens"],
            &["input_tokens_details", "cached_tokens"],
        ],
    ));
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
    use crate::config::ApiType;

    #[test]
    fn extracts_openai_chat_json_usage() {
        let mut tracker = UsageTracker::new(ApiType::OpenaiCompatible, false);
        tracker.observe(br#"{"usage":{"prompt_tokens":12,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":3}}}"#);
        assert_eq!(
            tracker.finish(),
            TokenUsage {
                input: 12,
                output: 4,
                cached: 3,
                cost: None,
            }
        );
    }

    #[test]
    fn extracts_numeric_or_string_upstream_cost() {
        let mut tracker = UsageTracker::new(ApiType::OpenaiCompatible, false);
        tracker
            .observe(br#"{"usage":{"prompt_tokens":12,"completion_tokens":4,"cost":"0.00125"}}"#);
        assert_eq!(tracker.finish().cost, Some(0.00125));
    }

    #[test]
    fn extracts_nested_responses_usage_from_split_crlf_sse() {
        let mut tracker = UsageTracker::new(ApiType::OpenaiCompatible, true);
        tracker.observe(b": keepalive\r\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":20,");
        tracker.observe(b"\"output_tokens\":8,\"input_tokens_details\":{\"cached_tokens\":7}}}}\r\n\r\ndata: [DONE]\r\n\r\n");
        assert_eq!(
            tracker.finish(),
            TokenUsage {
                input: 20,
                output: 8,
                cached: 7,
                cost: None,
            }
        );
    }

    #[test]
    fn combines_anthropic_usage_across_stream_events() {
        let mut tracker = UsageTracker::new(ApiType::Anthropic, true);
        tracker.observe(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":30,\"output_tokens\":1,\"cache_read_input_tokens\":9}}}\n\n");
        tracker.observe(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":11}}\n\n");
        assert_eq!(
            tracker.finish(),
            TokenUsage {
                input: 30,
                output: 11,
                cached: 9,
                cost: None,
            }
        );
    }

    #[test]
    fn supports_multiline_sse_data_fields() {
        let mut tracker = UsageTracker::new(ApiType::OpenaiCompatible, true);
        tracker.observe(
            b"data: {\"usage\":{\"prompt_tokens\":2,\ndata: \"completion_tokens\":1}}\n\n",
        );
        assert_eq!(
            tracker.finish(),
            TokenUsage {
                input: 2,
                output: 1,
                cached: 0,
                cost: None,
            }
        );
    }
}

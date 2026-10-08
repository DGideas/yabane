use std::collections::HashMap;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    limits::MAX_BUFFERED_BODY_BYTES,
    protocol::{Protocol, ResponseTermination},
};

const MAX_SSE_FRAME_SIZE: usize = 8 * 1024 * 1024;
const MAX_STREAM_ITEMS: usize = 4096;

pub struct StreamConverter {
    source: Protocol,
    target: Protocol,
    collect_output: bool,
    buffer: Vec<u8>,
    state: StreamState,
}

#[derive(Default)]
struct StreamState {
    id: String,
    model: String,
    created: u64,
    started: bool,
    text_started: bool,
    text: String,
    refusal: String,
    refusal_output_index: Option<usize>,
    finished: bool,
    generation_finished: bool,
    done: bool,
    truncated: bool,
    failure: Option<String>,
    termination: ResponseTermination,
    // Responses indices describe emitted output items, not source block indices.
    response_items: Vec<ResponseItem>,
    text_output_index: Option<usize>,
    sequence_number: u64,
    text_progress: HashMap<(u64, u64), EmittedProgress>,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    tools: HashMap<usize, ToolState>,
    /// Bytes of text, tool names, and tool arguments retained for an aggregated
    /// response. Streaming conversion keeps only the running state it needs.
    collected: usize,
}

#[derive(Default)]
struct ToolState {
    id: String,
    name: String,
    arguments: String,
    arguments_started: bool,
    argument_progress: EmittedProgress,
    started: bool,
    output_index: Option<usize>,
}

/// Bounded proof of the emitted prefix: cumulative done/terminal content may
/// append a suffix, but must never replace or duplicate bytes already sent.
#[derive(Default)]
struct EmittedProgress {
    bytes: usize,
    hash: Sha256,
}

impl EmittedProgress {
    fn record(&mut self, text: &str) {
        self.bytes += text.len();
        self.hash.update(text.as_bytes());
    }

    fn suffix<'a>(&self, text: &'a str) -> Result<&'a str, String> {
        let mismatch = || "Provider terminal content differs from streamed content".to_owned();
        let prefix = text.as_bytes().get(..self.bytes).ok_or_else(mismatch)?;
        if Sha256::digest(prefix) != self.hash.clone().finalize() {
            return Err(mismatch());
        }
        text.get(self.bytes..).ok_or_else(mismatch)
    }
}

enum ResponseItem {
    Text,
    Refusal,
    Tool(usize),
}

enum Event {
    Start,
    Text(String),
    Refusal(String),
    ToolStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolArguments {
        index: usize,
        delta: String,
    },
    Finish(ResponseTermination),
    Failure(String),
    Done,
}

impl StreamConverter {
    pub fn new(source: Protocol, target: Protocol) -> Self {
        Self {
            source,
            target,
            collect_output: target == Protocol::OpenAiResponses,
            buffer: Vec::new(),
            state: StreamState::default(),
        }
    }

    pub fn new_aggregating(source: Protocol, target: Protocol) -> Self {
        let mut converter = Self::new(source, target);
        converter.collect_output = true;
        converter
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<u8>, String> {
        self.buffer.extend_from_slice(chunk);
        let mut output = Vec::new();
        while let Some((end, delimiter)) = next_frame(&self.buffer) {
            if end > MAX_SSE_FRAME_SIZE {
                return Err("Provider SSE frame exceeds the 8 MiB conversion limit".to_owned());
            }
            let frame = self.buffer[..end].to_vec();
            self.buffer.drain(..end + delimiter);
            self.convert_frame(&frame, &mut output)?;
        }
        if self.buffer.len() > MAX_SSE_FRAME_SIZE {
            return Err("Provider SSE frame exceeds the 8 MiB conversion limit".to_owned());
        }
        Ok(output)
    }

    pub fn finish(&mut self) -> Result<Vec<u8>, String> {
        let mut output = Vec::new();
        if !self.buffer.is_empty() {
            let frame = std::mem::take(&mut self.buffer);
            self.convert_frame(&frame, &mut output)?;
        }
        if !self.state.done {
            self.render_truncation(&mut output)?;
            self.state.done = true;
            self.state.truncated = true;
        }
        Ok(output)
    }

    pub fn has_failed(&self) -> bool {
        self.state.truncated || self.state.failure.is_some()
    }

    pub fn non_stream_response(&self) -> Result<Vec<u8>, String> {
        if !self.collect_output {
            return Err("Stream converter was not configured for response aggregation".to_owned());
        }
        if self.state.truncated || !self.state.done {
            return Err("Provider stream ended before a terminal event".to_owned());
        }
        if let Some(message) = &self.state.failure {
            return Err(format!("Provider stream failed: {message}"));
        }
        let value = match self.target {
            Protocol::OpenAiResponses => terminal_response(&self.state),
            Protocol::OpenAiChat => {
                let tools: Vec<_> = sorted_tools(&self.state)
                    .into_iter()
                    .map(|(_, tool)| {
                        json!({
                            "id": tool.id, "type": "function", "function": {
                                "name": tool.name,
                                "arguments": tool.arguments
                            }
                        })
                    })
                    .collect();
                let mut message = json!({"role": "assistant", "content": self.state.text});
                if !self.state.refusal.is_empty() {
                    message["refusal"] = json!(self.state.refusal);
                }
                if !tools.is_empty() {
                    message["tool_calls"] = Value::Array(tools);
                }
                json!({
                    "id": self.state.id, "object": "chat.completion", "created": self.state.created,
                    "model": self.state.model,
                    "choices": [{"index": 0, "message": message, "finish_reason": self.state.termination.chat_reason(!self.state.tools.is_empty())?}],
                    "usage": openai_usage(&self.state)
                })
            }
            Protocol::AnthropicMessages => {
                let mut content = Vec::new();
                for item in &self.state.response_items {
                    let ResponseItem::Tool(index) = item else {
                        content.push(json!({"type": "text", "text": self.state.text}));
                        continue;
                    };
                    let tool = &self.state.tools[index];
                    let arguments = &tool.arguments;
                    let input = serde_json::from_str::<Value>(arguments).ok().filter(Value::is_object)
                        .ok_or_else(|| "Provider tool arguments cannot be represented as an Anthropic input object".to_owned())?;
                    content.push(json!({"type": "tool_use", "id": tool.id, "name": tool.name, "input": input}));
                }
                json!({
                    "id": self.state.id, "type": "message", "role": "assistant", "model": self.state.model,
                    "content": content, "stop_reason": self.state.termination.anthropic_reason(!self.state.tools.is_empty())?, "stop_sequence": null,
                    "usage": {"input_tokens": self.state.input_tokens.saturating_sub(self.state.cached_tokens), "output_tokens": self.state.output_tokens,
                        "cache_read_input_tokens": self.state.cached_tokens}
                })
            }
        };
        serde_json::to_vec(&value).map_err(|err| err.to_string())
    }

    fn convert_frame(&mut self, frame: &[u8], output: &mut Vec<u8>) -> Result<(), String> {
        if self.state.done {
            return Ok(());
        }
        let data = frame
            .split(|byte| matches!(byte, b'\n' | b'\r'))
            .filter_map(|line| {
                line.strip_prefix(b"data:").map(|data| {
                    if data.first() == Some(&b' ') {
                        &data[1..]
                    } else {
                        data
                    }
                })
            })
            .collect::<Vec<_>>();
        if data.is_empty() {
            return Ok(());
        }
        let data = data.join(&b'\n');
        if data == b"[DONE]" {
            return self.render(Event::Done, output);
        }
        let value: Value = serde_json::from_slice(&data).map_err(|_| {
            "Provider SSE data must be valid JSON for protocol conversion".to_owned()
        })?;
        let events = match self.source {
            Protocol::OpenAiChat => self.parse_chat(&value),
            Protocol::OpenAiResponses => self.parse_responses(&value)?,
            Protocol::AnthropicMessages => self.parse_anthropic(&value),
        };
        if self.state.text_progress.len() > MAX_STREAM_ITEMS {
            return Err("Provider stream exceeded the 4096 text-item conversion limit".to_owned());
        }
        if self.collect_output {
            for event in &events {
                let size = match event {
                    Event::Text(text) | Event::Refusal(text) => text.len(),
                    Event::ToolStart { id, name, .. } => id.len() + name.len(),
                    Event::ToolArguments { delta, .. } => delta.len(),
                    _ => 0,
                };
                self.state.collected = self.state.collected.saturating_add(size);
            }
            // A non-streaming caller, or a Responses terminal event, needs the
            // whole converted answer in memory. The per-frame limit does not
            // bound that total, so it shares the buffered-body ceiling instead
            // of growing without limit.
            if self.state.collected > MAX_BUFFERED_BODY_BYTES {
                return Err("Provider response exceeded the conversion limit".to_owned());
            }
        }
        for event in events {
            self.render(event, output)?;
        }
        Ok(())
    }

    fn parse_chat(&mut self, value: &Value) -> Vec<Event> {
        if let Some(error) = value.get("error") {
            return vec![Event::Failure(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Provider stream failed")
                    .to_owned(),
            )];
        }
        self.read_metadata(value, "created");
        read_chat_usage(value.get("usage"), &mut self.state);
        let mut events = vec![Event::Start];
        let delta = &value["choices"][0]["delta"];
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            events.push(Event::Text(text.to_owned()));
        }
        if let Some(text) = delta.get("refusal").and_then(Value::as_str) {
            events.push(Event::Refusal(text.to_owned()));
        }
        for call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !id.is_empty() || !name.is_empty() {
                events.push(Event::ToolStart {
                    index,
                    id: id.to_owned(),
                    name: name.to_owned(),
                });
            }
            if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str) {
                events.push(Event::ToolArguments {
                    index,
                    delta: arguments.to_owned(),
                });
            }
        }
        if let Some(reason) = value["choices"][0]
            .get("finish_reason")
            .and_then(Value::as_str)
        {
            events.push(Event::Finish(ResponseTermination::from_reason(Some(
                reason.to_owned(),
            ))));
        }
        events
    }

    fn parse_anthropic(&mut self, value: &Value) -> Vec<Event> {
        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                let message = &value["message"];
                self.read_metadata(message, "created_at");
                read_anthropic_usage(message.get("usage"), &mut self.state);
                vec![Event::Start]
            }
            Some("content_block_start") => {
                let block = &value["content_block"];
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        let mut events = vec![Event::ToolStart {
                            index,
                            id: field(block, "id"),
                            name: field(block, "name"),
                        }];
                        if let Some(input) = block.get("input").filter(|input| {
                            input.as_object().is_some_and(|object| !object.is_empty())
                        }) {
                            events.push(Event::ToolArguments {
                                index,
                                delta: input.to_string(),
                            });
                        }
                        events
                    }
                    Some("text") => vec![Event::Text(field(block, "text"))],
                    _ => Vec::new(),
                }
            }
            Some("content_block_delta") => {
                match value.pointer("/delta/type").and_then(Value::as_str) {
                    Some("text_delta") => vec![Event::Text(field(&value["delta"], "text"))],
                    Some("input_json_delta") => vec![Event::ToolArguments {
                        index: value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
                        delta: field(&value["delta"], "partial_json"),
                    }],
                    _ => Vec::new(),
                }
            }
            Some("content_block_stop") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if self
                    .state
                    .tools
                    .get(&index)
                    .is_some_and(|tool| !tool.arguments_started)
                {
                    vec![Event::ToolArguments {
                        index,
                        delta: "{}".to_owned(),
                    }]
                } else {
                    Vec::new()
                }
            }
            Some("message_delta") => {
                read_anthropic_usage(value.get("usage"), &mut self.state);
                value
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .map(|reason| {
                        Event::Finish(ResponseTermination::from_reason(Some(reason.to_owned())))
                    })
                    .into_iter()
                    .collect()
            }
            Some("message_stop") => vec![Event::Done],
            Some("error") => vec![Event::Failure(
                value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Provider stream failed")
                    .to_owned(),
            )],
            _ => Vec::new(),
        }
    }

    fn parse_responses(&mut self, value: &Value) -> Result<Vec<Event>, String> {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response = value.get("response").unwrap_or(value);
        self.read_metadata(response, "created_at");
        Ok(match kind {
            "response.created" | "response.in_progress" => vec![Event::Start],
            "response.refusal.delta" => {
                let text = field(value, "delta");
                let key = (
                    value["output_index"].as_u64().unwrap_or(0),
                    value["content_index"].as_u64().unwrap_or(0),
                );
                self.state
                    .text_progress
                    .entry(key)
                    .or_default()
                    .record(&text);
                vec![Event::Refusal(text)]
            }
            "response.refusal.done" => {
                let key = (
                    value["output_index"].as_u64().unwrap_or(0),
                    value["content_index"].as_u64().unwrap_or(0),
                );
                let progress = self.state.text_progress.entry(key).or_default();
                let text = field(value, "refusal");
                let suffix = progress.suffix(&text)?;
                progress.record(suffix);
                vec![Event::Refusal(suffix.to_owned())]
            }
            "response.output_text.delta" => {
                let text = field(value, "delta");
                let key = (
                    value["output_index"].as_u64().unwrap_or(0),
                    value["content_index"].as_u64().unwrap_or(0),
                );
                self.state
                    .text_progress
                    .entry(key)
                    .or_default()
                    .record(&text);
                vec![Event::Text(text)]
            }
            "response.output_text.done" => {
                let key = (
                    value["output_index"].as_u64().unwrap_or(0),
                    value["content_index"].as_u64().unwrap_or(0),
                );
                self.final_text(key, &field(value, "text"))?
            }
            "response.output_item.added"
                if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call") =>
            {
                let index = value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let mut events = vec![Event::ToolStart {
                    index,
                    id: value
                        .pointer("/item/call_id")
                        .or_else(|| value.pointer("/item/id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    name: value
                        .pointer("/item/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }];
                if let Some(arguments) = value
                    .pointer("/item/arguments")
                    .and_then(Value::as_str)
                    .filter(|raw| !raw.is_empty())
                {
                    events.push(Event::ToolArguments {
                        index,
                        delta: arguments.to_owned(),
                    });
                }
                events
            }
            "response.function_call_arguments.done" => {
                let index = value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                self.final_arguments(index, &field(value, "arguments"))?
            }
            "response.output_item.done"
                if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call") =>
            {
                let index = value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let item = &value["item"];
                self.validate_tool_identity(index, item)?;
                self.final_arguments(index, &field(item, "arguments"))?
            }
            "response.function_call_arguments.delta" => vec![Event::ToolArguments {
                index: value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize,
                delta: field(value, "delta"),
            }],
            "response.completed" | "response.incomplete" => {
                read_responses_usage(response.get("usage"), &mut self.state);
                let mut events = Vec::new();
                for (index, item) in response
                    .get("output")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    match item["type"].as_str() {
                        Some("message") => {
                            for (content_index, part) in item
                                .get("content")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .enumerate()
                            {
                                if part["type"] == "refusal" {
                                    let progress = self
                                        .state
                                        .text_progress
                                        .entry((index as u64, content_index as u64))
                                        .or_default();
                                    let text = field(part, "refusal");
                                    let suffix = progress.suffix(&text)?;
                                    progress.record(suffix);
                                    events.push(Event::Refusal(suffix.to_owned()));
                                }
                                if part["type"] == "output_text" {
                                    events.extend(self.final_text(
                                        (index as u64, content_index as u64),
                                        &field(part, "text"),
                                    )?);
                                }
                            }
                        }
                        Some("function_call") => {
                            if self.state.tools.contains_key(&index) {
                                self.validate_tool_identity(index, item)?;
                                events.extend(
                                    self.final_arguments(index, &field(item, "arguments"))?,
                                );
                            } else {
                                events.push(Event::ToolStart {
                                    index,
                                    id: field(item, "call_id"),
                                    name: field(item, "name"),
                                });
                                events.push(Event::ToolArguments {
                                    index,
                                    delta: field(item, "arguments"),
                                });
                            }
                        }
                        _ => {}
                    }
                }
                events.push(Event::Finish(ResponseTermination::from_responses(response)));
                events.push(Event::Done);
                events
            }
            "error" => vec![Event::Failure(
                value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Provider stream failed")
                    .to_owned(),
            )],
            "response.failed" => vec![
                Event::Failure(
                    response
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("Provider response failed")
                        .to_owned(),
                ),
                Event::Done,
            ],
            _ => Vec::new(),
        })
    }

    fn final_arguments(&self, index: usize, arguments: &str) -> Result<Vec<Event>, String> {
        let tool = self
            .state
            .tools
            .get(&index)
            .filter(|tool| tool.started)
            .ok_or_else(|| "Provider tool arguments arrived before the tool start".to_owned())?;
        // PROXY-59: retain a bounded digest, not another full argument buffer on
        // streaming-only paths. Cumulative done events must extend emitted bytes.
        let suffix = tool.argument_progress.suffix(arguments)?;
        Ok(if suffix.is_empty() {
            Vec::new()
        } else {
            vec![Event::ToolArguments {
                index,
                delta: suffix.to_owned(),
            }]
        })
    }

    fn final_text(&mut self, key: (u64, u64), text: &str) -> Result<Vec<Event>, String> {
        let progress = self.state.text_progress.entry(key).or_default();
        let suffix = progress.suffix(text)?;
        progress.record(suffix);
        Ok(if suffix.is_empty() {
            Vec::new()
        } else {
            vec![Event::Text(suffix.to_owned())]
        })
    }

    fn validate_tool_identity(&self, index: usize, item: &Value) -> Result<(), String> {
        let tool =
            self.state.tools.get(&index).ok_or_else(|| {
                "Provider tool completion arrived before the tool start".to_owned()
            })?;
        if item
            .get("call_id")
            .and_then(Value::as_str)
            .is_some_and(|id| id != tool.id)
            || item
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name != tool.name)
        {
            return Err(
                "Provider terminal tool identity differs from streamed identity".to_owned(),
            );
        }
        Ok(())
    }

    fn read_metadata(&mut self, value: &Value, created_field: &str) {
        if self.state.id.is_empty() {
            self.state.id = field(value, "id");
        }
        if self.state.model.is_empty() {
            self.state.model = field(value, "model");
        }
        if self.state.created == 0 {
            self.state.created = value
                .get(created_field)
                .and_then(Value::as_u64)
                .unwrap_or(0);
        }
    }

    fn render(&mut self, event: Event, output: &mut Vec<u8>) -> Result<(), String> {
        if self.state.done {
            return Ok(());
        }
        // PROXY-52 / PROXY-58: an empty delta cannot open a text block, nor
        // separate a tool call from its result in the client's saved history.
        if matches!(&event, Event::Text(text) | Event::Refusal(text) if text.is_empty()) {
            return Ok(());
        }
        if let Event::ToolStart { index, id, name } = &event {
            if let Some(tool) = self.state.tools.get(index) {
                if (!id.is_empty() && *id != tool.id) || (!name.is_empty() && *name != tool.name) {
                    return Err("Provider tool identity changed during streaming".to_owned());
                }
            } else if self.state.tools.len() >= MAX_STREAM_ITEMS {
                return Err(
                    "Provider stream exceeded the 4096 tool-item conversion limit".to_owned(),
                );
            }
        }
        if let Event::ToolArguments { index, delta } = &event {
            let tool = self
                .state
                .tools
                .get_mut(index)
                .filter(|tool| tool.started)
                .ok_or_else(|| {
                    "Provider tool arguments arrived before the tool start".to_owned()
                })?;
            tool.argument_progress.record(delta);
        }
        // Finish ends generation, not necessarily the stream. Chat can deliver
        // usage afterwards. Delay target terminal events until Done (PROXY-48).
        if let Event::Finish(termination) = event {
            self.state.termination = termination;
            self.state.generation_finished = true;
            return Ok(());
        }
        if matches!(event, Event::Done) && !self.state.generation_finished {
            return self.render(
                Event::Failure("Provider stream ended before a generation finish event".to_owned()),
                output,
            );
        }
        if matches!(event, Event::Done) && !self.state.finished {
            let finish = Event::Finish(self.state.termination.clone());
            match self.target {
                Protocol::OpenAiChat => self.render_chat(finish, output)?,
                Protocol::OpenAiResponses => self.render_responses(finish, output)?,
                Protocol::AnthropicMessages => self.render_anthropic(finish, output)?,
            }
        }
        if let Event::Failure(message) = event {
            self.state.failure = Some(message.clone());
            self.state.finished = true;
            self.state.done = true;
            return match self.target {
                Protocol::OpenAiChat => emit_data(
                    output,
                    &json!({"error": {"message": message, "type": "upstream_stream_error"}}),
                ),
                Protocol::AnthropicMessages => emit_event(
                    output,
                    "error",
                    &json!({"type": "error", "error": {"type": "api_error", "message": message}}),
                ),
                Protocol::OpenAiResponses => self.emit_responses_event(
                    output,
                    "response.failed",
                    json!({"type": "response.failed", "response": {"id": self.state.id, "object": "response", "status": "failed", "model": self.state.model, "error": {"code": "upstream_error", "message": message}}}),
                ),
            };
        }
        match self.target {
            Protocol::OpenAiChat => self.render_chat(event, output),
            Protocol::OpenAiResponses => self.render_responses(event, output),
            Protocol::AnthropicMessages => self.render_anthropic(event, output),
        }
    }

    fn render_truncation(&mut self, output: &mut Vec<u8>) -> Result<(), String> {
        let message = "Provider stream ended before a terminal event";
        match self.target {
            Protocol::OpenAiChat => emit_data(
                output,
                &json!({"error": {"message": message, "type": "upstream_stream_error"}}),
            ),
            Protocol::AnthropicMessages => emit_event(
                output,
                "error",
                &json!({"type": "error", "error": {"type": "api_error", "message": message}}),
            ),
            Protocol::OpenAiResponses => self.emit_responses_event(
                output,
                "response.failed",
                json!({"type": "response.failed", "response": {"id": self.state.id, "object": "response", "status": "failed", "model": self.state.model, "error": {"code": "upstream_stream_error", "message": message}}}),
            ),
        }
    }
}

impl StreamConverter {
    fn render_chat(&mut self, event: Event, output: &mut Vec<u8>) -> Result<(), String> {
        match event {
            Event::Refusal(text) => {
                self.ensure_started(output)?;
                if self.collect_output {
                    self.state.refusal.push_str(&text);
                }
                emit_data(
                    output,
                    &json!({"id":self.state.id, "object":"chat.completion.chunk", "created":self.state.created, "model":self.state.model,
                    "choices":[{"index":0, "delta":{"refusal":text}, "finish_reason":null}]}),
                )?;
            }
            Event::Start if !self.state.started => {
                self.state.started = true;
                emit_data(
                    output,
                    &json!({
                        "id": self.state.id, "object": "chat.completion.chunk", "created": self.state.created,
                        "model": self.state.model, "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null}]
                    }),
                )?;
            }
            Event::Start => {}
            Event::Text(text) => {
                self.ensure_started(output)?;
                if self.collect_output {
                    self.state.text.push_str(&text);
                }
                emit_data(
                    output,
                    &json!({
                        "id": self.state.id, "object": "chat.completion.chunk", "created": self.state.created,
                        "model": self.state.model, "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]
                    }),
                )?;
            }
            Event::ToolStart { index, id, name } => {
                self.ensure_started(output)?;
                let output_index = self.state.tools.len();
                let tool = self.state.tools.entry(index).or_default();
                if !id.is_empty() {
                    tool.id = id;
                }
                if !name.is_empty() {
                    tool.name = name;
                }
                if !tool.started {
                    tool.started = true;
                    tool.output_index = Some(output_index);
                    emit_data(
                        output,
                        &json!({
                            "id": self.state.id, "object": "chat.completion.chunk", "created": self.state.created,
                            "model": self.state.model, "choices": [{"index": 0, "delta": {"tool_calls": [{
                                "index": tool.output_index, "id": tool.id, "type": "function", "function": {"name": tool.name, "arguments": ""}
                            }]}, "finish_reason": null}]
                        }),
                    )?;
                }
            }
            Event::ToolArguments { index, delta } => {
                self.ensure_started(output)?;
                let tool = self
                    .state
                    .tools
                    .get_mut(&index)
                    .filter(|tool| tool.started)
                    .ok_or_else(|| {
                        "Provider tool arguments arrived before the tool start".to_owned()
                    })?;
                tool.arguments_started |= !delta.is_empty();
                if self.collect_output {
                    tool.arguments.push_str(&delta);
                }
                emit_data(
                    output,
                    &json!({
                        "id": self.state.id, "object": "chat.completion.chunk", "created": self.state.created,
                        "model": self.state.model, "choices": [{"index": 0, "delta": {"tool_calls": [{
                            "index": tool.output_index, "function": {"arguments": delta}
                        }]}, "finish_reason": null}]
                    }),
                )?;
            }
            Event::Finish(termination) if !self.state.finished => {
                let finish = termination.chat_reason(!self.state.tools.is_empty())?;
                self.ensure_started(output)?;
                emit_data(
                    output,
                    &json!({
                        "id": self.state.id, "object": "chat.completion.chunk", "created": self.state.created,
                        "model": self.state.model, "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
                        "usage": openai_usage(&self.state)
                    }),
                )?;
                self.state.finished = true;
            }
            Event::Finish(_) => {}
            Event::Done if !self.state.done => {
                if !self.state.finished {
                    self.render_chat(Event::Finish(self.state.termination.clone()), output)?;
                }
                output.extend_from_slice(b"data: [DONE]\n\n");
                self.state.done = true;
            }
            Event::Done => {}
            Event::Failure(_) => unreachable!("failure events are handled before target rendering"),
        }
        Ok(())
    }

    fn render_anthropic(&mut self, event: Event, output: &mut Vec<u8>) -> Result<(), String> {
        match event {
            // Anthropic has no refusal content block; retain the visible reason.
            Event::Refusal(text) => self.render_anthropic(Event::Text(text), output)?,
            Event::Start => self.ensure_anthropic_started(output)?,
            Event::Text(text) => {
                self.ensure_anthropic_started(output)?;
                if !self.state.text_started {
                    self.state.text_started = true;
                    let index = self.state.response_items.len();
                    self.state.text_output_index = Some(index);
                    self.state.response_items.push(ResponseItem::Text);
                    emit_event(
                        output,
                        "content_block_start",
                        &json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}),
                    )?;
                }
                if self.collect_output {
                    self.state.text.push_str(&text);
                }
                emit_event(
                    output,
                    "content_block_delta",
                    &json!({"type": "content_block_delta", "index": self.state.text_output_index, "delta": {"type": "text_delta", "text": text}}),
                )?;
            }
            Event::ToolStart { index, id, name } => {
                self.ensure_anthropic_started(output)?;
                let tool = self.state.tools.entry(index).or_default();
                if !id.is_empty() {
                    tool.id = id;
                }
                if !name.is_empty() {
                    tool.name = name;
                }
                if !tool.started {
                    tool.started = true;
                    tool.output_index = Some(self.state.response_items.len());
                    self.state.response_items.push(ResponseItem::Tool(index));
                    emit_event(
                        output,
                        "content_block_start",
                        &json!({
                            "type": "content_block_start", "index": tool.output_index,
                            "content_block": {"type": "tool_use", "id": tool.id, "name": tool.name, "input": {}}
                        }),
                    )?;
                }
            }
            Event::ToolArguments { index, delta } => {
                self.ensure_anthropic_started(output)?;
                let tool = self
                    .state
                    .tools
                    .get_mut(&index)
                    .filter(|tool| tool.started)
                    .ok_or_else(|| {
                        "Provider tool arguments arrived before the tool start".to_owned()
                    })?;
                tool.arguments_started |= !delta.is_empty();
                if self.collect_output {
                    tool.arguments.push_str(&delta);
                }
                emit_event(
                    output,
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta", "index": tool.output_index,
                        "delta": {"type": "input_json_delta", "partial_json": delta}
                    }),
                )?;
            }
            Event::Finish(termination) if !self.state.finished => {
                let stop = termination.anthropic_reason(!self.state.tools.is_empty())?;
                self.ensure_anthropic_started(output)?;
                for index in 0..self.state.response_items.len() {
                    emit_event(
                        output,
                        "content_block_stop",
                        &json!({"type": "content_block_stop", "index": index}),
                    )?;
                }
                emit_event(
                    output,
                    "message_delta",
                    &json!({
                        "type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null},
                        "usage": {"input_tokens": self.state.input_tokens.saturating_sub(self.state.cached_tokens),
                            "output_tokens": self.state.output_tokens, "cache_read_input_tokens": self.state.cached_tokens}
                    }),
                )?;
                self.state.finished = true;
            }
            Event::Finish(_) => {}
            Event::Done if !self.state.done => {
                if !self.state.finished {
                    self.render_anthropic(Event::Finish(self.state.termination.clone()), output)?;
                }
                emit_event(output, "message_stop", &json!({"type": "message_stop"}))?;
                self.state.done = true;
            }
            Event::Done => {}
            Event::Failure(_) => unreachable!("failure events are handled before target rendering"),
        }
        Ok(())
    }

    fn render_responses(&mut self, event: Event, output: &mut Vec<u8>) -> Result<(), String> {
        match event {
            Event::Refusal(text) => {
                self.ensure_responses_started(output)?;
                let item_id = format!("msg_refusal_{}", self.state.id);
                let index = if let Some(index) = self.state.refusal_output_index {
                    index
                } else {
                    let index = self.state.response_items.len();
                    self.state.refusal_output_index = Some(index);
                    self.state.response_items.push(ResponseItem::Refusal);
                    self.emit_responses_event(output, "response.output_item.added", json!({"type":"response.output_item.added", "output_index":index,
                        "item":{"id":item_id, "type":"message", "status":"in_progress", "role":"assistant", "content":[]}}))?;
                    self.emit_responses_event(output, "response.content_part.added", json!({"type":"response.content_part.added", "output_index":index,
                        "item_id":item_id, "content_index":0, "part":{"type":"refusal", "refusal":""}}))?;
                    index
                };
                self.state.refusal.push_str(&text);
                self.emit_responses_event(output, "response.refusal.delta", json!({"type":"response.refusal.delta", "output_index":index, "item_id":item_id, "content_index":0, "delta":text}))?;
            }
            Event::Start => self.ensure_responses_started(output)?,
            Event::Text(text) => {
                self.ensure_responses_started(output)?;
                let item_id = format!("msg_{}", self.state.id);
                if !self.state.text_started {
                    self.state.text_started = true;
                    let index = self.state.response_items.len();
                    self.state.text_output_index = Some(index);
                    self.state.response_items.push(ResponseItem::Text);
                    self.emit_responses_event(output, "response.output_item.added", json!({
                        "type": "response.output_item.added", "output_index": index,
                        "item": {"id": item_id, "type": "message", "status": "in_progress", "role": "assistant", "content": []}
                    }))?;
                    self.emit_responses_event(
                        output,
                        "response.content_part.added",
                        json!({
                            "type": "response.content_part.added", "item_id": item_id,
                            "output_index": index, "content_index": 0,
                            "part": {"type": "output_text", "text": "", "annotations": []}
                        }),
                    )?;
                }
                self.state.text.push_str(&text);
                self.emit_responses_event(output, "response.output_text.delta", json!({
                    "type": "response.output_text.delta", "item_id": item_id,
                    "output_index": self.state.text_output_index, "content_index": 0, "delta": text
                }))?;
            }
            Event::ToolStart { index, id, name } => {
                self.ensure_responses_started(output)?;
                let tool = self.state.tools.entry(index).or_default();
                if !id.is_empty() {
                    tool.id = id;
                }
                if !name.is_empty() {
                    tool.name = name;
                }
                if !tool.started {
                    tool.started = true;
                    let output_index = self.state.response_items.len();
                    tool.output_index = Some(output_index);
                    self.state.response_items.push(ResponseItem::Tool(index));
                    let event = json!({
                        "type": "response.output_item.added", "output_index": output_index,
                        "item": {"id": format!("fc_{}", tool.id), "type": "function_call", "status": "in_progress", "call_id": tool.id, "name": tool.name, "arguments": ""}
                    });
                    self.emit_responses_event(output, "response.output_item.added", event)?;
                }
            }
            Event::ToolArguments { index, delta } => {
                self.ensure_responses_started(output)?;
                let tool = self
                    .state
                    .tools
                    .get_mut(&index)
                    .filter(|tool| tool.started)
                    .ok_or_else(|| {
                        "Provider tool arguments arrived before the tool start".to_owned()
                    })?;
                tool.arguments_started |= !delta.is_empty();
                tool.arguments.push_str(&delta);
                let event = json!({
                    "type": "response.function_call_arguments.delta", "item_id": format!("fc_{}", tool.id),
                    "output_index": tool.output_index, "delta": delta
                });
                self.emit_responses_event(output, "response.function_call_arguments.delta", event)?;
            }
            Event::Finish(_) if !self.state.finished => {
                self.ensure_responses_started(output)?;
                let response = terminal_response(&self.state);
                // PROXY-49: done events and terminal output share exactly the
                // same item ordering, identity, arguments, and completion state.
                for (index, item) in response["output"]
                    .as_array()
                    .expect("output is an array")
                    .iter()
                    .enumerate()
                {
                    if item["type"] == "message" {
                        if item["content"][0]["type"] == "refusal" {
                            self.emit_responses_event(output, "response.refusal.done", json!({"type":"response.refusal.done", "item_id":item["id"],
                                "output_index":index, "content_index":0, "refusal":item["content"][0]["refusal"]}))?;
                        } else {
                            self.emit_responses_event(output, "response.output_text.done", json!({
                            "type": "response.output_text.done", "item_id": item["id"],
                            "output_index": index, "content_index": 0, "text": item["content"][0]["text"]
                        }))?;
                        }
                        self.emit_responses_event(output, "response.content_part.done", json!({
                            "type": "response.content_part.done", "item_id": item["id"],
                            "output_index": index, "content_index": 0, "part": item["content"][0]
                        }))?;
                    } else {
                        self.emit_responses_event(output, "response.function_call_arguments.done", json!({
                            "type": "response.function_call_arguments.done", "item_id": item["id"],
                            "output_index": index, "name": item["name"], "arguments": item["arguments"]
                        }))?;
                    }
                    self.emit_responses_event(
                        output,
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done", "output_index": index, "item": item
                        }),
                    )?;
                }
                let kind = if self.state.termination.responses_status() == "incomplete" {
                    "response.incomplete"
                } else {
                    "response.completed"
                };
                self.emit_responses_event(
                    output,
                    kind,
                    json!({"type": kind, "response": response}),
                )?;
                self.state.finished = true;
            }
            Event::Finish(_) => {}
            Event::Done => self.state.done = true,
            Event::Failure(_) => unreachable!("failure events are handled before target rendering"),
        }
        Ok(())
    }

    fn emit_responses_event(
        &mut self,
        output: &mut Vec<u8>,
        event: &str,
        mut value: Value,
    ) -> Result<(), String> {
        value["sequence_number"] = self.state.sequence_number.into();
        self.state.sequence_number += 1;
        emit_event(output, event, &value)
    }

    fn ensure_started(&mut self, output: &mut Vec<u8>) -> Result<(), String> {
        if !self.state.started {
            self.render_chat(Event::Start, output)?;
        }
        Ok(())
    }

    fn ensure_anthropic_started(&mut self, output: &mut Vec<u8>) -> Result<(), String> {
        if !self.state.started {
            self.state.started = true;
            emit_event(
                output,
                "message_start",
                &json!({
                    "type": "message_start", "message": {"id": self.state.id, "type": "message", "role": "assistant", "model": self.state.model,
                    "content": [], "stop_reason": null, "stop_sequence": null,
                    "usage": {"input_tokens": self.state.input_tokens.saturating_sub(self.state.cached_tokens), "output_tokens": 0, "cache_read_input_tokens": self.state.cached_tokens}}
                }),
            )?;
        }
        Ok(())
    }

    fn ensure_responses_started(&mut self, output: &mut Vec<u8>) -> Result<(), String> {
        if !self.state.started {
            self.state.started = true;
            self.emit_responses_event(
                output,
                "response.created",
                json!({"type": "response.created", "response": response_shell(&self.state, "in_progress")}),
            )?;
        }
        Ok(())
    }
}

fn emit_data(output: &mut Vec<u8>, value: &Value) -> Result<(), String> {
    output.extend_from_slice(b"data: ");
    output.extend_from_slice(&serde_json::to_vec(value).map_err(|err| err.to_string())?);
    output.extend_from_slice(b"\n\n");
    Ok(())
}

fn emit_event(output: &mut Vec<u8>, event: &str, value: &Value) -> Result<(), String> {
    output.extend_from_slice(format!("event: {event}\ndata: ").as_bytes());
    output.extend_from_slice(&serde_json::to_vec(value).map_err(|err| err.to_string())?);
    output.extend_from_slice(b"\n\n");
    Ok(())
}

fn openai_usage(state: &StreamState) -> Value {
    json!({"prompt_tokens": state.input_tokens, "completion_tokens": state.output_tokens,
        "total_tokens": state.input_tokens + state.output_tokens,
        "prompt_tokens_details": {"cached_tokens": state.cached_tokens}})
}

fn response_shell(state: &StreamState, status: &str) -> Value {
    json!({"id": state.id, "object": "response", "created_at": state.created, "status": status,
        "model": state.model, "output": [], "error": null, "incomplete_details": null})
}

fn terminal_response(state: &StreamState) -> Value {
    let status = state.termination.responses_status();
    let output = state.response_items.iter().map(|item| match item {
        ResponseItem::Text => json!({
            "id": format!("msg_{}", state.id), "type": "message", "status": status, "role": "assistant",
            "content": [{"type": "output_text", "text": state.text, "annotations": []}]
        }),
        ResponseItem::Refusal => json!({"id":format!("msg_refusal_{}", state.id), "type":"message", "status":status, "role":"assistant",
            "content":[{"type":"refusal", "refusal":state.refusal}]}),
        ResponseItem::Tool(index) => {
            let tool = &state.tools[index];
            json!({"id": format!("fc_{}", tool.id), "type": "function_call", "status": status,
                "call_id": tool.id, "name": tool.name, "arguments": tool.arguments})
        }
    }).collect();
    let mut response = response_shell(state, status);
    response["incomplete_details"] = state.termination.incomplete_details.clone();
    response["output"] = Value::Array(output);
    response["usage"] = json!({"input_tokens": state.input_tokens, "input_tokens_details": {"cached_tokens": state.cached_tokens},
        "output_tokens": state.output_tokens, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": state.input_tokens + state.output_tokens});
    response
}

fn sorted_tools(state: &StreamState) -> Vec<(usize, &ToolState)> {
    let mut tools: Vec<_> = state
        .tools
        .iter()
        .map(|(index, tool)| (*index, tool))
        .collect();
    tools.sort_by_key(|(index, tool)| tool.output_index.unwrap_or(*index));
    tools
}

fn next_frame(buffer: &[u8]) -> Option<(usize, usize)> {
    // PROXY-50: recognize the first empty line, regardless of newline style.
    // A trailing CR may be half of CRLF, so leave it until the next push/EOF.
    let mut previous_newline = None;
    let mut index = 0;
    while index < buffer.len() {
        let width = match buffer[index] {
            b'\r' if index + 1 == buffer.len() => return None,
            b'\r' if buffer[index + 1] == b'\n' => 2,
            b'\r' | b'\n' => 1,
            _ => {
                index += 1;
                continue;
            }
        };
        if let Some((start, end)) = previous_newline
            && end == index
        {
            return Some((start, index + width - start));
        }
        previous_newline = Some((index, index + width));
        index += width;
    }
    None
}

fn field(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn read_chat_usage(usage: Option<&Value>, state: &mut StreamState) {
    state.input_tokens = usage
        .and_then(|v| v.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(state.input_tokens);
    state.output_tokens = usage
        .and_then(|v| v.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(state.output_tokens);
    state.cached_tokens = usage
        .and_then(|v| v.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(state.cached_tokens);
}

fn read_anthropic_usage(usage: Option<&Value>, state: &mut StreamState) {
    let input = usage
        .and_then(|v| v.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = usage
        .and_then(|v| v.get("cache_read_input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    state.input_tokens = input.saturating_add(cached).max(state.input_tokens);
    state.output_tokens = usage
        .and_then(|v| v.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(state.output_tokens);
    state.cached_tokens = cached.max(state.cached_tokens);
}

fn read_responses_usage(usage: Option<&Value>, state: &mut StreamState) {
    state.input_tokens = usage
        .and_then(|v| v.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(state.input_tokens);
    state.output_tokens = usage
        .and_then(|v| v.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(state.output_tokens);
    state.cached_tokens = usage
        .and_then(|v| v.pointer("/input_tokens_details/cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(state.cached_tokens);
}

#[cfg(test)]
mod regression_tests;

#[cfg(test)]
mod tests {
    use super::{MAX_BUFFERED_BODY_BYTES, MAX_SSE_FRAME_SIZE, StreamConverter};
    use crate::protocol::Protocol;

    #[test]
    fn anthropic_stream_becomes_chat_stream_across_network_chunks() {
        let input = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\",\"usage\":{\"input_tokens\":3,\"cache_read_input_tokens\":9}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
        let mut converter = StreamConverter::new(Protocol::AnthropicMessages, Protocol::OpenAiChat);
        let split = input.len() / 2;
        let mut output = converter.push(&input.as_bytes()[..split]).unwrap();
        output.extend(converter.push(&input.as_bytes()[split..]).unwrap());
        output.extend(converter.finish().unwrap());
        assert!(converter.state.text.is_empty());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("chat.completion.chunk"));
        assert!(
            output.contains("\\\"content\\\":\\\"Hi\\\"") || output.contains("\"content\":\"Hi\"")
        );
        assert!(output.contains("\"prompt_tokens\":12"));
        assert!(output.contains("data: [DONE]"));
    }

    #[test]
    fn tool_continuation_omits_absent_metadata_and_empty_tools_flush_object() {
        let input = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\",\"usage\":{}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"void\",\"input\":{}}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":1}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
        let mut converter = StreamConverter::new(Protocol::AnthropicMessages, Protocol::OpenAiChat);
        let mut output = converter.push(input.as_bytes()).unwrap();
        output.extend(converter.finish().unwrap());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("\"arguments\":\"{}\""));
        assert!(!output.contains("\"id\":null"));
        assert!(!output.contains("\"name\":null"));
        assert_eq!(output.matches("data: [DONE]").count(), 1);
    }

    #[test]
    fn responses_failure_stays_failure_for_streaming_and_aggregated_callers() {
        let mut converter = StreamConverter::new_aggregating(
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
        );
        let output = converter
            .push(b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"id\":\"r1\",\"model\":\"m\",\"error\":{\"message\":\"overloaded\"}}}\n\n")
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("event: error"));
        assert!(output.contains("overloaded"));
        assert!(!output.contains("message_stop"));
        assert!(converter.non_stream_response().is_err());
    }

    /// Network chunk boundaries are arbitrary, so conversion must not depend on
    /// them. One-byte chunks and deterministic pseudo-random splits must produce
    /// exactly the same converted stream as a single chunk.
    #[test]
    fn conversion_output_does_not_depend_on_chunk_boundaries() {
        let input = concat!(
            "data: {\"id\":\"chat_1\",\"model\":\"gpt\",\"created\":12,\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat_1\",\"model\":\"gpt\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"a\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat_1\",\"model\":\"gpt\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n",
            "data: [DONE]\n\n"
        );
        let bytes = input.as_bytes();
        let baseline = {
            let mut converter =
                StreamConverter::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
            let mut output = converter.push(bytes).unwrap();
            output.extend(converter.finish().unwrap());
            output
        };
        let mut cases = vec![(1..bytes.len()).collect::<Vec<_>>()];
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        for _ in 0..300 {
            let mut points: Vec<usize> = (0..3)
                .map(|_| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    (state % bytes.len() as u64) as usize
                })
                .collect();
            points.sort_unstable();
            points.dedup();
            cases.push(points);
        }
        for points in cases {
            let mut converter =
                StreamConverter::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
            let mut output = Vec::new();
            let mut previous = 0;
            for point in points.into_iter().chain(std::iter::once(bytes.len())) {
                if point > previous {
                    output.extend(converter.push(&bytes[previous..point]).unwrap());
                }
                previous = point;
            }
            output.extend(converter.finish().unwrap());
            assert_eq!(
                output, baseline,
                "a different chunk split changed the conversion"
            );
        }
    }

    #[test]
    fn aggregated_output_past_the_memory_limit_fails_instead_of_growing() {
        let mut converter =
            StreamConverter::new_aggregating(Protocol::OpenAiChat, Protocol::OpenAiResponses);
        let chunk = format!(
            "data: {}\n\n",
            serde_json::json!({
                "id": "c", "model": "gpt",
                "choices": [{"delta": {"content": "x".repeat(1024 * 1024)}, "finish_reason": null}]
            })
        );
        let mut failure = None;
        // Each push carries one MiB, so this covers the shared ceiling plus a
        // margin; the bound must stop the loop before it runs out of attempts.
        for _ in 0..MAX_BUFFERED_BODY_BYTES / (1024 * 1024) + 4 {
            if let Err(error) = converter.push(chunk.as_bytes()) {
                failure = Some(error);
                break;
            }
        }
        let error = failure.expect("the aggregate limit must stop an unbounded response");
        assert!(error.contains("conversion limit"), "{error}");
    }

    /// A caller that receives events does not need the whole answer, so a target
    /// that does not embed it retains nothing and the aggregate bound is not
    /// reachable, however long the stream is. `Protocol::OpenAiResponses` as a
    /// target is the exception, because its terminal events carry the output text.
    #[test]
    fn streaming_conversion_retains_no_aggregated_output() {
        let chunk = format!(
            "data: {}\n\n",
            serde_json::json!({"choices": [{"delta": {"content": "x".repeat(4096)}, "finish_reason": null}]})
        );
        let mut converter = StreamConverter::new(Protocol::OpenAiChat, Protocol::AnthropicMessages);
        converter.push(chunk.as_bytes()).unwrap();
        assert_eq!(converter.state.collected, 0);
        assert!(converter.state.text.is_empty());
    }

    #[test]
    fn oversized_unterminated_frame_is_rejected() {
        let mut converter = StreamConverter::new(Protocol::AnthropicMessages, Protocol::OpenAiChat);
        let oversized = vec![b'x'; MAX_SSE_FRAME_SIZE + 1];
        let error = converter.push(&oversized).unwrap_err();
        assert!(error.contains("8 MiB"));
    }

    #[test]
    fn truncated_stream_emits_failure_instead_of_success() {
        let input = "data: {\"id\":\"chat_1\",\"model\":\"gpt\",\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n";
        let mut converter = StreamConverter::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
        let mut output = converter.push(input.as_bytes()).unwrap();
        output.extend(converter.finish().unwrap());
        assert!(converter.non_stream_response().is_err());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("event: response.failed"));
        assert!(output.contains("upstream_stream_error"));
        assert!(!output.contains("event: response.completed"));
    }

    #[test]
    fn chat_stream_becomes_responses_events() {
        let input = concat!(
            "data: {\"id\":\"chat_1\",\"model\":\"gpt\",\"created\":12,\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat_1\",\"model\":\"gpt\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n",
            "data: [DONE]\n\n"
        );
        let mut converter = StreamConverter::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
        let mut output = converter.push(input.as_bytes()).unwrap();
        output.extend(converter.finish().unwrap());
        let response: serde_json::Value =
            serde_json::from_slice(&converter.non_stream_response().unwrap()).unwrap();
        assert_eq!(response["output"][0]["content"][0]["text"], "ok");
        assert_eq!(response["usage"]["total_tokens"], 3);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("event: response.created"));
        assert!(output.contains("event: response.output_text.delta"));
        assert!(output.contains("event: response.completed"));
        assert!(output.contains("\"total_tokens\":3"));
    }
}

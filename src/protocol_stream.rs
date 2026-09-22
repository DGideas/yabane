use std::collections::HashMap;

use serde_json::{Value, json};

use crate::protocol::Protocol;

const MAX_SSE_FRAME_SIZE: usize = 8 * 1024 * 1024;

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
    finished: bool,
    done: bool,
    truncated: bool,
    failure: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    tools: HashMap<usize, ToolState>,
}

#[derive(Default)]
struct ToolState {
    id: String,
    name: String,
    arguments: String,
    arguments_started: bool,
    started: bool,
}

enum Event {
    Start,
    Text(String),
    ToolStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolArguments {
        index: usize,
        delta: String,
    },
    Finish(Option<String>),
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
                return Err("Upstream SSE frame exceeds the 8 MiB conversion limit".to_owned());
            }
            let frame = self.buffer[..end].to_vec();
            self.buffer.drain(..end + delimiter);
            self.convert_frame(&frame, &mut output)?;
        }
        if self.buffer.len() > MAX_SSE_FRAME_SIZE {
            return Err("Upstream SSE frame exceeds the 8 MiB conversion limit".to_owned());
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
        if self.state.truncated {
            return Err("Upstream stream ended before a terminal event".to_owned());
        }
        if let Some(message) = &self.state.failure {
            return Err(format!("Upstream stream failed: {message}"));
        }
        let value = match self.target {
            Protocol::OpenAiResponses => completed_response(&self.state),
            Protocol::OpenAiChat => {
                let tools: Vec<_> = sorted_tools(&self.state)
                    .into_iter()
                    .map(|(_, tool)| json!({
                        "id": tool.id, "type": "function", "function": {
                            "name": tool.name,
                            "arguments": if tool.arguments.is_empty() { "{}" } else { &tool.arguments }
                        }
                    }))
                    .collect();
                let mut message = json!({"role": "assistant", "content": self.state.text});
                if !tools.is_empty() {
                    message["tool_calls"] = Value::Array(tools);
                }
                json!({
                    "id": self.state.id, "object": "chat.completion", "created": self.state.created,
                    "model": self.state.model,
                    "choices": [{"index": 0, "message": message, "finish_reason": chat_finish(None, !self.state.tools.is_empty())}],
                    "usage": openai_usage(&self.state)
                })
            }
            Protocol::AnthropicMessages => {
                let mut content = Vec::new();
                if !self.state.text.is_empty() {
                    content.push(json!({"type": "text", "text": self.state.text}));
                }
                content.extend(sorted_tools(&self.state).into_iter().map(|(_, tool)| {
                    let input = serde_json::from_str(&tool.arguments).unwrap_or_else(|_| json!({}));
                    json!({"type": "tool_use", "id": tool.id, "name": tool.name, "input": input})
                }));
                json!({
                    "id": self.state.id, "type": "message", "role": "assistant", "model": self.state.model,
                    "content": content, "stop_reason": anthropic_finish(None, !self.state.tools.is_empty()), "stop_sequence": null,
                    "usage": {"input_tokens": self.state.input_tokens.saturating_sub(self.state.cached_tokens), "output_tokens": self.state.output_tokens,
                        "cache_read_input_tokens": self.state.cached_tokens}
                })
            }
        };
        serde_json::to_vec(&value).map_err(|err| err.to_string())
    }

    fn convert_frame(&mut self, frame: &[u8], output: &mut Vec<u8>) -> Result<(), String> {
        let data = frame
            .split(|byte| *byte == b'\n')
            .filter_map(|line| {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
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
            "Upstream SSE data must be valid JSON for protocol conversion".to_owned()
        })?;
        let events = match self.source {
            Protocol::OpenAiChat => self.parse_chat(&value),
            Protocol::OpenAiResponses => self.parse_responses(&value),
            Protocol::AnthropicMessages => self.parse_anthropic(&value),
        };
        for event in events {
            self.render(event, output)?;
        }
        Ok(())
    }

    fn parse_chat(&mut self, value: &Value) -> Vec<Event> {
        self.read_metadata(value, "created");
        read_chat_usage(value.get("usage"), &mut self.state);
        let mut events = vec![Event::Start];
        let delta = &value["choices"][0]["delta"];
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            events.push(Event::Text(text.to_owned()));
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
            events.push(Event::Finish(Some(reason.to_owned())));
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
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    vec![Event::ToolStart {
                        index: value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
                        id: field(block, "id"),
                        name: field(block, "name"),
                    }]
                } else {
                    Vec::new()
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
            Some("message_delta") => {
                read_anthropic_usage(value.get("usage"), &mut self.state);
                vec![Event::Finish(
                    value
                        .pointer("/delta/stop_reason")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                )]
            }
            Some("message_stop") => vec![Event::Done],
            _ => Vec::new(),
        }
    }

    fn parse_responses(&mut self, value: &Value) -> Vec<Event> {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response = value.get("response").unwrap_or(value);
        self.read_metadata(response, "created_at");
        match kind {
            "response.created" | "response.in_progress" => vec![Event::Start],
            "response.output_text.delta" => vec![Event::Text(field(value, "delta"))],
            "response.output_item.added"
                if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call") =>
            {
                vec![Event::ToolStart {
                    index: value
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize,
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
                }]
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
                vec![
                    Event::Finish(
                        response
                            .get("stop_reason")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    ),
                    Event::Done,
                ]
            }
            "response.failed" => vec![
                Event::Failure(
                    response
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("Upstream response failed")
                        .to_owned(),
                ),
                Event::Done,
            ],
            _ => Vec::new(),
        }
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
                Protocol::OpenAiResponses => emit_event(
                    output,
                    "response.failed",
                    &json!({"type": "response.failed", "response": {"id": self.state.id, "object": "response", "status": "failed", "model": self.state.model, "error": {"code": "upstream_error", "message": message}}}),
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
        let message = "Upstream stream ended before a terminal event";
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
            Protocol::OpenAiResponses => emit_event(
                output,
                "response.failed",
                &json!({"type": "response.failed", "response": {"id": self.state.id, "object": "response", "status": "failed", "model": self.state.model, "error": {"code": "upstream_stream_error", "message": message}}}),
            ),
        }
    }
}

impl StreamConverter {
    fn render_chat(&mut self, event: Event, output: &mut Vec<u8>) -> Result<(), String> {
        match event {
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
                let tool = self.state.tools.entry(index).or_default();
                if !id.is_empty() {
                    tool.id = id;
                }
                if !name.is_empty() {
                    tool.name = name;
                }
                if !tool.started {
                    tool.started = true;
                    emit_data(
                        output,
                        &json!({
                            "id": self.state.id, "object": "chat.completion.chunk", "created": self.state.created,
                            "model": self.state.model, "choices": [{"index": 0, "delta": {"tool_calls": [{
                                "index": index, "id": tool.id, "type": "function", "function": {"name": tool.name, "arguments": ""}
                            }]}, "finish_reason": null}]
                        }),
                    )?;
                }
            }
            Event::ToolArguments { index, delta } => {
                self.ensure_started(output)?;
                let tool = self.state.tools.entry(index).or_default();
                tool.arguments_started |= !delta.is_empty();
                if self.collect_output {
                    tool.arguments.push_str(&delta);
                }
                emit_data(
                    output,
                    &json!({
                        "id": self.state.id, "object": "chat.completion.chunk", "created": self.state.created,
                        "model": self.state.model, "choices": [{"index": 0, "delta": {"tool_calls": [{
                            "index": index, "function": {"arguments": delta}
                        }]}, "finish_reason": null}]
                    }),
                )?;
            }
            Event::Finish(reason) if !self.state.finished => {
                self.ensure_started(output)?;
                let empty_tools: Vec<_> = self
                    .state
                    .tools
                    .iter()
                    .filter(|(_, tool)| !tool.arguments_started)
                    .map(|(index, _)| *index)
                    .collect();
                for index in empty_tools {
                    emit_data(
                        output,
                        &json!({
                            "id": self.state.id, "object": "chat.completion.chunk", "created": self.state.created,
                            "model": self.state.model, "choices": [{"index": 0, "delta": {"tool_calls": [{
                                "index": index, "function": {"arguments": "{}"}
                            }]}, "finish_reason": null}]
                        }),
                    )?;
                }
                let finish = chat_finish(reason.as_deref(), !self.state.tools.is_empty());
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
                    self.render_chat(Event::Finish(None), output)?;
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
            Event::Start => self.ensure_anthropic_started(output)?,
            Event::Text(text) => {
                self.ensure_anthropic_started(output)?;
                if !self.state.text_started {
                    self.state.text_started = true;
                    emit_event(
                        output,
                        "content_block_start",
                        &json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                    )?;
                }
                if self.collect_output {
                    self.state.text.push_str(&text);
                }
                emit_event(
                    output,
                    "content_block_delta",
                    &json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
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
                    emit_event(
                        output,
                        "content_block_start",
                        &json!({
                            "type": "content_block_start", "index": index + 1,
                            "content_block": {"type": "tool_use", "id": tool.id, "name": tool.name, "input": {}}
                        }),
                    )?;
                }
            }
            Event::ToolArguments { index, delta } => {
                self.ensure_anthropic_started(output)?;
                let tool = self.state.tools.entry(index).or_default();
                tool.arguments_started |= !delta.is_empty();
                if self.collect_output {
                    tool.arguments.push_str(&delta);
                }
                emit_event(
                    output,
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta", "index": index + 1,
                        "delta": {"type": "input_json_delta", "partial_json": delta}
                    }),
                )?;
            }
            Event::Finish(reason) if !self.state.finished => {
                self.ensure_anthropic_started(output)?;
                if self.state.text_started {
                    emit_event(
                        output,
                        "content_block_stop",
                        &json!({"type": "content_block_stop", "index": 0}),
                    )?;
                }
                let mut indexes: Vec<_> = self
                    .state
                    .tools
                    .iter()
                    .filter(|(_, tool)| tool.started)
                    .map(|(index, _)| *index)
                    .collect();
                indexes.sort_unstable();
                for index in indexes {
                    emit_event(
                        output,
                        "content_block_stop",
                        &json!({"type": "content_block_stop", "index": index + 1}),
                    )?;
                }
                let stop = anthropic_finish(reason.as_deref(), !self.state.tools.is_empty());
                emit_event(
                    output,
                    "message_delta",
                    &json!({
                        "type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null},
                        "usage": {"output_tokens": self.state.output_tokens}
                    }),
                )?;
                self.state.finished = true;
            }
            Event::Finish(_) => {}
            Event::Done if !self.state.done => {
                if !self.state.finished {
                    self.render_anthropic(Event::Finish(None), output)?;
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
            Event::Start => self.ensure_responses_started(output)?,
            Event::Text(text) => {
                self.ensure_responses_started(output)?;
                if !self.state.text_started {
                    self.state.text_started = true;
                    emit_event(
                        output,
                        "response.output_item.added",
                        &json!({
                            "type": "response.output_item.added", "output_index": 0,
                            "item": {"id": format!("msg_{}", self.state.id), "type": "message", "status": "in_progress", "role": "assistant", "content": []}
                        }),
                    )?;
                    emit_event(
                        output,
                        "response.content_part.added",
                        &json!({
                            "type": "response.content_part.added", "output_index": 0, "content_index": 0,
                            "part": {"type": "output_text", "text": "", "annotations": []}
                        }),
                    )?;
                }
                self.state.text.push_str(&text);
                emit_event(
                    output,
                    "response.output_text.delta",
                    &json!({
                        "type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": text
                    }),
                )?;
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
                    emit_event(
                        output,
                        "response.output_item.added",
                        &json!({
                            "type": "response.output_item.added", "output_index": index + 1,
                            "item": {"id": format!("fc_{}", tool.id), "type": "function_call", "status": "in_progress", "call_id": tool.id, "name": tool.name, "arguments": ""}
                        }),
                    )?;
                }
            }
            Event::ToolArguments { index, delta } => {
                self.ensure_responses_started(output)?;
                let tool = self.state.tools.entry(index).or_default();
                tool.arguments_started |= !delta.is_empty();
                tool.arguments.push_str(&delta);
                emit_event(
                    output,
                    "response.function_call_arguments.delta",
                    &json!({
                        "type": "response.function_call_arguments.delta", "output_index": index + 1, "delta": delta
                    }),
                )?;
            }
            Event::Finish(_) if !self.state.finished => {
                self.ensure_responses_started(output)?;
                let response = completed_response(&self.state);
                emit_event(
                    output,
                    "response.completed",
                    &json!({"type": "response.completed", "response": response}),
                )?;
                self.state.finished = true;
            }
            Event::Finish(_) => {}
            Event::Done if !self.state.done => {
                if !self.state.finished {
                    self.render_responses(Event::Finish(None), output)?;
                }
                self.state.done = true;
            }
            Event::Done => {}
            Event::Failure(_) => unreachable!("failure events are handled before target rendering"),
        }
        Ok(())
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
            emit_event(
                output,
                "response.created",
                &json!({"type": "response.created", "response": response_shell(&self.state, "in_progress")}),
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

fn completed_response(state: &StreamState) -> Value {
    let mut output = Vec::new();
    if state.text_started {
        output.push(json!({"id": format!("msg_{}", state.id), "type": "message", "status": "completed", "role": "assistant",
            "content": [{"type": "output_text", "text": state.text, "annotations": []}]}));
    }
    let mut tools: Vec<_> = state.tools.iter().collect();
    tools.sort_by_key(|(index, _)| **index);
    output.extend(tools.into_iter().map(|(_, tool)| {
        json!({"id": format!("fc_{}", tool.id), "type": "function_call", "status": "completed",
        "call_id": tool.id, "name": tool.name, "arguments": tool.arguments})
    }));
    let mut response = response_shell(state, "completed");
    response["output"] = Value::Array(output);
    response["usage"] = json!({"input_tokens": state.input_tokens, "input_tokens_details": {"cached_tokens": state.cached_tokens},
        "output_tokens": state.output_tokens, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": state.input_tokens + state.output_tokens});
    response
}

fn chat_finish(reason: Option<&str>, has_tools: bool) -> &'static str {
    if has_tools || matches!(reason, Some("tool_use" | "tool_calls")) {
        "tool_calls"
    } else if matches!(reason, Some("max_tokens" | "length")) {
        "length"
    } else {
        "stop"
    }
}

fn anthropic_finish(reason: Option<&str>, has_tools: bool) -> &'static str {
    if has_tools || matches!(reason, Some("tool_use" | "tool_calls")) {
        "tool_use"
    } else if matches!(reason, Some("max_tokens" | "length")) {
        "max_tokens"
    } else {
        "end_turn"
    }
}

fn sorted_tools(state: &StreamState) -> Vec<(usize, &ToolState)> {
    let mut tools: Vec<_> = state
        .tools
        .iter()
        .map(|(index, tool)| (*index, tool))
        .collect();
    tools.sort_by_key(|(index, _)| *index);
    tools
}

fn next_frame(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|end| (end, 2))
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|end| (end, 4))
        })
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
mod tests {
    use super::{MAX_SSE_FRAME_SIZE, StreamConverter};
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

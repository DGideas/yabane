use serde_json::{Map, Value, json};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
}

impl Protocol {
    pub fn name(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai_chat_completions",
            Self::OpenAiResponses => "openai_responses",
            Self::AnthropicMessages => "anthropic_messages",
        }
    }
}

pub fn convert_request(body: &[u8], source: Protocol, target: Protocol) -> Result<Vec<u8>, String> {
    if source == target {
        return Ok(body.to_vec());
    }

    let value: Value =
        serde_json::from_slice(body).map_err(|_| "Request body must be valid JSON".to_owned())?;
    let converted = match (source, target) {
        (Protocol::OpenAiChat, Protocol::OpenAiResponses) => chat_to_responses(value)?,
        (Protocol::OpenAiChat, Protocol::AnthropicMessages) => chat_to_anthropic(value)?,
        (Protocol::OpenAiResponses, Protocol::OpenAiChat) => responses_to_chat(value)?,
        (Protocol::OpenAiResponses, Protocol::AnthropicMessages) => responses_to_anthropic(value)?,
        (Protocol::AnthropicMessages, Protocol::OpenAiChat) => anthropic_to_chat(value)?,
        (Protocol::AnthropicMessages, Protocol::OpenAiResponses) => anthropic_to_responses(value)?,
        _ => unreachable!("equal protocols returned before conversion"),
    };
    serde_json::to_vec(&converted).map_err(|err| format!("Could not serialize request: {err}"))
}

pub fn convert_response(
    body: &[u8],
    source: Protocol,
    target: Protocol,
) -> Result<Vec<u8>, String> {
    if source == target {
        return Ok(body.to_vec());
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| {
        "Upstream response body must be valid JSON for protocol conversion".to_owned()
    })?;
    let response = CanonicalResponse::parse(value, source)?;
    serde_json::to_vec(&response.render(target))
        .map_err(|err| format!("Could not serialize converted response: {err}"))
}

#[derive(Default)]
struct CanonicalResponse {
    id: String,
    model: String,
    created: u64,
    text: String,
    tool_calls: Vec<CanonicalToolCall>,
    stop_reason: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
}

struct CanonicalToolCall {
    id: String,
    name: String,
    arguments: Value,
}

impl CanonicalResponse {
    fn parse(value: Value, protocol: Protocol) -> Result<Self, String> {
        if protocol == Protocol::OpenAiResponses
            && value.get("status").and_then(Value::as_str) == Some("failed")
        {
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("OpenAI Responses result failed");
            return Err(format!(
                "Upstream OpenAI Responses result failed: {message}"
            ));
        }
        match protocol {
            Protocol::OpenAiChat => Self::from_chat(value),
            Protocol::OpenAiResponses => Self::from_responses(value),
            Protocol::AnthropicMessages => Self::from_anthropic(value),
        }
    }

    fn from_chat(value: Value) -> Result<Self, String> {
        let message = value
            .pointer("/choices/0/message")
            .ok_or_else(|| "OpenAI Chat response has no first message".to_owned())?;
        let mut response = Self {
            id: string_field(&value, "id"),
            model: string_field(&value, "model"),
            created: value.get("created").and_then(Value::as_u64).unwrap_or(0),
            text: text_content(message.get("content")),
            stop_reason: value
                .pointer("/choices/0/finish_reason")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..Self::default()
        };
        response.tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|call| {
                let raw = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                CanonicalToolCall {
                    id: string_field(call, "id"),
                    name: call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    arguments: serde_json::from_str(raw)
                        .unwrap_or_else(|_| Value::String(raw.to_owned())),
                }
            })
            .collect();
        response.read_openai_usage(value.get("usage"));
        Ok(response)
    }

    fn from_anthropic(value: Value) -> Result<Self, String> {
        let content = value
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| "Anthropic response content must be an array".to_owned())?;
        let mut response = Self {
            id: string_field(&value, "id"),
            model: string_field(&value, "model"),
            text: content
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
            stop_reason: value
                .get("stop_reason")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..Self::default()
        };
        response.tool_calls = content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
            .map(|block| CanonicalToolCall {
                id: string_field(block, "id"),
                name: string_field(block, "name"),
                arguments: block.get("input").cloned().unwrap_or_else(|| json!({})),
            })
            .collect();
        let usage = value.get("usage");
        response.input_tokens = usage
            .and_then(|usage| usage.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        response.output_tokens = usage
            .and_then(|usage| usage.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        response.cached_tokens = usage
            .and_then(|usage| usage.get("cache_read_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Ok(response)
    }

    fn from_responses(value: Value) -> Result<Self, String> {
        let output = value
            .get("output")
            .and_then(Value::as_array)
            .ok_or_else(|| "OpenAI Responses output must be an array".to_owned())?;
        let mut response = Self {
            id: string_field(&value, "id"),
            model: string_field(&value, "model"),
            created: value.get("created_at").and_then(Value::as_u64).unwrap_or(0),
            stop_reason: value
                .get("stop_reason")
                .and_then(Value::as_str)
                .or_else(|| {
                    value
                        .pointer("/incomplete_details/reason")
                        .and_then(Value::as_str)
                })
                .map(str::to_owned),
            ..Self::default()
        };
        for item in output {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    response.text.push_str(
                        &item
                            .get("content")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter(|part| {
                                matches!(
                                    part.get("type").and_then(Value::as_str),
                                    Some("output_text") | Some("text")
                                )
                            })
                            .filter_map(|part| part.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join(""),
                    );
                }
                Some("function_call") => response.tool_calls.push(CanonicalToolCall {
                    id: item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    name: string_field(item, "name"),
                    arguments: item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .and_then(|raw| serde_json::from_str(raw).ok())
                        .or_else(|| item.get("arguments").cloned())
                        .unwrap_or_else(|| json!({})),
                }),
                _ => {}
            }
        }
        response.read_responses_usage(value.get("usage"));
        Ok(response)
    }

    fn read_openai_usage(&mut self, usage: Option<&Value>) {
        self.input_tokens = usage
            .and_then(|usage| usage.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.output_tokens = usage
            .and_then(|usage| usage.get("completion_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.cached_tokens = usage
            .and_then(|usage| usage.pointer("/prompt_tokens_details/cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
    }

    fn read_responses_usage(&mut self, usage: Option<&Value>) {
        self.input_tokens = usage
            .and_then(|usage| usage.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.output_tokens = usage
            .and_then(|usage| usage.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.cached_tokens = usage
            .and_then(|usage| usage.pointer("/input_tokens_details/cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
    }

    fn render(&self, protocol: Protocol) -> Value {
        match protocol {
            Protocol::OpenAiChat => self.render_chat(),
            Protocol::OpenAiResponses => self.render_responses(),
            Protocol::AnthropicMessages => self.render_anthropic(),
        }
    }

    fn render_chat(&self) -> Value {
        let tool_calls: Vec<_> = self
            .tool_calls
            .iter()
            .map(|call| {
                json!({
                    "id": call.id, "type": "function", "function": {
                        "name": call.name,
                        "arguments": arguments_string(&call.arguments)
                    }
                })
            })
            .collect();
        let mut message = json!({"role": "assistant", "content": self.text});
        if !tool_calls.is_empty() {
            message["tool_calls"] = Value::Array(tool_calls);
        }
        json!({
            "id": self.id,
            "object": "chat.completion",
            "created": self.created,
            "model": self.model,
            "choices": [{"index": 0, "message": message, "finish_reason": chat_finish_reason(self.stop_reason.as_deref(), !self.tool_calls.is_empty())}],
            "usage": {
                "prompt_tokens": self.input_tokens,
                "completion_tokens": self.output_tokens,
                "total_tokens": self.input_tokens + self.output_tokens,
                "prompt_tokens_details": {"cached_tokens": self.cached_tokens}
            }
        })
    }

    fn render_anthropic(&self) -> Value {
        let mut content = Vec::new();
        if !self.text.is_empty() {
            content.push(json!({"type": "text", "text": self.text}));
        }
        content.extend(self.tool_calls.iter().map(|call| {
            json!({
                "type": "tool_use", "id": call.id, "name": call.name, "input": call.arguments
            })
        }));
        json!({
            "id": self.id,
            "type": "message",
            "role": "assistant",
            "model": self.model,
            "content": content,
            "stop_reason": anthropic_stop_reason(self.stop_reason.as_deref(), !self.tool_calls.is_empty()),
            "stop_sequence": null,
            "usage": {
                "input_tokens": self.input_tokens,
                "output_tokens": self.output_tokens,
                "cache_read_input_tokens": self.cached_tokens
            }
        })
    }

    fn render_responses(&self) -> Value {
        let mut output = Vec::new();
        if !self.text.is_empty() {
            output.push(json!({
                "id": format!("msg_{}", self.id), "type": "message", "status": "completed", "role": "assistant",
                "content": [{"type": "output_text", "text": self.text, "annotations": []}]
            }));
        }
        output.extend(self.tool_calls.iter().map(|call| json!({
            "id": format!("fc_{}", call.id), "type": "function_call", "status": "completed",
            "call_id": call.id, "name": call.name, "arguments": arguments_string(&call.arguments)
        })));
        json!({
            "id": self.id,
            "object": "response",
            "created_at": self.created,
            "status": "completed",
            "model": self.model,
            "output": output,
            "parallel_tool_calls": true,
            "error": null,
            "incomplete_details": null,
            "usage": {
                "input_tokens": self.input_tokens,
                "input_tokens_details": {"cached_tokens": self.cached_tokens},
                "output_tokens": self.output_tokens,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": self.input_tokens + self.output_tokens
            }
        })
    }
}

fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn text_content(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

fn arguments_string(arguments: &Value) -> String {
    arguments
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_owned()))
}

fn chat_finish_reason(reason: Option<&str>, has_tools: bool) -> &'static str {
    match reason {
        Some("tool_use") | Some("tool_calls") => "tool_calls",
        Some("max_tokens") | Some("max_output_tokens") | Some("length") => "length",
        _ if has_tools => "tool_calls",
        Some("end_turn") | Some("stop_sequence") | Some("stop") | None => "stop",
        _ => "stop",
    }
}

fn anthropic_stop_reason(reason: Option<&str>, has_tools: bool) -> &'static str {
    if has_tools || reason == Some("tool_calls") {
        "tool_use"
    } else if matches!(reason, Some("length" | "max_output_tokens")) {
        "max_tokens"
    } else {
        "end_turn"
    }
}

fn object(value: Value) -> Result<Map<String, Value>, String> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "Request body must be a JSON object".to_owned())
}

fn chat_to_responses(value: Value) -> Result<Value, String> {
    let mut source = object(value)?;
    let messages = source
        .remove("messages")
        .ok_or_else(|| "Chat Completions request must contain messages".to_owned())?;
    source.insert("input".to_owned(), chat_messages_to_responses(messages)?);
    let max_output_tokens = source
        .remove("max_completion_tokens")
        .or_else(|| source.remove("max_tokens"));
    if let Some(max_output_tokens) = max_output_tokens {
        source.insert("max_output_tokens".to_owned(), max_output_tokens);
    }
    if let Some(response_format) = source.remove("response_format") {
        source.insert(
            "text".to_owned(),
            json!({"format": chat_response_format_to_responses(response_format)}),
        );
    }
    if let Some(reasoning_effort) = source.remove("reasoning_effort") {
        source.insert("reasoning".to_owned(), json!({"effort": reasoning_effort}));
    }
    if let Some(tools) = source.remove("tools") {
        source.insert("tools".to_owned(), chat_tools_to_responses(tools)?);
    }
    if let Some(choice) = source.remove("tool_choice") {
        source.insert(
            "tool_choice".to_owned(),
            chat_tool_choice_to_responses(choice),
        );
    }
    remove_fields(
        &mut source,
        &[
            "audio",
            "frequency_penalty",
            "logit_bias",
            "logprobs",
            "modalities",
            "n",
            "prediction",
            "presence_penalty",
            "seed",
            "stop",
            "stream_options",
            "top_logprobs",
            "web_search_options",
        ],
    );
    Ok(Value::Object(source))
}

fn responses_to_chat(value: Value) -> Result<Value, String> {
    let mut source = object(value)?;
    let input = source
        .remove("input")
        .ok_or_else(|| "Responses request must contain input".to_owned())?;
    let mut messages = responses_input_to_chat(input)?
        .as_array()
        .cloned()
        .expect("responses input conversion always returns an array");
    if let Some(instructions) = source.remove("instructions") {
        messages.insert(0, json!({"role": "system", "content": instructions}));
    }
    source.insert("messages".to_owned(), Value::Array(messages));
    rename(&mut source, "max_output_tokens", "max_completion_tokens");
    if let Some(text) = source.remove("text")
        && let Some(format) = text.get("format")
    {
        source.insert(
            "response_format".to_owned(),
            responses_response_format_to_chat(format.clone()),
        );
    }
    if let Some(reasoning) = source.remove("reasoning")
        && let Some(effort) = reasoning.get("effort")
    {
        source.insert("reasoning_effort".to_owned(), effort.clone());
    }
    if let Some(tools) = source.remove("tools") {
        source.insert("tools".to_owned(), responses_tools_to_chat(tools)?);
    }
    if let Some(choice) = source.remove("tool_choice") {
        source.insert(
            "tool_choice".to_owned(),
            responses_tool_choice_to_chat(choice),
        );
    }
    for unsupported in [
        "background",
        "include",
        "max_tool_calls",
        "previous_response_id",
        "prompt_cache_key",
        "safety_identifier",
        "truncation",
    ] {
        source.remove(unsupported);
    }
    Ok(Value::Object(source))
}

fn chat_to_anthropic(value: Value) -> Result<Value, String> {
    let mut source = object(value)?;
    let messages = source
        .remove("messages")
        .ok_or_else(|| "Chat Completions request must contain messages".to_owned())?;
    let (system, messages) = chat_messages_to_anthropic(messages)?;
    source.insert("messages".to_owned(), messages);
    if let Some(system) = system {
        source.insert("system".to_owned(), system);
    }
    let max_tokens = source
        .remove("max_completion_tokens")
        .or_else(|| source.remove("max_tokens"))
        .unwrap_or_else(|| Value::Number(4096.into()));
    source.insert("max_tokens".to_owned(), max_tokens);
    rename(&mut source, "stop", "stop_sequences");
    if let Some(tools) = source.remove("tools") {
        source.insert("tools".to_owned(), chat_tools_to_anthropic(tools)?);
    }
    if let Some(choice) = source.remove("tool_choice") {
        source.insert(
            "tool_choice".to_owned(),
            chat_tool_choice_to_anthropic(choice),
        );
    }
    source.remove("n");
    source.remove("response_format");
    source.remove("stream_options");
    remove_fields(
        &mut source,
        &[
            "audio",
            "frequency_penalty",
            "logit_bias",
            "logprobs",
            "modalities",
            "prediction",
            "presence_penalty",
            "seed",
            "top_logprobs",
            "user",
            "web_search_options",
        ],
    );
    Ok(Value::Object(source))
}

fn anthropic_to_chat(value: Value) -> Result<Value, String> {
    let mut source = object(value)?;
    let messages = source
        .remove("messages")
        .ok_or_else(|| "Anthropic Messages request must contain messages".to_owned())?;
    let mut messages = anthropic_messages_to_chat(messages)?;
    if let Some(system) = source.remove("system") {
        messages.insert(0, json!({"role": "system", "content": system}));
    }
    source.insert("messages".to_owned(), Value::Array(messages));
    rename(&mut source, "max_tokens", "max_completion_tokens");
    rename(&mut source, "stop_sequences", "stop");
    if let Some(tools) = source.remove("tools") {
        source.insert("tools".to_owned(), anthropic_tools_to_chat(tools)?);
    }
    if let Some(choice) = source.remove("tool_choice") {
        source.insert(
            "tool_choice".to_owned(),
            anthropic_tool_choice_to_chat(choice),
        );
    }
    remove_fields(
        &mut source,
        &[
            "container",
            "context_management",
            "mcp_servers",
            "output_config",
            "thinking",
            "top_k",
        ],
    );
    Ok(Value::Object(source))
}

fn responses_to_anthropic(value: Value) -> Result<Value, String> {
    let chat = responses_to_chat(value)?;
    chat_to_anthropic(chat)
}

fn anthropic_to_responses(value: Value) -> Result<Value, String> {
    let original = object(value)?;
    let mut converted = object(chat_to_responses(anthropic_to_chat(Value::Object(
        original.clone(),
    ))?)?)?;
    converted.insert("input".to_owned(), anthropic_input_to_responses(&original)?);
    Ok(Value::Object(converted))
}

fn chat_messages_to_responses(messages: Value) -> Result<Value, String> {
    let messages = messages
        .as_array()
        .ok_or_else(|| "Chat Completions messages must be an array".to_owned())?;
    let mut input = Vec::new();
    for message in messages {
        let mut message = object(message.clone())?;
        if message.get("role").and_then(Value::as_str) == Some("tool") {
            input.push(json!({
                "type": "function_call_output",
                "call_id": message.remove("tool_call_id").unwrap_or(Value::Null),
                "output": message.remove("content").unwrap_or(Value::String(String::new()))
            }));
            continue;
        }
        if let Some(tool_calls) = message.remove("tool_calls") {
            if message.get("content").is_some_and(|content| {
                !content.is_null() && content.as_array().is_none_or(|parts| !parts.is_empty())
            }) {
                input.push(Value::Object(message.clone()));
            }
            for call in tool_calls.as_array().into_iter().flatten() {
                let function = &call["function"];
                input.push(json!({
                    "type": "function_call",
                    "call_id": call.get("id").cloned().unwrap_or(Value::Null),
                    "name": function.get("name").cloned().unwrap_or(Value::Null),
                    "arguments": function.get("arguments").cloned().unwrap_or(Value::String("{}".to_owned()))
                }));
            }
            continue;
        }
        if let Some(content) = message.remove("content") {
            message.insert("content".to_owned(), chat_content_to_responses(content)?);
        }
        input.push(Value::Object(message));
    }
    Ok(Value::Array(input))
}

fn chat_content_to_responses(content: Value) -> Result<Value, String> {
    if content.is_string() || content.is_null() {
        return Ok(content);
    }
    let parts = content
        .as_array()
        .ok_or_else(|| "Chat message content must be a string or array".to_owned())?;
    Ok(Value::Array(
        parts
            .iter()
            .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                Some("text") => Some(json!({"type": "input_text", "text": part.get("text").cloned().unwrap_or(Value::String(String::new()))})),
                Some("image_url") => Some(json!({"type": "input_image", "image_url": part.pointer("/image_url/url").cloned().unwrap_or(Value::Null)})),
                Some("file") => chat_file_to_responses(part),
                _ => None,
            })
            .collect(),
    ))
}

fn anthropic_input_to_responses(source: &Map<String, Value>) -> Result<Value, String> {
    let mut input = Vec::new();
    if let Some(system) = source.get("system") {
        input.push(json!({"role": "system", "content": anthropic_content_to_responses(system.clone(), false)?}));
    }
    let messages = source
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "Anthropic messages must be an array".to_owned())?;
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let content = message.get("content").cloned().unwrap_or(Value::Null);
        if let Some(blocks) = content.as_array() {
            let mut pending = Vec::new();
            for block in blocks {
                let kind = block.get("type").and_then(Value::as_str);
                if matches!(kind, Some("text" | "image" | "document")) {
                    pending.push(block.clone());
                    continue;
                }
                flush_anthropic_message(&mut input, role, &mut pending)?;
                match kind {
                    Some("tool_use") => input.push(json!({
                        "type": "function_call", "call_id": block.get("id").cloned().unwrap_or(Value::Null),
                        "name": block.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": serde_json::to_string(block.get("input").unwrap_or(&Value::Null)).unwrap_or_else(|_| "{}".to_owned())
                    })),
                    Some("tool_result") => input.push(json!({
                        "type": "function_call_output", "call_id": block.get("tool_use_id").cloned().unwrap_or(Value::Null),
                        "output": block.get("content").cloned().unwrap_or(Value::String(String::new()))
                    })),
                    Some("thinking") => input.push(json!({
                        "type": "reasoning", "summary": [{"type": "summary_text", "text": block.get("thinking").cloned().unwrap_or(Value::String(String::new()))}],
                        "encrypted_content": block.get("signature").cloned().unwrap_or(Value::Null)
                    })),
                    Some("redacted_thinking") => input.push(json!({
                        "type": "reasoning", "summary": [], "encrypted_content": block.get("data").cloned().unwrap_or(Value::Null)
                    })),
                    _ => {}
                }
            }
            flush_anthropic_message(&mut input, role, &mut pending)?;
        } else {
            input.push(json!({"role": role, "content": content}));
        }
    }
    Ok(Value::Array(input))
}

fn flush_anthropic_message(
    input: &mut Vec<Value>,
    role: &str,
    pending: &mut Vec<Value>,
) -> Result<(), String> {
    if pending.is_empty() {
        return Ok(());
    }
    let blocks = Value::Array(std::mem::take(pending));
    input.push(json!({"role": role, "content": anthropic_content_to_responses(blocks, role == "assistant")?}));
    Ok(())
}

fn anthropic_content_to_responses(content: Value, output: bool) -> Result<Value, String> {
    if content.is_string() {
        return Ok(content);
    }
    let blocks = content
        .as_array()
        .ok_or_else(|| "Anthropic content must be a string or array".to_owned())?;
    Ok(Value::Array(blocks.iter().filter_map(|block| match block.get("type").and_then(Value::as_str) {
        Some("text") => Some(json!({"type": if output { "output_text" } else { "input_text" }, "text": block.get("text").cloned().unwrap_or(Value::String(String::new()))})),
        Some("image") => {
            let image = anthropic_image_to_chat(block);
            Some(json!({"type": "input_image", "image_url": image.pointer("/image_url/url").cloned().unwrap_or(Value::Null)}))
        }
        Some("document") => anthropic_document_to_responses(block),
        _ => None,
    }).collect()))
}

fn responses_input_to_chat(input: Value) -> Result<Value, String> {
    if let Some(text) = input.as_str() {
        return Ok(json!([{"role": "user", "content": text}]));
    }
    let items = input
        .as_array()
        .ok_or_else(|| "Responses input must be a string or array".to_owned())?;
    let mut messages = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => messages.push(json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or(Value::Null),
                    "type": "function",
                    "function": {
                        "name": item.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": item.get("arguments").cloned().unwrap_or(Value::String("{}".to_owned()))
                    }
                }]
            })),
            Some("function_call_output") => messages.push(json!({
                "role": "tool",
                "tool_call_id": item.get("call_id").cloned().unwrap_or(Value::Null),
                "content": item.get("output").cloned().unwrap_or(Value::String(String::new()))
            })),
            _ => {
                let mut message = object(item.clone())?;
                message.remove("type");
                if let Some(content) = message.remove("content") {
                    message.insert("content".to_owned(), responses_content_to_chat(content)?);
                }
                messages.push(Value::Object(message));
            }
        }
    }
    Ok(Value::Array(messages))
}

fn chat_messages_to_anthropic(messages: Value) -> Result<(Option<Value>, Value), String> {
    let messages = messages
        .as_array()
        .ok_or_else(|| "Chat Completions messages must be an array".to_owned())?;
    let known_call_ids: Vec<String> = messages
        .iter()
        .flat_map(|message| {
            message
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|call| call.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let mut system = Vec::new();
    let mut converted = Vec::new();
    for item in messages {
        let mut message = object(item.clone())?;
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_owned();
        if role == "system" || role == "developer" {
            system.extend(content_to_anthropic_blocks(
                message.remove("content").unwrap_or(Value::Null),
            )?);
            continue;
        }
        if role == "tool" {
            let call_id = message.remove("tool_call_id").unwrap_or(Value::Null);
            let content = message
                .remove("content")
                .unwrap_or(Value::String(String::new()));
            if call_id
                .as_str()
                .is_some_and(|id| known_call_ids.iter().any(|known| known == id))
            {
                converted.push(json!({"role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": call_id, "content": content
                }]}));
            } else {
                converted.push(json!({"role": "user", "content": content}));
            }
            continue;
        }
        let mut content =
            content_to_anthropic_blocks(message.remove("content").unwrap_or(Value::Null))?;
        if let Some(calls) = message.remove("tool_calls") {
            for call in calls.as_array().into_iter().flatten() {
                let arguments = call["function"]
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let input = arguments
                    .as_str()
                    .and_then(|raw| serde_json::from_str(raw).ok())
                    .unwrap_or(arguments);
                content.push(json!({
                    "type": "tool_use",
                    "id": call.get("id").cloned().unwrap_or(Value::Null),
                    "name": call["function"].get("name").cloned().unwrap_or(Value::Null),
                    "input": input
                }));
            }
        }
        converted.push(json!({"role": role, "content": content}));
    }
    let system = (!system.is_empty()).then_some(Value::Array(system));
    Ok((system, Value::Array(converted)))
}

fn anthropic_messages_to_chat(messages: Value) -> Result<Vec<Value>, String> {
    let messages = messages
        .as_array()
        .ok_or_else(|| "Anthropic messages must be an array".to_owned())?;
    let mut converted = Vec::new();
    for item in messages {
        let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
        let content = item.get("content").cloned().unwrap_or(Value::Null);
        if let Some(blocks) = content.as_array() {
            let mut normal = Vec::new();
            let mut calls = Vec::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_use") => calls.push(json!({
                        "id": block.get("id").cloned().unwrap_or(Value::Null),
                        "type": "function",
                        "function": {
                            "name": block.get("name").cloned().unwrap_or(Value::Null),
                            "arguments": serde_json::to_string(block.get("input").unwrap_or(&json!({}))).unwrap_or_else(|_| "{}".to_owned())
                        }
                    })),
                    Some("tool_result") => converted.push(json!({
                        "role": "tool",
                        "tool_call_id": block.get("tool_use_id").cloned().unwrap_or(Value::Null),
                        "content": block.get("content").cloned().unwrap_or(Value::String(String::new()))
                    })),
                    Some("text") => normal.push(json!({"type": "text", "text": block.get("text").cloned().unwrap_or(Value::String(String::new()))})),
                    Some("image") => normal.push(anthropic_image_to_chat(block)),
                    _ => {}
                }
            }
            if !normal.is_empty() || !calls.is_empty() {
                let mut message = Map::new();
                message.insert("role".to_owned(), Value::String(role.to_owned()));
                message.insert("content".to_owned(), Value::Array(normal));
                if !calls.is_empty() {
                    message.insert("tool_calls".to_owned(), Value::Array(calls));
                }
                converted.push(Value::Object(message));
            }
        } else {
            converted.push(json!({"role": role, "content": content}));
        }
    }
    Ok(converted)
}

fn content_to_anthropic_blocks(content: Value) -> Result<Vec<Value>, String> {
    if content.is_null() {
        return Ok(Vec::new());
    }
    if let Some(text) = content.as_str() {
        return Ok(vec![json!({"type": "text", "text": text})]);
    }
    let parts = content
        .as_array()
        .ok_or_else(|| "Message content must be a string or array".to_owned())?;
    Ok(parts.iter().filter_map(|part| match part.get("type").and_then(Value::as_str) {
        Some("text") | Some("input_text") => Some(json!({"type": "text", "text": part.get("text").cloned().unwrap_or(Value::String(String::new()))})),
        Some("image_url") => chat_image_to_anthropic(part),
        Some("input_image") => responses_image_to_anthropic(part),
        Some("file") => chat_file_to_anthropic(part),
        _ => None,
    }).collect())
}

fn responses_content_to_chat(content: Value) -> Result<Value, String> {
    if content.is_string() {
        return Ok(content);
    }
    let parts = content
        .as_array()
        .ok_or_else(|| "Responses message content must be a string or array".to_owned())?;
    Ok(Value::Array(parts.iter().filter_map(|part| match part.get("type").and_then(Value::as_str) {
        Some("input_text") | Some("output_text") | Some("text") => Some(json!({"type": "text", "text": part.get("text").cloned().unwrap_or(Value::String(String::new()))})),
        Some("input_image") => Some(json!({"type": "image_url", "image_url": {"url": part.get("image_url").cloned().unwrap_or(Value::Null)}})),
        Some("input_file") => responses_file_to_chat(part),
        _ => None,
    }).collect()))
}

fn anthropic_document_to_responses(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    let mut file = Map::new();
    file.insert("type".to_owned(), Value::String("input_file".to_owned()));
    match source.get("type").and_then(Value::as_str) {
        Some("url") => {
            file.insert("file_url".to_owned(), source.get("url")?.clone());
        }
        Some("base64" | "text") => {
            file.insert("file_data".to_owned(), source.get("data")?.clone());
        }
        Some("file") => {
            file.insert("file_id".to_owned(), source.get("file_id")?.clone());
        }
        _ => return None,
    }
    if let Some(title) = block.get("title") {
        file.insert("filename".to_owned(), title.clone());
    }
    Some(Value::Object(file))
}

fn chat_file_to_responses(part: &Value) -> Option<Value> {
    let source = part.get("file")?.as_object()?;
    let mut file = Map::new();
    file.insert("type".to_owned(), Value::String("input_file".to_owned()));
    for field in ["file_data", "file_id", "file_url", "filename"] {
        if let Some(value) = source.get(field) {
            file.insert(field.to_owned(), value.clone());
        }
    }
    (file.len() > 1).then_some(Value::Object(file))
}

fn responses_file_to_chat(part: &Value) -> Option<Value> {
    let mut file = Map::new();
    for field in ["file_data", "file_id", "file_url", "filename"] {
        if let Some(value) = part.get(field) {
            file.insert(field.to_owned(), value.clone());
        }
    }
    (!file.is_empty()).then(|| json!({"type": "file", "file": file}))
}

fn chat_file_to_anthropic(part: &Value) -> Option<Value> {
    let file = part.get("file")?;
    let source = if let Some(url) = file.get("file_url") {
        json!({"type": "url", "url": url})
    } else {
        let data = file.get("file_data")?;
        json!({
            "type": "base64",
            "media_type": file.get("file_type")?.clone(),
            "data": data
        })
    };
    let mut document = json!({"type": "document", "source": source});
    if let Some(filename) = file.get("filename") {
        document["title"] = filename.clone();
    }
    Some(document)
}

fn chat_image_to_anthropic(part: &Value) -> Option<Value> {
    let url = part.get("image_url")?.get("url")?.as_str()?;
    data_url_to_anthropic(url)
        .or_else(|| Some(json!({"type": "image", "source": {"type": "url", "url": url}})))
}

fn responses_image_to_anthropic(part: &Value) -> Option<Value> {
    let url = part.get("image_url")?.as_str()?;
    data_url_to_anthropic(url)
        .or_else(|| Some(json!({"type": "image", "source": {"type": "url", "url": url}})))
}

fn data_url_to_anthropic(url: &str) -> Option<Value> {
    let raw = url.strip_prefix("data:")?;
    let (media_type, data) = raw.split_once(";base64,")?;
    Some(
        json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data}}),
    )
}

fn anthropic_image_to_chat(block: &Value) -> Value {
    let source = &block["source"];
    let url = if source.get("type").and_then(Value::as_str) == Some("base64") {
        format!(
            "data:{};base64,{}",
            source["media_type"]
                .as_str()
                .unwrap_or("application/octet-stream"),
            source["data"].as_str().unwrap_or("")
        )
    } else {
        source["url"].as_str().unwrap_or("").to_owned()
    };
    json!({"type": "image_url", "image_url": {"url": url}})
}

fn chat_tools_to_anthropic(tools: Value) -> Result<Value, String> {
    map_array(tools, |tool| {
        let function = &tool["function"];
        json!({
            "name": function.get("name").cloned().unwrap_or(Value::Null),
            "description": function.get("description").cloned().unwrap_or(Value::Null),
            "input_schema": function.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"}))
        })
    })
}

fn anthropic_tools_to_chat(tools: Value) -> Result<Value, String> {
    map_array(tools, |tool| {
        json!({"type": "function", "function": {
            "name": tool.get("name").cloned().unwrap_or(Value::Null),
            "description": tool.get("description").cloned().unwrap_or(Value::Null),
            "parameters": tool.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object"}))
        }})
    })
}

fn chat_tools_to_responses(tools: Value) -> Result<Value, String> {
    map_array(tools, |tool| {
        let function = &tool["function"];
        json!({
            "type": "function",
            "name": function.get("name").cloned().unwrap_or(Value::Null),
            "description": function.get("description").cloned().unwrap_or(Value::Null),
            "parameters": function.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"})),
            "strict": function.get("strict").cloned().unwrap_or(Value::Bool(false))
        })
    })
}

fn responses_tools_to_chat(tools: Value) -> Result<Value, String> {
    let tools = tools
        .as_array()
        .ok_or_else(|| "Tools must be an array".to_owned())?;
    Ok(Value::Array(
        tools
            .iter()
            .filter(|tool| tool.get("type").and_then(Value::as_str) == Some("function"))
            .map(|tool| {
                json!({"type": "function", "function": {
                    "name": tool.get("name").cloned().unwrap_or(Value::Null),
                    "description": tool.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"})),
                    "strict": tool.get("strict").cloned().unwrap_or(Value::Bool(false))
                }})
            })
            .collect(),
    ))
}

fn map_array(tools: Value, convert: impl Fn(&Value) -> Value) -> Result<Value, String> {
    let tools = tools
        .as_array()
        .ok_or_else(|| "Tools must be an array".to_owned())?;
    Ok(Value::Array(tools.iter().map(convert).collect()))
}

fn chat_response_format_to_responses(format: Value) -> Value {
    if format.get("type").and_then(Value::as_str) != Some("json_schema") {
        return format;
    }
    let Some(schema) = format.get("json_schema").and_then(Value::as_object) else {
        return format;
    };
    let mut converted = Map::new();
    converted.insert("type".to_owned(), Value::String("json_schema".to_owned()));
    converted.extend(schema.clone());
    Value::Object(converted)
}

fn responses_response_format_to_chat(format: Value) -> Value {
    if format.get("type").and_then(Value::as_str) != Some("json_schema") {
        return format;
    }
    let mut schema = format.as_object().cloned().unwrap_or_default();
    schema.remove("type");
    json!({"type": "json_schema", "json_schema": schema})
}

fn chat_tool_choice_to_responses(choice: Value) -> Value {
    match choice.pointer("/function/name") {
        Some(name) => json!({"type": "function", "name": name}),
        None => choice,
    }
}

fn responses_tool_choice_to_chat(choice: Value) -> Value {
    match choice.get("type").and_then(Value::as_str) {
        Some("function") => json!({
            "type": "function",
            "function": {"name": choice.get("name").cloned().unwrap_or(Value::Null)}
        }),
        _ => choice,
    }
}

fn chat_tool_choice_to_anthropic(choice: Value) -> Value {
    match choice.as_str() {
        Some("auto") => json!({"type": "auto"}),
        Some("required") => json!({"type": "any"}),
        Some("none") => json!({"type": "none"}),
        _ => choice
            .get("function")
            .and_then(|function| function.get("name"))
            .map_or(choice.clone(), |name| json!({"type": "tool", "name": name})),
    }
}

fn anthropic_tool_choice_to_chat(choice: Value) -> Value {
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => Value::String("auto".to_owned()),
        Some("any") => Value::String("required".to_owned()),
        Some("none") => Value::String("none".to_owned()),
        Some("tool") => {
            json!({"type": "function", "function": {"name": choice.get("name").cloned().unwrap_or(Value::Null)}})
        }
        _ => choice,
    }
}

fn remove_fields(object: &mut Map<String, Value>, fields: &[&str]) {
    for field in fields {
        object.remove(*field);
    }
}

fn rename(object: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = object.remove(from) {
        object.insert(to.to_owned(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::{Protocol, convert_request};
    use serde_json::Value;

    fn convert(input: Value, source: Protocol, target: Protocol) -> Value {
        serde_json::from_slice(
            &convert_request(&serde_json::to_vec(&input).unwrap(), source, target).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn chat_to_anthropic_maps_system_images_tools_and_limits() {
        let converted = convert(
            serde_json::json!({
                "model": "claude",
                "messages": [
                    {"role": "system", "content": "Be concise"},
                    {"role": "user", "content": [{"type": "text", "text": "look"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,abc"}}]},
                    {"role": "assistant", "content": null, "tool_calls": [{"id": "call-1", "type": "function", "function": {"name": "weather", "arguments": "{\"city\":\"HK\"}"}}]},
                    {"role": "tool", "tool_call_id": "call-1", "content": "sunny"}
                ],
                "max_completion_tokens": 300,
                "tools": [{"type": "function", "function": {"name": "weather", "description": "Weather", "parameters": {"type": "object"}}}]
            }),
            Protocol::OpenAiChat,
            Protocol::AnthropicMessages,
        );
        assert_eq!(converted["system"][0]["text"], "Be concise");
        assert_eq!(
            converted["messages"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );
        assert_eq!(
            converted["messages"][1]["content"][0]["input"]["city"],
            "HK"
        );
        assert_eq!(
            converted["messages"][2]["content"][0]["tool_use_id"],
            "call-1"
        );
        assert_eq!(converted["max_tokens"], 300);
        assert_eq!(converted["tools"][0]["input_schema"]["type"], "object");
    }

    #[test]
    fn document_sources_use_target_file_fields_without_wrapping_source_objects() {
        let responses = convert(
            serde_json::json!({
                "model": "gpt", "max_tokens": 50,
                "messages": [{"role": "user", "content": [
                    {"type": "document", "title": "report.pdf", "source": {"type": "url", "url": "https://example.com/report.pdf"}},
                    {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBERi0="}}
                ]}]
            }),
            Protocol::AnthropicMessages,
            Protocol::OpenAiResponses,
        );
        assert_eq!(responses["input"][0]["content"][0]["type"], "input_file");
        assert_eq!(
            responses["input"][0]["content"][0]["file_url"],
            "https://example.com/report.pdf"
        );
        assert_eq!(
            responses["input"][0]["content"][0]["filename"],
            "report.pdf"
        );
        assert_eq!(responses["input"][0]["content"][1]["file_data"], "JVBERi0=");
        assert!(!responses["input"][0]["content"][1]["file_data"].is_object());

        let anthropic = convert(
            serde_json::json!({
                "model": "claude", "max_output_tokens": 50,
                "input": [{"role": "user", "content": [{
                    "type": "input_file", "file_url": "https://example.com/guide.pdf", "filename": "guide.pdf"
                }]}]
            }),
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
        );
        assert_eq!(anthropic["messages"][0]["content"][0]["type"], "document");
        assert_eq!(
            anthropic["messages"][0]["content"][0]["source"]["type"],
            "url"
        );
        assert_eq!(
            anthropic["messages"][0]["content"][0]["source"]["url"],
            "https://example.com/guide.pdf"
        );
        assert_eq!(anthropic["messages"][0]["content"][0]["title"], "guide.pdf");
    }

    #[test]
    fn anthropic_to_responses_maps_tool_round_trip() {
        let converted = convert(
            serde_json::json!({
                "model": "gpt",
                "system": "Help",
                "messages": [
                    {"role": "assistant", "content": [{"type": "tool_use", "id": "call-1", "name": "search", "input": {"q": "rust"}}]},
                    {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call-1", "content": "result"}]}
                ],
                "max_tokens": 100
            }),
            Protocol::AnthropicMessages,
            Protocol::OpenAiResponses,
        );
        assert_eq!(converted["input"][0]["role"], "system");
        assert_eq!(converted["input"][1]["type"], "function_call");
        assert_eq!(converted["input"][2]["type"], "function_call_output");
        assert_eq!(converted["max_output_tokens"], 100);
    }

    #[test]
    fn anthropic_response_to_chat_preserves_text_tools_usage_and_finish_reason() {
        let input = serde_json::json!({
            "id": "msg_1", "type": "message", "model": "claude", "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "Checking"},
                {"type": "tool_use", "id": "call_1", "name": "weather", "input": {"city": "HK"}}
            ],
            "usage": {"input_tokens": 12, "output_tokens": 5, "cache_read_input_tokens": 3}
        });
        let converted: Value = serde_json::from_slice(
            &super::convert_response(
                &serde_json::to_vec(&input).unwrap(),
                Protocol::AnthropicMessages,
                Protocol::OpenAiChat,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(converted["choices"][0]["message"]["content"], "Checking");
        assert_eq!(
            converted["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "weather"
        );
        assert_eq!(converted["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            converted["usage"]["prompt_tokens_details"]["cached_tokens"],
            3
        );
    }

    #[test]
    fn responses_response_to_anthropic_preserves_output_and_usage() {
        let input = serde_json::json!({
            "id": "resp_1", "object": "response", "created_at": 10, "model": "gpt",
            "output": [
                {"type": "message", "content": [{"type": "output_text", "text": "Done"}]},
                {"type": "function_call", "call_id": "call_1", "name": "save", "arguments": "{\"ok\":true}"}
            ],
            "usage": {"input_tokens": 8, "output_tokens": 4, "input_tokens_details": {"cached_tokens": 2}}
        });
        let converted: Value = serde_json::from_slice(
            &super::convert_response(
                &serde_json::to_vec(&input).unwrap(),
                Protocol::OpenAiResponses,
                Protocol::AnthropicMessages,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(converted["content"][0]["text"], "Done");
        assert_eq!(converted["content"][1]["input"]["ok"], true);
        assert_eq!(converted["stop_reason"], "tool_use");
        assert_eq!(converted["usage"]["input_tokens"], 8);
    }

    #[test]
    fn responses_failure_incomplete_and_tool_endings_are_not_reported_as_successful_stop() {
        let failed = serde_json::json!({
            "id": "resp_failed", "object": "response", "status": "failed", "model": "gpt",
            "error": {"code": "server_error", "message": "overloaded"}, "output": []
        });
        let error = super::convert_response(
            &serde_json::to_vec(&failed).unwrap(),
            Protocol::OpenAiResponses,
            Protocol::OpenAiChat,
        )
        .unwrap_err();
        assert!(error.contains("overloaded"));

        let incomplete = serde_json::json!({
            "id": "resp_incomplete", "object": "response", "status": "incomplete", "model": "gpt",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "partial"}]}]
        });
        let chat: Value = serde_json::from_slice(
            &super::convert_response(
                &serde_json::to_vec(&incomplete).unwrap(),
                Protocol::OpenAiResponses,
                Protocol::OpenAiChat,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(chat["choices"][0]["finish_reason"], "length");

        let tool = serde_json::json!({
            "id": "resp_tool", "object": "response", "status": "completed", "model": "gpt",
            "output": [{"type": "function_call", "call_id": "call_1", "name": "lookup", "arguments": "{}"}]
        });
        let chat: Value = serde_json::from_slice(
            &super::convert_response(
                &serde_json::to_vec(&tool).unwrap(),
                Protocol::OpenAiResponses,
                Protocol::OpenAiChat,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn chat_to_responses_preserves_json_schema_property_order() {
        let input = br#"{"model":"gpt","messages":[{"role":"user","content":"hi"}],"max_tokens":42,"response_format":{"type":"json_schema","json_schema":{"name":"ordered","strict":true,"schema":{"type":"object","properties":{"zeta":{"type":"string"},"alpha":{"type":"integer"},"middle":{"type":"boolean"}}}}},"tool_choice":{"type":"function","function":{"name":"lookup"}},"tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object"},"strict":true}}],"frequency_penalty":1}"#;
        let output = String::from_utf8(
            convert_request(input, Protocol::OpenAiChat, Protocol::OpenAiResponses).unwrap(),
        )
        .unwrap();
        let converted: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(converted["max_output_tokens"], 42);
        assert_eq!(converted["text"]["format"]["type"], "json_schema");
        assert_eq!(converted["text"]["format"]["name"], "ordered");
        assert_eq!(converted["text"]["format"]["strict"], true);
        assert!(converted["text"]["format"].get("json_schema").is_none());
        assert_eq!(converted["tool_choice"]["name"], "lookup");
        assert_eq!(converted["tools"][0]["strict"], true);
        assert!(converted.get("max_tokens").is_none());
        assert!(converted.get("frequency_penalty").is_none());
        let zeta = output.find("\"zeta\"").unwrap();
        let alpha = output.find("\"alpha\"").unwrap();
        let middle = output.find("\"middle\"").unwrap();
        assert!(
            zeta < alpha && alpha < middle,
            "schema order changed: {output}"
        );
    }

    #[test]
    fn source_only_fields_and_non_function_tools_do_not_reach_target_protocols() {
        let chat = convert(
            serde_json::json!({
                "model": "gpt", "input": "hi", "max_output_tokens": 20,
                "max_tool_calls": 3, "prompt_cache_key": "private-cache",
                "tools": [
                    {"type": "web_search_preview"},
                    {"type": "function", "name": "lookup", "parameters": {"type": "object"}, "strict": true}
                ],
                "text": {"format": {"type": "json_schema", "name": "answer", "schema": {"type": "object"}}}
            }),
            Protocol::OpenAiResponses,
            Protocol::OpenAiChat,
        );
        assert!(chat.get("max_tool_calls").is_none());
        assert!(chat.get("prompt_cache_key").is_none());
        assert_eq!(chat["tools"].as_array().unwrap().len(), 1);
        assert_eq!(chat["tools"][0]["function"]["name"], "lookup");
        assert_eq!(chat["tools"][0]["function"]["strict"], true);
        assert_eq!(chat["response_format"]["json_schema"]["name"], "answer");

        let anthropic = convert(
            serde_json::json!({
                "model": "gpt", "messages": [{"role": "user", "content": "hi"}], "max_tokens": 20,
                "thinking": {"type": "enabled", "budget_tokens": 1024}, "top_k": 5, "container": "ctx"
            }),
            Protocol::AnthropicMessages,
            Protocol::OpenAiChat,
        );
        assert!(anthropic.get("thinking").is_none());
        assert!(anthropic.get("top_k").is_none());
        assert!(anthropic.get("container").is_none());
    }

    #[test]
    fn anthropic_to_responses_preserves_interleaved_block_order() {
        let converted = convert(
            serde_json::json!({
                "model": "gpt", "max_tokens": 100,
                "messages": [{"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "t0", "signature": "s0"},
                    {"type": "text", "text": "step 0"},
                    {"type": "tool_use", "id": "call0", "name": "weather", "input": {"city": "sf"}},
                    {"type": "redacted_thinking", "data": "encrypted"},
                    {"type": "text", "text": "step 1"},
                    {"type": "tool_use", "id": "call1", "name": "weather", "input": {"city": "la"}}
                ]}]
            }),
            Protocol::AnthropicMessages,
            Protocol::OpenAiResponses,
        );
        let types: Vec<_> = converted["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                item.get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("message")
            })
            .collect();
        assert_eq!(
            types,
            [
                "reasoning",
                "message",
                "function_call",
                "reasoning",
                "message",
                "function_call"
            ]
        );
    }

    #[test]
    fn anthropic_tool_result_without_content_is_not_dropped() {
        let converted = convert(
            serde_json::json!({
                "model": "gpt", "max_tokens": 100,
                "messages": [{"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call0", "is_error": true}]}]
            }),
            Protocol::AnthropicMessages,
            Protocol::OpenAiResponses,
        );
        assert_eq!(converted["input"][0]["type"], "function_call_output");
        assert_eq!(converted["input"][0]["call_id"], "call0");
        assert_eq!(converted["input"][0]["output"], "");
    }

    #[test]
    fn orphan_responses_tool_output_becomes_anthropic_user_text() {
        let converted = convert(
            serde_json::json!({
                "model": "claude", "max_output_tokens": 100,
                "input": [{"type": "function_call_output", "call_id": "orphan", "output": "Sunny"}]
            }),
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
        );
        assert_eq!(converted["messages"][0]["role"], "user");
        assert_eq!(converted["messages"][0]["content"], "Sunny");
    }

    #[test]
    fn responses_instructions_and_named_tool_choice_reach_anthropic() {
        let converted = convert(
            serde_json::json!({
                "model": "claude", "instructions": "Be precise", "input": "question", "max_output_tokens": 50,
                "tools": [{"type": "function", "name": "lookup", "parameters": {"type": "object"}}],
                "tool_choice": {"type": "function", "name": "lookup"}
            }),
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
        );
        assert_eq!(converted["system"][0]["text"], "Be precise");
        assert_eq!(converted["tool_choice"]["type"], "tool");
        assert_eq!(converted["tool_choice"]["name"], "lookup");
        assert_eq!(converted["tools"][0]["name"], "lookup");
        assert!(converted.get("instructions").is_none());
    }

    #[test]
    fn same_protocol_preserves_exact_bytes() {
        let input = br#"{ "model": "native", "vendor_field": 1 }"#;
        assert_eq!(
            convert_request(input, Protocol::OpenAiChat, Protocol::OpenAiChat).unwrap(),
            input
        );
    }
}

use super::{Protocol, convert_request, convert_response};
use serde_json::{Value, json};

fn request(value: Value, source: Protocol, target: Protocol) -> Value {
    serde_json::from_slice(
        &convert_request(&serde_json::to_vec(&value).unwrap(), source, target).unwrap(),
    )
    .unwrap()
}

#[test]
fn responses_text_within_a_tool_batch_does_not_separate_calls_from_results() {
    // PROXY-53: text and calls belong to one assistant turn until a result or
    // another role arrives, including history saved by older converters.
    for text in ["", " ", "Checking the other file"] {
        let input = json!({"input":[
            {"type":"function_call","call_id":"a","name":"read","arguments":"{}"},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]},
            {"type":"function_call","call_id":"b","name":"read","arguments":"{}"},
            {"type":"message","role":"assistant","content":"after"},
            {"type":"function_call_output","call_id":"a","output":"one"},
            {"type":"function_call_output","call_id":"b","output":"two"}
        ]});
        let chat = request(
            input.clone(),
            Protocol::OpenAiResponses,
            Protocol::OpenAiChat,
        );
        assert_eq!(chat["messages"].as_array().unwrap().len(), 3);
        assert_eq!(
            chat["messages"][0]["tool_calls"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            chat["messages"][0]["content"],
            json!([
                {"type":"text","text":text}, {"type":"text","text":"after"}
            ])
        );
        assert_eq!(chat["messages"][1]["tool_call_id"], "a");
        assert_eq!(chat["messages"][2]["tool_call_id"], "b");
        let anthropic = request(
            input,
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
        );
        assert_eq!(anthropic["messages"].as_array().unwrap().len(), 2);
        assert_eq!(
            anthropic["messages"][1]["content"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}

#[test]
fn chat_assistant_parts_are_converted_even_with_tools() {
    // PROXY-54: both branches must produce Responses parts, not raw Chat parts.
    for with_tools in [false, true] {
        let mut message = json!({"role":"assistant","content":[{"type":"text","text":"checking"}]});
        if with_tools {
            message["tool_calls"] =
                json!([{"id":"a","type":"function","function":{"name":"read","arguments":"{}"}}]);
        }
        let result = request(
            json!({"messages":[message]}),
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
        );
        assert_eq!(
            result["input"][0]["content"],
            json!([{"type":"output_text","text":"checking"}])
        );
        if with_tools {
            assert_eq!(result["input"][1]["call_id"], "a");
        }
    }
    let result = request(
        json!({"messages":[{"role":"user","content":[
            {"type":"text","text":"look"},
            {"type":"image_url","image_url":{"url":"https://example.test/a.png","detail":"high"}}
        ]}]}),
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
    );
    assert_eq!(result["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(result["input"][0]["content"][1]["detail"], "high");
}

#[test]
fn anthropic_opaque_reasoning_never_becomes_openai_encrypted_state() {
    // PROXY-55: a signature is provider state, not a portable encrypted payload.
    for target in [Protocol::OpenAiChat, Protocol::OpenAiResponses] {
        let result = request(
            json!({"messages":[
                {"role":"user","content":"question"},
                {"role":"assistant","content":[
                    {"type":"thinking","thinking":"private-thought","signature":"private-signature"},
                    {"type":"redacted_thinking","data":"private-data"},
                    {"type":"text","text":"answer"}
                ]}
            ]}),
            Protocol::AnthropicMessages,
            target,
        );
        let raw = result.to_string();
        assert!(!raw.contains("private-"));
        assert!(!raw.contains("encrypted_content"));
        assert!(raw.contains("answer"));
    }
}

#[test]
fn responses_server_side_history_is_not_silently_discarded() {
    // PROXY-56: unsupported state is an explicit error, not a successful request
    // against a shortened history. Native requests stay byte-identical.
    for field in ["previous_response_id", "conversation"] {
        let mut input = json!({"input":"continue"});
        input[field] = json!("private-state-id");
        let bytes = serde_json::to_vec(&input).unwrap();
        for target in [Protocol::OpenAiChat, Protocol::AnthropicMessages] {
            let error = convert_request(&bytes, Protocol::OpenAiResponses, target).unwrap_err();
            assert!(error.contains(field));
            assert!(!error.contains("private-state-id"));
        }
        assert_eq!(
            convert_request(&bytes, Protocol::OpenAiResponses, Protocol::OpenAiResponses).unwrap(),
            bytes
        );
    }
}

#[test]
fn request_controls_and_tool_only_messages_use_target_shapes() {
    // PROXY-61: never leak mutually exclusive/source-only controls to the target.
    let chat = json!({"max_tokens":20,"max_completion_tokens":30,"stop":"END","parallel_tool_calls":false,"reasoning_effort":"high",
        "tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}],
        "messages":[{"role":"assistant","content":"","tool_calls":[{"id":"a","type":"function","function":{"name":"lookup","arguments":"{}"}}]}]
    });
    let anthropic = request(
        chat.clone(),
        Protocol::OpenAiChat,
        Protocol::AnthropicMessages,
    );
    assert_eq!(anthropic["max_tokens"], 30);
    assert_eq!(anthropic["stop_sequences"], json!(["END"]));
    assert_eq!(anthropic["tool_choice"]["disable_parallel_tool_use"], true);
    assert!(anthropic.get("parallel_tool_calls").is_none());
    assert!(anthropic.get("reasoning_effort").is_none());
    assert_eq!(
        anthropic["messages"][0]["content"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(anthropic["messages"][0]["content"][0]["type"], "tool_use");
    let responses = request(chat, Protocol::OpenAiChat, Protocol::OpenAiResponses);
    assert_eq!(responses["max_output_tokens"], 30);
    assert!(responses.get("max_tokens").is_none());
    let reverse = request(anthropic, Protocol::AnthropicMessages, Protocol::OpenAiChat);
    assert_eq!(reverse["parallel_tool_calls"], false);
    assert!(reverse["messages"][0]["content"].is_null());
}

#[test]
fn documents_survive_all_supported_routes_without_losing_mime_data() {
    // PROXY-62: inline file_data uses a data URI; Anthropic carries MIME separately.
    let source = json!({"messages":[{"role":"user","content":[
        {"type":"document","title":"report.pdf","source":{"type":"base64","media_type":"application/pdf","data":"JVBERi0="}},
        {"type":"document","title":"remote.pdf","source":{"type":"url","url":"https://example.test/report.pdf"}}
    ]}]});
    for target in [Protocol::OpenAiChat, Protocol::OpenAiResponses] {
        let converted = request(source.clone(), Protocol::AnthropicMessages, target);
        let parts = if target == Protocol::OpenAiChat {
            &converted["messages"][0]["content"]
        } else {
            &converted["input"][0]["content"]
        };
        assert_eq!(parts.as_array().unwrap().len(), 2);
        let file = if target == Protocol::OpenAiChat {
            &parts[0]["file"]
        } else {
            &parts[0]
        };
        assert_eq!(file["file_data"], "data:application/pdf;base64,JVBERi0=");
        assert_eq!(file["filename"], "report.pdf");
        let roundtrip = request(converted, target, Protocol::AnthropicMessages);
        assert_eq!(
            roundtrip["messages"][0]["content"],
            source["messages"][0]["content"]
        );
    }
    let unsupported = serde_json::to_vec(&json!({"messages":[{"role":"user","content":[{"type":"file","file":{"file_data":"missing-mime-private-data"}}]}]})).unwrap();
    let error = convert_request(
        &unsupported,
        Protocol::OpenAiChat,
        Protocol::AnthropicMessages,
    )
    .unwrap_err();
    assert!(!error.contains("private-data"));
}

#[test]
fn refusals_are_not_converted_to_empty_successful_answers() {
    // PROXY-63: preserve the reason in standard OpenAI refusal fields, or visible
    // Anthropic text where that content-block kind has no equivalent.
    for source in [Protocol::OpenAiChat, Protocol::OpenAiResponses] {
        let history = if source == Protocol::OpenAiChat {
            json!({"messages":[{"role":"assistant","content":null,"refusal":"Cannot help"}]})
        } else {
            json!({"input":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"Cannot help"}]}]})
        };
        for target in [
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
        ] {
            assert!(
                request(history.clone(), source, target)
                    .to_string()
                    .contains("Cannot help")
            );
        }
        let value = if source == Protocol::OpenAiChat {
            json!({"choices":[{"message":{"role":"assistant","content":null,"refusal":"Cannot help"},"finish_reason":"stop"}]})
        } else {
            json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"Cannot help"}]}]})
        };
        for target in [
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
        ] {
            let bytes =
                convert_response(&serde_json::to_vec(&value).unwrap(), source, target).unwrap();
            let result: Value = serde_json::from_slice(&bytes).unwrap();
            let reason = match target {
                Protocol::OpenAiChat => &result["choices"][0]["message"]["refusal"],
                Protocol::OpenAiResponses => &result["output"][0]["content"][0]["refusal"],
                Protocol::AnthropicMessages => &result["content"][0]["text"],
            };
            assert_eq!(reason, "Cannot help");
        }
    }
}

#[test]
fn unfinished_responses_json_does_not_become_a_successful_chat_answer() {
    // PROXY-60: queued/background/cancelled replies are not completed answers.
    for status in ["queued", "in_progress", "cancelled"] {
        let bytes = serde_json::to_vec(&json!({"status":status,"output":[]})).unwrap();
        for target in [Protocol::OpenAiChat, Protocol::AnthropicMessages] {
            assert!(convert_response(&bytes, Protocol::OpenAiResponses, target).is_err());
        }
        assert_eq!(
            convert_response(&bytes, Protocol::OpenAiResponses, Protocol::OpenAiResponses).unwrap(),
            bytes
        );
    }
}

#[test]
fn anthropic_argument_objects_preserve_large_json_numbers() {
    // PROXY-57: converting to an object must not round caller/tool data either.
    let raw = "{\"id\":123456789012345678901234567890,\"x\":1.00}";
    let converted = request(
        json!({"messages":[{"role":"assistant","content":null,"tool_calls":[
            {"id":"a","type":"function","function":{"name":"lookup","arguments":raw}}
        ]}]}),
        Protocol::OpenAiChat,
        Protocol::AnthropicMessages,
    );
    assert_eq!(
        converted["messages"][0]["content"][0]["input"].to_string(),
        raw
    );
    let bytes = convert_response(
        &serde_json::to_vec(&json!({"status":"completed","output":[
            {"type":"function_call","call_id":"a","name":"lookup","arguments":raw}
        ]}))
        .unwrap(),
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["content"][0]["input"].to_string(), raw);
}

#[test]
fn json_tool_arguments_keep_exact_bytes_between_openai_protocols() {
    // PROXY-57: parsing and re-serializing tool arguments can round large numbers,
    // remove whitespace, or unwrap a JSON string. Only Anthropic needs an object.
    for raw in [
        "{ \"id\": 123456789012345678901234567890, \"x\":1.00 }",
        "\"literal\"",
        "{\"x\":",
        "",
    ] {
        for (source, target) in [
            (Protocol::OpenAiChat, Protocol::OpenAiResponses),
            (Protocol::OpenAiResponses, Protocol::OpenAiChat),
        ] {
            let value = if source == Protocol::OpenAiChat {
                json!({"choices":[{"message":{"tool_calls":[{"id":"a","function":{"name":"read","arguments":raw}}]},"finish_reason":"tool_calls"}]})
            } else {
                json!({"status":"completed","output":[{"type":"function_call","call_id":"a","name":"read","arguments":raw}]})
            };
            let bytes =
                convert_response(&serde_json::to_vec(&value).unwrap(), source, target).unwrap();
            let result: Value = serde_json::from_slice(&bytes).unwrap();
            let arguments = if target == Protocol::OpenAiResponses {
                &result["output"][0]["arguments"]
            } else {
                &result["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
            };
            assert_eq!(arguments, raw);
        }
    }
}

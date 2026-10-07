use super::StreamConverter;
use crate::protocol::{Protocol, convert_response};
use serde_json::{Value, json};

fn frame(value: Value) -> String {
    format!("data: {value}\n\n")
}

fn events(bytes: &[u8]) -> Vec<Value> {
    std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|data| *data != "[DONE]")
        .map(|data| serde_json::from_str(data).unwrap())
        .collect()
}

fn chat_delta(delta: Value, reason: Value) -> Value {
    json!({"id":"c1", "model":"m", "choices":[{"index":0,"delta":delta,"finish_reason":reason}]})
}

fn convert_stream(input: &str, source: Protocol, target: Protocol) -> (Vec<Value>, Value) {
    let mut converter = StreamConverter::new_aggregating(source, target);
    let mut output = converter.push(input.as_bytes()).unwrap();
    output.extend(converter.finish().unwrap());
    let aggregate = serde_json::from_slice(&converter.non_stream_response().unwrap()).unwrap();
    (events(&output), aggregate)
}

#[test]
fn incomplete_reason_survives_json_sse_and_aggregation() {
    // PROXY-48: a token-limited answer never becomes a normal stop, even with tools.
    for with_tool in [false, true] {
        let mut output = vec![
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"partial"}]}),
        ];
        let mut input =
            frame(json!({"type":"response.created","response":{"id":"r1","model":"m"}}));
        input +=
            &frame(json!({"type":"response.output_text.delta","output_index":0,"delta":"partial"}));
        if with_tool {
            let tool =
                json!({"type":"function_call","call_id":"call_1","name":"f","arguments":"{}"});
            output.push(tool.clone());
            input +=
                &frame(json!({"type":"response.output_item.added","output_index":1,"item":tool}));
            input += &frame(
                json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{}"}),
            );
        }
        let response = json!({"id":"r1","model":"m","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":output});
        input += &frame(json!({"type":"response.incomplete","response":response}));
        for (target, field, expected) in [
            (Protocol::OpenAiChat, "/choices/0/finish_reason", "length"),
            (Protocol::AnthropicMessages, "/stop_reason", "max_tokens"),
        ] {
            let converted: Value = serde_json::from_slice(
                &convert_response(
                    &serde_json::to_vec(&response).unwrap(),
                    Protocol::OpenAiResponses,
                    target,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(converted.pointer(field).unwrap(), expected);
            let (events, aggregate) = convert_stream(&input, Protocol::OpenAiResponses, target);
            assert_eq!(aggregate.pointer(field).unwrap(), expected);
            assert!(events.iter().any(|event| {
                event
                    .pointer("/choices/0/finish_reason")
                    .or_else(|| event.pointer("/delta/stop_reason"))
                    == Some(&json!(expected))
            }));
        }
    }
}

#[test]
fn chat_final_usage_is_emitted_only_after_the_terminal_marker() {
    // PROXY-48: Chat may put usage in a separate chunk after finish_reason.
    for reason in ["stop", "length", "content_filter"] {
        for target in [
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
        ] {
            // Anthropic has no portable content-filter stop equivalent in this adapter.
            if reason == "content_filter" && target == Protocol::AnthropicMessages {
                continue;
            }
            let mut converter = StreamConverter::new_aggregating(Protocol::OpenAiChat, target);
            let early = converter
                .push(frame(chat_delta(json!({"content":"partial"}), json!(reason))).as_bytes())
                .unwrap();
            assert!(
                !events(&early).iter().any(|event| {
                    matches!(
                        event["type"].as_str(),
                        Some("response.completed" | "response.incomplete" | "message_delta")
                    ) || event
                        .pointer("/choices/0/finish_reason")
                        .is_some_and(|value| !value.is_null())
                }),
                "must wait for final usage"
            );
            converter.push(frame(json!({"id":"c1","model":"m","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":3}}})).as_bytes()).unwrap();
            let final_bytes = converter.push(b"data: [DONE]\n\n").unwrap();
            let final_events = events(&final_bytes);
            assert!(converter.finish().unwrap().is_empty());
            let aggregate: Value =
                serde_json::from_slice(&converter.non_stream_response().unwrap()).unwrap();
            match target {
                Protocol::OpenAiResponses => {
                    let terminal = final_events.last().unwrap();
                    assert_eq!(terminal["response"], aggregate);
                    assert_eq!(aggregate["usage"]["total_tokens"], 15);
                    assert_eq!(
                        aggregate["usage"]["input_tokens_details"]["cached_tokens"],
                        3
                    );
                    assert_eq!(
                        aggregate["status"],
                        if reason == "stop" {
                            "completed"
                        } else {
                            "incomplete"
                        }
                    );
                    if reason != "stop" {
                        assert_eq!(
                            aggregate["incomplete_details"]["reason"],
                            if reason == "length" {
                                "max_output_tokens"
                            } else {
                                reason
                            }
                        );
                    }
                }
                Protocol::OpenAiChat => {
                    let terminal = final_events.last().unwrap();
                    assert_eq!(terminal["usage"], aggregate["usage"]);
                    assert_eq!(terminal["usage"]["total_tokens"], 15);
                    assert_eq!(terminal["choices"][0]["finish_reason"], reason);
                }
                Protocol::AnthropicMessages => {
                    let terminal = final_events
                        .iter()
                        .find(|e| e["type"] == "message_delta")
                        .unwrap();
                    assert_eq!(terminal["usage"], aggregate["usage"]);
                    assert_eq!(terminal["usage"]["input_tokens"], 7);
                    assert_eq!(terminal["usage"]["output_tokens"], 5);
                    assert_eq!(terminal["usage"]["cache_read_input_tokens"], 3);
                }
            }
        }
    }
}

#[test]
fn finish_without_done_or_followed_by_failure_never_emits_success() {
    // PROXY-30 / PROXY-48: finish_reason is not the transport terminal marker.
    for target in [
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
    ] {
        for failure in [false, true] {
            let mut converter = StreamConverter::new_aggregating(Protocol::OpenAiChat, target);
            let mut output = converter
                .push(frame(chat_delta(json!({"content":"partial"}), json!("stop"))).as_bytes())
                .unwrap();
            if failure {
                output.extend(
                    converter
                        .push(frame(json!({"error":{"message":"fixture failure"}})).as_bytes())
                        .unwrap(),
                );
            }
            output.extend(converter.finish().unwrap());
            assert!(converter.has_failed());
            assert!(converter.non_stream_response().is_err());
            let output = String::from_utf8(output).unwrap();
            assert!(!output.contains("response.completed"));
            assert!(!output.contains("message_stop"));
            assert!(!output.contains("\"finish_reason\":\"stop\""));
        }
    }
}

#[test]
fn responses_tools_have_stable_indices_ids_and_complete_lifecycles() {
    // PROXY-49: allocate target indices independently of source block/tool indices.
    for text_position in ["none", "before", "after"] {
        let mut input = String::new();
        if text_position == "before" {
            input += &frame(chat_delta(json!({"content":"Checking"}), Value::Null));
        }
        for (index, id) in [(3, "call_a"), (8, "call_b")] {
            input += &frame(chat_delta(
                json!({"tool_calls":[{"index":index,"id":id,"function":{"name":"lookup","arguments":"{\"n\":"}}]}),
                Value::Null,
            ));
        }
        // Interleaved arguments keep their original call identity.
        for (index, delta) in [(8, "2}"), (3, "1}")] {
            input += &frame(chat_delta(
                json!({"tool_calls":[{"index":index,"function":{"arguments":delta}}]}),
                Value::Null,
            ));
        }
        if text_position == "after" {
            input += &frame(chat_delta(json!({"content":"Checking"}), Value::Null));
        }
        input += &frame(chat_delta(json!({}), json!("tool_calls")));
        input += "data: [DONE]\n\n";
        let (events, aggregate) =
            convert_stream(&input, Protocol::OpenAiChat, Protocol::OpenAiResponses);
        for (sequence, event) in events.iter().enumerate() {
            assert_eq!(event["sequence_number"], sequence);
        }
        let items = aggregate["output"].as_array().unwrap();
        let starts: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == "response.output_item.added")
            .collect();
        let ends: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == "response.output_item.done")
            .collect();
        assert_eq!(starts.len(), items.len());
        assert_eq!(ends.len(), items.len());
        for (index, item) in items.iter().enumerate() {
            assert_eq!(starts[index]["output_index"], index);
            assert_eq!(starts[index]["item"]["id"], item["id"]);
            assert_eq!(ends[index]["output_index"], index);
            assert_eq!(ends[index]["item"], *item);
        }
        for event in &events {
            if let Some(index) = event["output_index"].as_u64()
                && !matches!(
                    event["type"].as_str(),
                    Some("response.output_item.added" | "response.output_item.done")
                )
            {
                assert_eq!(event["item_id"], items[index as usize]["id"]);
            }
        }
        let tools: Vec<_> = items
            .iter()
            .filter(|item| item["type"] == "function_call")
            .collect();
        assert_eq!(tools[0]["call_id"], "call_a");
        assert_eq!(tools[0]["arguments"], "{\"n\":1}");
        assert_eq!(tools[1]["call_id"], "call_b");
        assert_eq!(tools[1]["arguments"], "{\"n\":2}");
        assert_eq!(
            events
                .iter()
                .filter(|e| e["type"] == "response.function_call_arguments.done")
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e["type"] == "response.output_text.done")
                .count(),
            usize::from(text_position != "none")
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e["type"] == "response.content_part.done")
                .count(),
            usize::from(text_position != "none")
        );
    }
}

#[test]
fn json_and_stream_limits_to_responses_have_the_same_status_and_usage() {
    // PROXY-48: JSON responses and aggregated SSE use the same terminal mapping.
    let chat = json!({"id":"c1","model":"m","choices":[{"message":{"role":"assistant","content":"partial"},"finish_reason":"length"}],"usage":{"prompt_tokens":10,"completion_tokens":5}});
    let anthropic = json!({"id":"c1","model":"m","content":[{"type":"text","text":"partial"}],"stop_reason":"max_tokens","usage":{"input_tokens":10,"output_tokens":5}});
    let chat_stream = frame(chat_delta(json!({"content":"partial"}), json!("length")))
        + &frame(json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5}}))
        + "data: [DONE]\n\n";
    let anthropic_stream = frame(json!({"type":"message_start","message":{"id":"c1","model":"m","usage":{"input_tokens":10}}}))
        + &frame(json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}))
        + &frame(json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":4}}))
        // A usage-only update must not clear the earlier max_tokens reason.
        + &frame(json!({"type":"message_delta","delta":{},"usage":{"output_tokens":5}}))
        + &frame(json!({"type":"message_stop"}));
    for (source, response, stream) in [
        (Protocol::OpenAiChat, chat, chat_stream),
        (Protocol::AnthropicMessages, anthropic, anthropic_stream),
    ] {
        let converted: Value = serde_json::from_slice(
            &convert_response(
                &serde_json::to_vec(&response).unwrap(),
                source,
                Protocol::OpenAiResponses,
            )
            .unwrap(),
        )
        .unwrap();
        let (events, aggregate) = convert_stream(&stream, source, Protocol::OpenAiResponses);
        assert_eq!(converted["status"], "incomplete");
        assert_eq!(
            converted["incomplete_details"]["reason"],
            "max_output_tokens"
        );
        for field in ["status", "incomplete_details", "output", "usage"] {
            assert_eq!(converted[field], aggregate[field], "{field}");
        }
        assert_eq!(events.last().unwrap()["type"], "response.incomplete");
        assert_eq!(events.last().unwrap()["response"], aggregate);
    }
}

#[test]
fn unrepresentable_incomplete_reason_is_not_reported_as_normal_stop() {
    // PROXY-48: preserve opaque incomplete details in Responses, fail conversion
    // when the other protocol cannot express the terminal meaning.
    for details in [
        Value::Null,
        json!({"reason":"future_reason","detail":"fixture"}),
    ] {
        let response = json!({"id":"r1","model":"m","status":"incomplete","incomplete_details":details,"output":[]});
        let input = frame(json!({"type":"response.incomplete","response":response}));
        let (_, aggregate) =
            convert_stream(&input, Protocol::OpenAiResponses, Protocol::OpenAiResponses);
        assert_eq!(aggregate["status"], "incomplete");
        assert_eq!(aggregate["incomplete_details"], details);
        for target in [Protocol::OpenAiChat, Protocol::AnthropicMessages] {
            assert!(
                convert_response(
                    &serde_json::to_vec(&response).unwrap(),
                    Protocol::OpenAiResponses,
                    target
                )
                .is_err()
            );
            let mut converter = StreamConverter::new(Protocol::OpenAiResponses, target);
            assert!(
                converter
                    .push(input.as_bytes())
                    .unwrap_err()
                    .contains("cannot be represented")
            );
        }
    }
    let response = json!({"id":"r1","model":"m","status":"incomplete","incomplete_details":{"reason":"content_filter"},"output":[]});
    let chat: Value = serde_json::from_slice(
        &convert_response(
            &serde_json::to_vec(&response).unwrap(),
            Protocol::OpenAiResponses,
            Protocol::OpenAiChat,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(chat["choices"][0]["finish_reason"], "content_filter");
    assert!(
        convert_response(
            &serde_json::to_vec(&response).unwrap(),
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages
        )
        .is_err()
    );
}

#[test]
fn provider_error_events_are_not_overwritten_by_a_later_done_marker() {
    // PROXY-30 / PROXY-48: source errors win over transport completion.
    for (source, error) in [
        (Protocol::OpenAiChat, json!({"error":{"message":"failed"}})),
        (
            Protocol::AnthropicMessages,
            json!({"type":"error","error":{"message":"failed"}}),
        ),
        (
            Protocol::OpenAiResponses,
            json!({"type":"error","message":"failed"}),
        ),
    ] {
        for target in [
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            Protocol::AnthropicMessages,
        ] {
            let mut converter = StreamConverter::new_aggregating(source, target);
            let output = converter
                .push((frame(error.clone()) + "data: [DONE]\n\n").as_bytes())
                .unwrap();
            assert!(converter.has_failed());
            assert!(converter.non_stream_response().is_err());
            assert!(converter.finish().unwrap().is_empty());
            let events = events(&output);
            assert_eq!(events.len(), 1);
            assert!(events[0].get("error").is_some() || events[0]["type"] == "response.failed");
        }
    }
}

#[test]
fn incomplete_tool_arguments_are_never_repaired_or_labelled_complete() {
    // PROXY-48 / PROXY-49: preserve partial argument bytes, not an invented {}.
    for arguments in ["", "{\"x\":"] {
        let input = frame(chat_delta(
            json!({"tool_calls":[{"index":0,"id":"call_1","function":{"name":"f","arguments":arguments}}]}),
            json!("length"),
        )) + "data: [DONE]\n\n";
        let (events, aggregate) =
            convert_stream(&input, Protocol::OpenAiChat, Protocol::OpenAiResponses);
        assert_eq!(aggregate["status"], "incomplete");
        assert_eq!(aggregate["output"][0]["status"], "incomplete");
        assert_eq!(aggregate["output"][0]["arguments"], arguments);
        let done = events
            .iter()
            .find(|e| e["type"] == "response.function_call_arguments.done")
            .unwrap();
        assert_eq!(done["arguments"], arguments);
        let (events, aggregate) =
            convert_stream(&input, Protocol::OpenAiChat, Protocol::OpenAiChat);
        assert_eq!(aggregate["choices"][0]["finish_reason"], "length");
        assert_eq!(
            aggregate["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            arguments
        );
        let mut anthropic =
            StreamConverter::new_aggregating(Protocol::OpenAiChat, Protocol::AnthropicMessages);
        anthropic.push(input.as_bytes()).unwrap();
        anthropic.finish().unwrap();
        assert!(
            anthropic
                .non_stream_response()
                .unwrap_err()
                .contains("input object")
        );
        let deltas: String = events
            .iter()
            .filter_map(|e| {
                e.pointer("/choices/0/delta/tool_calls/0/function/arguments")
                    .and_then(Value::as_str)
            })
            .collect();
        assert_eq!(deltas, arguments);
    }
}

#[test]
fn anthropic_source_block_indices_do_not_leak_into_responses_output_indices() {
    // PROXY-49: source block indices may include unsupported blocks before tools.
    let input = frame(json!({"type":"message_start","message":{"id":"m1","model":"m"}}))
        + &frame(
            json!({"type":"content_block_start","index":4,"content_block":{"type":"tool_use","id":"call_1","name":"f","input":{}}}),
        )
        + &frame(
            json!({"type":"content_block_delta","index":4,"delta":{"type":"input_json_delta","partial_json":"{\"x\":1}"}}),
        )
        + &frame(
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
        )
        + &frame(json!({"type":"message_stop"}));
    let (events, aggregate) = convert_stream(
        &input,
        Protocol::AnthropicMessages,
        Protocol::OpenAiResponses,
    );
    for event in events.iter().filter(|e| e.get("output_index").is_some()) {
        assert_eq!(event["output_index"], 0);
    }
    assert_eq!(aggregate["output"][0]["call_id"], "call_1");
    assert_eq!(aggregate["output"][0]["arguments"], "{\"x\":1}");
}

#[test]
fn normal_stops_keep_final_usage_in_every_conversion_direction() {
    // PROXY-48: waiting for a terminal marker does not lose ordinary finishes.
    let chat = frame(chat_delta(json!({"content":"ok"}), json!("stop")))
        + &frame(
            json!({"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":3}}}),
        )
        + "data: [DONE]\n\n";
    let anthropic = frame(
        json!({"type":"message_start","message":{"id":"m1","model":"m","usage":{"input_tokens":9,"cache_read_input_tokens":3}}}),
    ) + &frame(
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
    ) + &frame(
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
    ) + &frame(json!({"type":"message_stop"}));
    let responses = frame(json!({"type":"response.created","response":{"id":"r1","model":"m"}}))
        + &frame(json!({"type":"response.output_text.delta","output_index":0,"delta":"ok"}))
        + &frame(
            json!({"type":"response.completed","response":{"id":"r1","model":"m","status":"completed","usage":{"input_tokens":12,"output_tokens":2,"input_tokens_details":{"cached_tokens":3}}}}),
        );
    for (source, input) in [
        (Protocol::OpenAiChat, chat),
        (Protocol::AnthropicMessages, anthropic),
        (Protocol::OpenAiResponses, responses),
    ] {
        for target in [
            Protocol::OpenAiChat,
            Protocol::AnthropicMessages,
            Protocol::OpenAiResponses,
        ] {
            let (events, aggregate) = convert_stream(&input, source, target);
            let terminal = events
                .iter()
                .find(|event| match target {
                    Protocol::OpenAiChat => event
                        .pointer("/choices/0/finish_reason")
                        .is_some_and(|reason| !reason.is_null()),
                    Protocol::AnthropicMessages => event["type"] == "message_delta",
                    Protocol::OpenAiResponses => event["type"] == "response.completed",
                })
                .unwrap();
            match target {
                Protocol::OpenAiChat => {
                    assert_eq!(terminal["choices"][0]["finish_reason"], "stop");
                    assert_eq!(aggregate["choices"][0]["message"]["content"], "ok");
                    assert_eq!(terminal["usage"], aggregate["usage"]);
                    assert_eq!(terminal["usage"]["total_tokens"], 14);
                }
                Protocol::AnthropicMessages => {
                    assert_eq!(terminal["delta"]["stop_reason"], "end_turn");
                    assert_eq!(aggregate["content"][0]["text"], "ok");
                    assert_eq!(terminal["usage"], aggregate["usage"]);
                    assert_eq!(terminal["usage"]["input_tokens"], 9);
                    assert_eq!(terminal["usage"]["cache_read_input_tokens"], 3);
                    assert_eq!(terminal["usage"]["output_tokens"], 2);
                }
                Protocol::OpenAiResponses => {
                    assert_eq!(terminal["response"], aggregate);
                    assert_eq!(aggregate["output"][0]["content"][0]["text"], "ok");
                    assert_eq!(aggregate["usage"]["total_tokens"], 14);
                }
            }
        }
    }
}

#[test]
fn sse_mixed_line_endings_are_independent_of_network_splits() {
    // PROXY-50: take the earliest delimiter, not whichever newline style is preferred.
    for (first, second) in [
        ("\r\n\r\n", "\n\n"),
        ("\n\n", "\r\n\r\n"),
        ("\n\r\n", "\r\n\n"),
        ("\r\r", "\n\n"),
        ("\r\n\r", "\n\r"),
    ] {
        let input = format!(
            "data: {}{first}data: {}{second}data: [DONE]\r\n\r\n",
            chat_delta(json!({"content":"你好"}), Value::Null),
            chat_delta(json!({}), json!("stop"))
        );
        let bytes = input.as_bytes();
        let baseline = {
            let mut converter =
                StreamConverter::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
            let mut output = converter.push(bytes).unwrap();
            output.extend(converter.finish().unwrap());
            output
        };
        for split in 0..=bytes.len() {
            let mut converter =
                StreamConverter::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
            let mut output = converter.push(&bytes[..split]).unwrap();
            output.extend(converter.push(&bytes[split..]).unwrap());
            output.extend(converter.finish().unwrap());
            assert_eq!(output, baseline, "split at {split}");
        }
        let mut converter = StreamConverter::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
        let mut output = Vec::new();
        for byte in bytes {
            output.extend(converter.push(&[*byte]).unwrap());
        }
        output.extend(converter.finish().unwrap());
        assert_eq!(output, baseline);
    }
}

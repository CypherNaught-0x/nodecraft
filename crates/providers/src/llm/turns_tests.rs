//! Multi-turn conversations, tool calls and usage (#195), for both dialects.

use super::tests::Canned;
use super::*;
use crate::http::HttpResponse;
use futures::executor::block_on;
use serde_json::json;
use std::sync::Mutex;

fn weather() -> Tool {
    Tool {
        name: "get_weather".into(),
        description: "Current weather for a city".into(),
        input_schema: json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"], "additionalProperties": false}),
        strict: true,
    }
}

fn call(id: &str) -> Part {
    Part::ToolUse { id: id.into(), name: "get_weather".into(), input: json!({"city": "Oslo"}) }
}

fn result(id: &str) -> Part {
    Part::ToolResult { id: id.into(), content: "4 °C, rain".into(), is_error: false }
}

/// A conversation where the model called a tool and the user sends back its result.
fn tool_turns() -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        system: None,
        messages: vec![
            Message::user("Weather in Oslo?"),
            Message { role: Role::Assistant, parts: vec![Part::Text("Let me check.".into()), call("call_1")] },
            Message { role: Role::User, parts: vec![Part::Text("Thanks".into()), result("call_1")] },
        ],
        max_tokens: Some(100),
        temperature: None,
        tools: vec![weather()],
    }
}

fn job(inputs: Vec<(&str, &str)>) -> ProviderJob {
    ProviderJob {
        model: "m".into(),
        params: Default::default(),
        inputs: inputs.into_iter().map(|(role, text)| JobInput::Text { role: role.into(), text: text.into() }).collect(),
        idempotency_key: "k".into(),
    }
}

#[test]
fn jobs_carry_the_conversation_in_order() {
    let mut inputs = job(vec![("system", "be kind"), ("prompt", "hi"), ("assistant", "Hello!"), ("assistant", "How can I help?"), ("prompt", "a poem")]).inputs;
    inputs.push(JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(&b"P"[..]) });
    let r = chat_request(&ProviderJob { inputs, ..job(vec![]) }).unwrap();
    assert_eq!(r.system.as_deref(), Some("be kind"));
    let roles: Vec<Role> = r.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, [Role::User, Role::Assistant, Role::User]);
    assert_eq!(r.messages[1].parts.len(), 2, "consecutive replies are one message");
    assert_eq!(r.messages[2].parts.len(), 2, "the image joins the user's last turn");
    // A job ending on an assistant turn is refused by Claude, accepted (as a prefill) elsewhere.
    let mut ends_assistant = chat_request(&job(vec![("prompt", "hi"), ("assistant", "Hel")])).unwrap();
    assert!(ends_assistant.clone().validate(Dialect::OpenAi).is_ok());
    assert!(ends_assistant.validate(Dialect::Anthropic).is_err());
    assert!(chat_request(&job(vec![("system", "only a system prompt")])).unwrap().validate(Dialect::OpenAi).is_err());
}

#[test]
fn tool_conversations_are_validated() {
    for d in [Dialect::OpenAi, Dialect::Anthropic] {
        assert!(tool_turns().validate(d).is_ok());
    }
    let check = |edit: &dyn Fn(&mut ChatRequest)| {
        let mut r = tool_turns();
        edit(&mut r);
        r.validate(Dialect::OpenAi).map_err(|e| (e.kind, e.message))
    };
    let refused = |edit: &dyn Fn(&mut ChatRequest), says: &str| {
        let (kind, message) = check(edit).unwrap_err();
        assert_eq!(kind, ErrorKind::UnsupportedInput);
        assert!(message.contains(says), "{message}");
    };
    refused(&|r| r.messages[2].parts = vec![Part::Text("no result".into())], "needs its result");
    refused(&|r| r.messages[2].parts.push(result("call_9")), "doesn't answer");
    refused(&|r| r.messages.truncate(2), "needs its result");
    refused(&|r| r.messages[0].parts.push(call("call_0")), "only assistant messages");
    refused(&|r| r.messages[1].parts.push(result("call_1")), "only user messages");
    refused(&|r| r.messages[1].parts[1] = call(""), "empty or too long");
    refused(&|r| r.tools.push(weather()), "two tools");
    refused(&|r| r.tools[0].name = "get weather".into(), "valid tool name");
    refused(&|r| r.tools[0].name = "x".repeat(65), "valid tool name");
    refused(&|r| r.tools[0].input_schema = json!({"type": "string"}), "object schema");
    refused(&|r| r.tools = (0..=MAX_TOOLS).map(|i| Tool { name: format!("t{i}"), ..weather() }).collect(), "more than");
    // A result answers the previous message only, not an older one.
    refused(
        &|r| {
            r.messages.push(Message::assistant("It's raining."));
            r.messages.push(Message { role: Role::User, parts: vec![result("call_1")] });
        },
        "doesn't answer",
    );
}

#[test]
fn opaque_blocks_go_only_to_their_dialect() {
    let thinking = Part::Opaque { dialect: Dialect::Anthropic, block: json!({"type": "thinking", "thinking": "hmm", "signature": "sig"}) };
    let mut r = tool_turns();
    r.messages[1].parts.insert(0, thinking.clone());
    let mut for_openai = r.clone();
    for_openai.validate(Dialect::OpenAi).unwrap();
    assert!(!for_openai.messages[1].parts.contains(&thinking), "dropped for the other dialect");
    r.validate(Dialect::Anthropic).unwrap();
    assert_eq!(r.messages[1].parts[0], thinking, "kept, first, for Claude");
    // An assistant turn that was only thinking can't go to another dialect.
    let only = ChatRequest {
        messages: vec![Message::user("a"), Message { role: Role::Assistant, parts: vec![thinking] }, Message::user("b")],
        tools: vec![],
        ..tool_turns()
    };
    assert!(only.clone().validate(Dialect::OpenAi).is_err());
    assert!(only.clone().validate(Dialect::Anthropic).is_ok());
}

#[test]
fn openai_tool_requests() {
    let b = openai::body(&tool_turns(), false, true);
    assert_eq!(
        b["tools"][0],
        json!({"type": "function", "function": {"name": "get_weather", "description": "Current weather for a city", "parameters": weather().input_schema, "strict": true}})
    );
    let m = &b["messages"];
    assert_eq!(m[1]["content"], "Let me check.");
    assert_eq!(m[1]["tool_calls"][0], json!({"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Oslo\"}"}}));
    assert_eq!(m[2], json!({"role": "tool", "tool_call_id": "call_1", "content": "4 °C, rain"}), "tool results straight after the calls");
    assert_eq!(m[3], json!({"role": "user", "content": "Thanks"}));
    assert!(b.get("tool_choice").is_none());

    let mut r = tool_turns();
    r.messages[1].parts = vec![Part::ToolUse { id: "call_1".into(), name: "get_weather".into(), input: Value::String("{city: Oslo".into()) }];
    r.messages[2].parts = vec![Part::ToolResult { id: "call_1".into(), content: "unknown city".into(), is_error: true }];
    r.tools[0].strict = false;
    let b = openai::body(&r, false, false);
    assert_eq!(b["messages"][1]["content"], Value::Null, "a turn of only calls has null content");
    assert_eq!(b["messages"][1]["tool_calls"][0]["function"]["arguments"], "{city: Oslo", "unparsed arguments go back as they came");
    assert_eq!(b["messages"][2]["content"], "Error: unknown city");
    assert_eq!(b["messages"].as_array().unwrap().len(), 3, "no empty user message after the results");
    assert!(b["tools"][0]["function"].get("strict").is_none(), "strict only when asked");
}

#[test]
fn openai_tool_replies() {
    let r = openai::parse(
        br#"{"choices":[{"message":{"content":null,"tool_calls":[
            {"id":"call_a","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Oslo\"}"}},
            {"type":"function","function":{"name":"get_time","arguments":{"tz":"CET"}}},
            {"id":"call_c","type":"function","function":{"name":"broken","arguments":"{nope"}}
        ]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":20,"completion_tokens":9}}"#,
    )
    .unwrap();
    assert_eq!(r.stop, StopReason::ToolUse);
    assert_eq!(r.text, "");
    let calls: Vec<_> = r.tool_calls().collect();
    assert_eq!(calls.len(), 3);
    assert_eq!((calls[0].id, calls[0].name, calls[0].input), ("call_a", "get_weather", &json!({"city": "Oslo"})));
    assert_eq!((calls[1].id, calls[1].input), ("call_1", &json!({"tz": "CET"})), "missing ids are filled in; object arguments accepted");
    assert_eq!(calls[2].input, &json!("{nope"), "arguments that aren't JSON stay text");
    assert_eq!((r.usage.input_tokens, r.usage.output_tokens), (Some(20), Some(9)));
    assert_eq!(r.message.role, Role::Assistant);
    assert_eq!(openai::parse(br#"{"choices":[{"message":{"tool_calls":[{"id":"x","function":{}}]}}]}"#).unwrap_err().kind, ErrorKind::Malformed);
}

#[test]
fn openai_streamed_tool_calls() {
    let mut s = StreamState::default();
    let ev = |d: &str| sse::SseEvent { event: None, data: d.into() };
    for d in [
        r#"{"choices":[{"delta":{"role":"assistant","content":"Checking"}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"get_time","arguments":"{}"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"ci"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"get_weather","arguments":"ty\":\"Oslo\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ] {
        openai::stream_event(&ev(d), &mut s).unwrap();
    }
    let parts = openai::stream_parts(&mut s).unwrap();
    assert_eq!(
        parts,
        vec![
            Part::Text("Checking".into()),
            Part::ToolUse { id: "call_a".into(), name: "get_weather".into(), input: json!({"city": "Oslo"}) },
            Part::ToolUse { id: "call_b".into(), name: "get_time".into(), input: json!({}) },
        ]
    );
    assert_eq!(s.stop, Some(StopReason::ToolUse));
    // A server can't make endless calls.
    let mut s = StreamState::default();
    let flood: Vec<Value> = (0..=MAX_REPLY_BLOCKS as u64).map(|i| json!({"index": i, "id": "x", "function": {"name": "f"}})).collect();
    let flood = json!({"choices": [{"delta": {"tool_calls": flood}}]}).to_string();
    assert_eq!(openai::stream_event(&ev(&flood), &mut s).unwrap_err().kind, ErrorKind::Malformed);
}

#[test]
fn anthropic_tool_requests() {
    let mut r = tool_turns();
    r.messages[1].parts.insert(0, Part::Opaque { dialect: Dialect::Anthropic, block: json!({"type": "thinking", "thinking": "plan", "signature": "sig"}) });
    r.messages[2].parts.push(Part::ToolResult { id: "call_1".into(), content: "again".into(), is_error: true });
    r.messages[1].parts.push(Part::ToolUse { id: "call_1".into(), name: "get_weather".into(), input: json!("not an object") });
    let b = anthropic::body(&r, false);
    assert_eq!(
        b["tools"][0],
        json!({"name": "get_weather", "description": "Current weather for a city", "input_schema": weather().input_schema, "strict": true})
    );
    assert!(b.get("tool_choice").is_none(), "auto: current Claude models reject forced calls");
    let assistant = &b["messages"][1]["content"];
    assert_eq!(assistant[0], json!({"type": "thinking", "thinking": "plan", "signature": "sig"}), "thinking goes back unchanged");
    assert_eq!(assistant[2], json!({"type": "tool_use", "id": "call_1", "name": "get_weather", "input": {"city": "Oslo"}}));
    assert_eq!(assistant[3]["input"], json!({}), "non-object input can't be sent to Claude");
    let user = &b["messages"][2]["content"];
    assert_eq!(user[0], json!({"type": "tool_result", "tool_use_id": "call_1", "content": "4 °C, rain"}), "results first");
    assert_eq!(user[1]["is_error"], true);
    assert_eq!(user[2], json!({"type": "text", "text": "Thanks"}));
}

#[test]
fn anthropic_tool_replies_keep_thinking() {
    let r = anthropic::parse(
        br#"{"content":[
            {"type":"thinking","thinking":"I should call the tool","signature":"EqQB"},
            {"type":"text","text":"Checking."},
            {"type":"tool_use","id":"toolu_1","name":"get_weather","input":{"city":"Oslo"}},
            {"type":"redacted_thinking","data":"abc"}
        ],"stop_reason":"tool_use","usage":{"input_tokens":30,"output_tokens":12}}"#,
    )
    .unwrap();
    assert_eq!(r.stop, StopReason::ToolUse);
    assert_eq!(r.text, "Checking.");
    assert_eq!(r.message.parts.len(), 4);
    assert_eq!(
        r.message.parts[0],
        Part::Opaque { dialect: Dialect::Anthropic, block: json!({"type":"thinking","thinking":"I should call the tool","signature":"EqQB"}) }
    );
    assert!(matches!(&r.message.parts[3], Part::Opaque { block, .. } if block["type"] == "redacted_thinking"));
    let calls: Vec<_> = r.tool_calls().collect();
    assert_eq!((calls[0].id, calls[0].input), ("toolu_1", &json!({"city": "Oslo"})));
}

#[test]
fn anthropic_streamed_tool_calls_and_thinking() {
    let mut s = StreamState::default();
    let ev = |d: &str| sse::SseEvent { event: None, data: d.into() };
    let mut text = String::new();
    for d in [
        r#"{"type":"message_start","message":{"usage":{"input_tokens":30}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Use the "}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"tool."}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQB"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Checking."}}"#,
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":""}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"city\": \"Os"}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"lo\"}"}}"#,
        r#"{"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"toolu_2","name":"get_time","input":{}}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":40}}"#,
        r#"{"type":"message_stop"}"#,
    ] {
        if let Some(t) = anthropic::stream_event(&ev(d), &mut s).unwrap() {
            text.push_str(&t);
        }
    }
    assert_eq!(text, "Checking.");
    let parts = anthropic::stream_parts(&mut s).unwrap();
    assert_eq!(
        parts,
        vec![
            Part::Opaque { dialect: Dialect::Anthropic, block: json!({"type": "thinking", "thinking": "Use the tool.", "signature": "EqQB"}) },
            Part::Text("Checking.".into()),
            Part::ToolUse { id: "toolu_1".into(), name: "get_weather".into(), input: json!({"city": "Oslo"}) },
            Part::ToolUse { id: "toolu_2".into(), name: "get_time".into(), input: json!({}) },
        ]
    );
    assert_eq!((s.usage.input_tokens, s.usage.output_tokens, s.stop.clone()), (Some(30), Some(40), Some(StopReason::ToolUse)));
}

/// The agent loop: ask, run the tool, send the reply back as it came plus the result.
#[test]
fn a_tool_loop_round_trips() {
    let second =
        r#"{"content":[{"type":"text","text":"It's 4 °C and raining in Oslo."}],"stop_reason":"end_turn","usage":{"input_tokens":60,"output_tokens":10}}"#;
    let first = r#"{"content":[{"type":"thinking","thinking":"call it","signature":"sig1"},{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{"city":"Oslo"}}],"stop_reason":"tool_use"}"#;
    let http = Arc::new(Canned {
        replies: Mutex::new(vec![
            HttpResponse { status: 200, headers: vec![], body: second.as_bytes().to_vec() },
            HttpResponse { status: 200, headers: vec![], body: first.as_bytes().to_vec() },
        ]),
        seen: Mutex::new(vec![]),
    });
    let client = LlmClient::new(Endpoint::anthropic(ApiKey::new("k")), http.clone());
    let mut req = ChatRequest { messages: vec![Message::user("Weather in Oslo?")], ..tool_turns() };
    let reply = block_on(client.chat(&req)).unwrap();
    assert_eq!(reply.stop, StopReason::ToolUse);
    let results: Vec<Part> = reply.tool_calls().map(|c| Part::ToolResult { id: c.id.into(), content: "4 °C, rain".into(), is_error: false }).collect();
    req.messages.push(reply.message.clone());
    req.messages.push(Message { role: Role::User, parts: results });
    let done = block_on(client.chat(&req)).unwrap();
    assert_eq!(done.text, "It's 4 °C and raining in Oslo.");
    let sent: Value = serde_json::from_slice(&http.seen.lock().unwrap()[1].body).unwrap();
    assert_eq!(sent["messages"][1]["content"][0], json!({"type":"thinking","thinking":"call it","signature":"sig1"}), "the reply went back unchanged");
    assert_eq!(sent["messages"][2]["content"][0]["tool_use_id"], "toolu_1");
}

#[test]
fn reported_costs_become_money() {
    assert_eq!(usd(0.0012), Some(Money::new(Currency::USD, 1200)));
    assert_eq!(usd(0.000_000_4), Some(Money::new(Currency::USD, 1)), "a fraction of a micro-dollar is never free");
    assert_eq!(usd(0.0), Some(Money::new(Currency::USD, 0)));
    for bad in [f64::NAN, f64::INFINITY, -0.5, 1e13] {
        assert_eq!(usd(bad), None, "{bad}");
    }
    let header = |v: &str| reported_cost(&[("x-litellm-response-cost".into(), v.into())]);
    assert_eq!(header(" 0.5 "), Some(Money::new(Currency::USD, 500_000)));
    assert_eq!(header("NaN"), None);
    assert_eq!(header("lots"), None);
}

#[test]
fn litellm_key_spend() {
    let http = Canned::new(200, r#"{"key":"sk-…","info":{"spend":1.25,"max_budget":null,"models":[]}}"#);
    let client = LlmClient::new(Endpoint::litellm("http://proxy:4000/", Some(ApiKey::new("vk"))), http.clone());
    let spend = block_on(client.litellm_key_info()).unwrap();
    assert_eq!(spend, KeySpend { spent: Some(Money::new(Currency::USD, 1_250_000)), max_budget: None });
    let req = http.request();
    assert_eq!(req.url, "http://proxy:4000/key/info");
    assert!(req.headers.contains(&("Authorization".into(), "Bearer vk".into())));
    let claude = LlmClient::new(Endpoint::anthropic(ApiKey::new("k")), Canned::new(200, "{}"));
    assert_eq!(block_on(claude.litellm_key_info()).unwrap_err().kind, ErrorKind::UnsupportedInput);
    let odd = LlmClient::new(Endpoint::litellm("http://proxy:4000", None), Canned::new(200, r#"{"info":{"spend":-3}}"#));
    assert_eq!(block_on(odd.litellm_key_info()).unwrap().spent, None, "nonsense amounts are dropped");
}

#[test]
fn review_regressions() {
    let ev = |d: &str| sse::SseEvent { event: None, data: d.into() };
    // Calls without an index, one per chunk, stay separate.
    let mut s = StreamState::default();
    for d in [
        r#"{"choices":[{"delta":{"tool_calls":[{"id":"a","function":{"name":"f","arguments":"{\"x\":"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"1}"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"id":"b","function":{"name":"g","arguments":"{}"}}]}}]}"#,
    ] {
        openai::stream_event(&ev(d), &mut s).unwrap();
    }
    assert_eq!(
        openai::stream_parts(&mut s).unwrap(),
        vec![Part::ToolUse { id: "a".into(), name: "f".into(), input: json!({"x": 1}) }, Part::ToolUse { id: "b".into(), name: "g".into(), input: json!({}) },]
    );
    // Unnamed calls are malformed in every path.
    let mut s = StreamState::default();
    openai::stream_event(&ev(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"arguments":"{}"}}]}}]}"#), &mut s).unwrap();
    assert_eq!(openai::stream_parts(&mut s).unwrap_err().kind, ErrorKind::Malformed);
    let mut s = StreamState::default();
    anthropic::stream_event(&ev(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t"}}"#), &mut s).unwrap();
    assert_eq!(anthropic::stream_parts(&mut s).unwrap_err().kind, ErrorKind::Malformed);
    assert_eq!(
        anthropic::parse(br#"{"content":[{"type":"tool_use","name":"f","input":{}}],"stop_reason":"tool_use"}"#).unwrap_err().kind,
        ErrorKind::Malformed
    );
    // Server tool blocks keep their streamed input.
    let mut s = StreamState::default();
    anthropic::stream_event(
        &ev(r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srv","name":"web_search","input":{}}}"#),
        &mut s,
    )
    .unwrap();
    anthropic::stream_event(
        &ev(r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"oslo\"}"}}"#),
        &mut s,
    )
    .unwrap();
    assert!(matches!(&anthropic::stream_parts(&mut s).unwrap()[0], Part::Opaque { block, .. } if block["input"] == json!({"query": "oslo"})));
    // Duplicate ids are refused before sending.
    let mut r = tool_turns();
    r.messages[1].parts.push(call("call_1"));
    assert!(r.validate(Dialect::OpenAi).unwrap_err().message.contains("twice"));
    let mut r = tool_turns();
    r.messages[2].parts.push(result("call_1"));
    assert!(r.validate(Dialect::OpenAi).unwrap_err().message.contains("twice"));
    // A thinking-only turn sent elsewhere says why it can't go.
    let only = Message { role: Role::Assistant, parts: vec![Part::Opaque { dialect: Dialect::Anthropic, block: json!({"type": "thinking"}) }] };
    let mut r = ChatRequest { messages: vec![Message::user("a"), only, Message::user("b")], tools: vec![], ..tool_turns() };
    assert!(r.validate(Dialect::OpenAi).unwrap_err().message.contains("reasoning"));
    // Float noise doesn't add a micro-dollar.
    for micros in [123u64, 246, 1, 999_999, 1_234_567] {
        assert_eq!(usd(micros as f64 / 1e6), Some(Money::new(Currency::USD, micros)), "{micros}");
    }
    assert_eq!(usd(1.000_000_4), Some(Money::new(Currency::USD, 1_000_001)), "large amounts still round up");
    assert_eq!(usd(12.345_678_4), Some(Money::new(Currency::USD, 12_345_679)));
}

/// Long streamed replies stay linear: each delta appends in place.
#[test]
fn long_anthropic_streams_are_linear() {
    let mut s = StreamState::default();
    let ev = sse::SseEvent { event: None, data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"abcdefgh"}}"#.into() };
    let started = std::time::Instant::now();
    for _ in 0..200_000 {
        anthropic::stream_event(&ev, &mut s).unwrap();
    }
    assert_eq!(s.blocks[&0].value["thinking"].as_str().map(str::len), Some(1_600_000));
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
}

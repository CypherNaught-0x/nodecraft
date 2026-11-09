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
    // Unnamed calls and server tool blocks: `openai::tests` and `anthropic::tests`.
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

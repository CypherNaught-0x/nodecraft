//! DeepSeek, Mistral, Groq and xAI (#1122): each in rig's dialect for it, against canned replies.

use super::tests::Canned;
use super::*;
use crate::http::{ChunkSink, HttpResponse};
use futures::executor::block_on;
use serde_json::json;
use std::sync::Mutex;

/// A provider: its endpoint, the URL chat requests go to, and a model of its catalog.
type Vendor = (fn(ApiKey) -> Endpoint, &'static str, &'static str);

fn vendors() -> [Vendor; 4] {
    [
        (Endpoint::deepseek, "https://api.deepseek.com/chat/completions", "deepseek-flash"),
        (Endpoint::mistral, "https://api.mistral.ai/v1/chat/completions", "mistral-small-2603"),
        (Endpoint::groq, "https://api.groq.com/openai/v1/chat/completions", "openai/gpt-oss-20b"),
        (Endpoint::xai, "https://api.x.ai/v1/chat/completions", "grok-4.3"),
    ]
}

const REPLY: &str =
    r#"{"choices":[{"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}],"usage":{"prompt_tokens":9,"completion_tokens":1}}"#;

fn ask(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        system: Some("be brief".into()),
        messages: vec![Message::user("ping")],
        max_tokens: Some(64),
        temperature: None,
        tools: vec![],
    }
}

fn weather() -> Tool {
    Tool {
        name: "get_weather".into(),
        description: "Current weather for a city".into(),
        input_schema: json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}),
        strict: false,
    }
}

#[test]
fn requests_go_to_each_provider_with_its_key() {
    for (endpoint, url, model) in vendors() {
        let http = Canned::new(200, REPLY);
        let client = LlmClient::new(endpoint(ApiKey::new("sk-vendor")), http.clone());
        let reply = block_on(client.chat(&ask(model))).unwrap();
        assert_eq!((reply.text.as_str(), reply.usage.input_tokens, reply.usage.output_tokens), ("pong", Some(9), Some(1)), "{url}");
        let req = http.request();
        assert_eq!(req.url, url);
        assert!(req.headers.contains(&("Authorization".into(), "Bearer sk-vendor".into())), "{url}");
        assert!(req.secret_headers.contains(&"authorization".to_owned()));
        let names: Vec<String> = req.headers.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
        assert!(!names.iter().any(|n| n == "user-agent" || n == "originator" || n.contains("session")), "{url}: {names:?}");
        let body = http.body();
        assert_eq!(body["model"], model);
        assert_eq!(body["messages"][0], json!({"role": "system", "content": "be brief"}), "{url}");
        assert_eq!(body["messages"][1], json!({"role": "user", "content": "ping"}), "{url}");
        assert_eq!(body["max_tokens"], 64, "{url}");
        assert!(body.get("max_completion_tokens").is_none());
        assert!(!client.retry_ambiguous_submit(), "{url}: a chat request that may have been sent is never resent");
        assert!(!format!("{client:?}").contains("sk-vendor"));
    }
}

#[test]
fn tool_calls_go_and_come_back() {
    for (endpoint, url, model) in vendors() {
        let reply = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Oslo\"}"}}]},"finish_reason":"tool_calls"}]}"#;
        let http = Canned::new(200, reply);
        let client = LlmClient::new(endpoint(ApiKey::new("k")), http.clone());
        let r = block_on(client.chat(&ChatRequest { tools: vec![weather()], ..ask(model) })).unwrap();
        assert_eq!(r.stop, StopReason::ToolUse, "{url}");
        let calls: Vec<_> = r.tool_calls().collect();
        assert_eq!((calls[0].id, calls[0].name, calls[0].input), ("call_1", "get_weather", &json!({"city": "Oslo"})), "{url}");
        let body = http.body();
        assert_eq!(
            body["tools"][0],
            json!({"type": "function", "function": {"name": "get_weather", "description": "Current weather for a city", "parameters": weather().input_schema}}),
            "{url}: tools go as written"
        );
        assert!(body.get("tool_choice").is_none(), "{url}");
        // The next turn sends the call and its result back.
        let mut next = ChatRequest { tools: vec![weather()], ..ask(model) };
        next.messages.push(r.message.clone());
        next.messages.push(Message { role: Role::User, parts: vec![Part::ToolResult { id: "call_1".into(), content: "4 °C".into(), is_error: false }] });
        let http = Canned::new(200, REPLY);
        block_on(LlmClient::new(endpoint(ApiKey::new("k")), http.clone()).chat(&next)).unwrap();
        let body = http.body();
        assert_eq!(body["messages"][2]["tool_calls"][0]["function"], json!({"name": "get_weather", "arguments": "{\"city\":\"Oslo\"}"}), "{url}");
        assert_eq!(body["messages"][3], json!({"role": "tool", "tool_call_id": "call_1", "content": "4 °C"}), "{url}");
    }
}

/// Streams `body` back in small pieces.
struct Sse {
    body: Vec<u8>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Http for Sse {
    fn send(&self, _request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::Server, "only streamed requests are expected")) })
    }
    fn send_streaming<'a>(&'a self, request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        self.seen.lock().unwrap().push(request);
        Box::pin(async move {
            for piece in self.body.chunks(13) {
                on_chunk(piece)?;
            }
            Ok(HttpResponse { status: 200, headers: vec![], body: Vec::new() })
        })
    }
}

#[test]
fn streamed_replies_with_tool_calls() {
    let events = [
        r#"{"choices":[{"delta":{"role":"assistant","content":"Checking"}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"get_weather","arguments":"{\"ci"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ty\":\"Oslo\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":20,"completion_tokens":9}}"#,
        "[DONE]",
    ];
    let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    for (endpoint, url, model) in vendors() {
        let http = Arc::new(Sse { body: body.clone().into_bytes(), seen: Mutex::default() });
        let client = LlmClient::new(endpoint(ApiKey::new("k")), http.clone());
        let mut pieces = Vec::new();
        let r = block_on(client.chat_stream(&ChatRequest { tools: vec![weather()], ..ask(model) }, &mut |t| pieces.push(t.to_owned()))).unwrap();
        assert_eq!(pieces.concat(), "Checking", "{url}");
        assert_eq!(
            r.message.parts,
            vec![Part::Text("Checking".into()), Part::ToolUse { id: "call_a".into(), name: "get_weather".into(), input: json!({"city": "Oslo"}) }],
            "{url}"
        );
        assert_eq!((r.stop, r.usage.input_tokens, r.usage.output_tokens), (StopReason::ToolUse, Some(20), Some(9)), "{url}");
        let sent: Value = serde_json::from_slice(&http.seen.lock().unwrap()[0].body).unwrap();
        assert_eq!(sent["stream"], true, "{url}");
        // The stream guard holds for every dialect.
        let huge = r#"{"choices":[{"delta":{"tool_calls":[{"index":100000000000,"id":"a","function":{"name":"f","arguments":"{}"}}]}}]}"#;
        let http = Arc::new(Sse { body: format!("data: {huge}\n\n").into_bytes(), seen: Mutex::default() });
        let e = block_on(LlmClient::new(endpoint(ApiKey::new("k")), http).chat_stream(&ask(model), &mut |_| {})).unwrap_err();
        assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Malformed, "the reply has too many tool calls"), "{url}");
    }
}

#[test]
fn error_replies_keep_their_kinds() {
    for (endpoint, url, model) in vendors() {
        let limited = Canned::with_headers(429, r#"{"error":{"message":"slow down","code":"rate_limit_exceeded"}}"#, vec![("retry-after".into(), "4".into())]);
        let e = block_on(LlmClient::new(endpoint(ApiKey::new("k")), limited).chat(&ask(model))).unwrap_err();
        assert_eq!((e.kind, e.retry_after_ms), (ErrorKind::RateLimited, Some(4000)), "{url}");
        let e = block_on(LlmClient::new(endpoint(ApiKey::new("k")), Canned::new(401, r#"{"error":{"message":"bad key"}}"#)).chat(&ask(model))).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Auth, "{url}");
        // Mistral's own error body has no `error` object: the status decides.
        let e = block_on(
            LlmClient::new(endpoint(ApiKey::new("k")), Canned::new(402, r#"{"object":"error","message":"no credit","type":"payment"}"#)).chat(&ask(model)),
        )
        .unwrap_err();
        assert_eq!(e.kind, ProviderError::from_status(402, b"").kind, "{url}");
        let e = block_on(LlmClient::new(endpoint(ApiKey::new("k")), Canned::new(503, "busy")).chat(&ask(model))).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Busy, "{url}");
    }
}

/// DeepSeek's thinking models return their reasoning beside the answer and want it back, with the
/// calls, in later turns of a request that carries tools.
#[test]
fn deepseek_reasoning_goes_back() {
    let reply = r#"{"choices":[{"message":{"content":null,"reasoning_content":"Look it up.","tool_calls":[{"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Oslo\"}"}}]},"finish_reason":"tool_calls"}]}"#;
    let client = LlmClient::new(Endpoint::deepseek(ApiKey::new("k")), Canned::new(200, reply));
    let r = block_on(client.chat(&ChatRequest { tools: vec![weather()], ..ask("deepseek-flash") })).unwrap();
    assert_eq!(r.message.parts[0], Part::Opaque { dialect: Dialect::OpenAi, block: json!({"reasoning_content": "Look it up."}) });
    assert_eq!(r.text, "", "reasoning isn't the answer");
    let mut next = ChatRequest { tools: vec![weather()], ..ask("deepseek-flash") };
    next.messages.push(r.message.clone());
    next.messages.push(Message { role: Role::User, parts: vec![Part::ToolResult { id: "call_1".into(), content: "4 °C".into(), is_error: false }] });
    let http = Canned::new(200, REPLY);
    block_on(LlmClient::new(Endpoint::deepseek(ApiKey::new("k")), http.clone()).chat(&next)).unwrap();
    let body = http.body();
    assert_eq!(body["messages"][2]["reasoning_content"], "Look it up.", "{body}");
    assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "call_1");
    // Other endpoints keep no reasoning, as before.
    let r =
        block_on(LlmClient::new(Endpoint::groq(ApiKey::new("k")), Canned::new(200, reply)).chat(&ChatRequest { tools: vec![weather()], ..ask("m") })).unwrap();
    assert!(!r.message.parts.iter().any(|p| matches!(p, Part::Opaque { .. })));
}

/// A streamed DeepSeek reply's reasoning is kept too, and isn't streamed as answer text.
#[test]
fn deepseek_streamed_reasoning_is_kept_not_shown() {
    let events = [
        r#"{"choices":[{"delta":{"role":"assistant","reasoning_content":"Think"}}]}"#,
        r#"{"choices":[{"delta":{"reasoning_content":"ing."}}]}"#,
        r#"{"choices":[{"delta":{"content":"pong"},"finish_reason":"stop"}]}"#,
        "[DONE]",
    ];
    let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    let http = Arc::new(Sse { body: body.into_bytes(), seen: Mutex::default() });
    let mut pieces = Vec::new();
    let r =
        block_on(LlmClient::new(Endpoint::deepseek(ApiKey::new("k")), http).chat_stream(&ask("deepseek-flash"), &mut |t| pieces.push(t.to_owned()))).unwrap();
    assert_eq!(pieces.concat(), "pong");
    assert_eq!(r.message.parts, vec![Part::Opaque { dialect: Dialect::OpenAi, block: json!({"reasoning_content": "Thinking."}) }, Part::Text("pong".into())]);
}

/// Mistral's chunks: an image goes as its own chunk, the system prompt and text as strings.
#[test]
fn mistral_content_shapes() {
    let http = Canned::new(200, REPLY);
    let mut r = ask("mistral-large-4");
    r.messages[0].parts.push(Part::Image { mime: "image/png".into(), bytes: Arc::from(&b"PNG"[..]) });
    block_on(LlmClient::new(Endpoint::mistral(ApiKey::new("k")), http.clone()).chat(&r)).unwrap();
    let body = http.body();
    assert_eq!(body["messages"][0], json!({"role": "system", "content": "be brief"}));
    assert_eq!(body["messages"][1]["content"][0], json!({"type": "text", "text": "ping"}));
    assert_eq!(body["messages"][1]["content"][1], json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,UE5H"}}));
}

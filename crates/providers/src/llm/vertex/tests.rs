use super::*;
use crate::Provider;
use crate::google_cloud::tests::{NOW, fixed_clock, granted, stored};
use crate::http::{HttpRequest, HttpResponse};
use crate::provider::BoxFuture;
use futures::executor::block_on;
use std::collections::VecDeque;
use std::sync::Mutex;

const TOKEN: &str = "public-vertex-fixture-token";
#[derive(Default)]
struct Transport {
    seen: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<HttpResponse>>,
}
impl Http for Transport {
    fn send_no_redirect(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.send(req)
    }
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.seen.lock().unwrap().push(req);
        let reply = self.replies.lock().unwrap().pop_front().ok_or_else(|| ProviderError::new(ErrorKind::Network, "no fixture reply"));
        Box::pin(async { reply })
    }
}
fn transport(replies: Vec<HttpResponse>) -> Arc<Transport> {
    let mut all = VecDeque::from([granted(TOKEN).unwrap()]);
    all.extend(replies);
    Arc::new(Transport { seen: Mutex::new(vec![]), replies: Mutex::new(all) })
}
fn answer(parts: Value) -> Value {
    json!({"candidates":[{"content":{"role":"model","parts":parts},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":2,"thoughtsTokenCount":3}})
}
fn ok(value: Value) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body: value.to_string().into_bytes() }
}
fn request() -> ChatRequest {
    ChatRequest {
        model: "gemini-2.5-flash".into(),
        system: Some("Be brief".into()),
        messages: vec![super::super::Message::user("Hello")],
        max_tokens: Some(64),
        temperature: None,
        tools: vec![],
    }
}
fn client(http: Arc<Transport>) -> LlmClient {
    LlmClient::vertex("global", &stored(), http, fixed_clock(NOW)).unwrap()
}
#[test]
fn whole_vertex_reply_uses_native_wire_and_keeps_credentials_out_of_urls_and_debug() {
    let http = transport(vec![ok(answer(json!([{"text":"Hello back"}])))]);
    let client = client(http.clone());
    let reply = block_on(client.chat(&request())).unwrap();
    assert_eq!(reply.text, "Hello back");
    assert_eq!((reply.usage.input_tokens, reply.usage.output_tokens), (Some(4), Some(5)));
    assert!(!format!("{client:?}").contains(TOKEN));
    assert!(!client.retry_ambiguous_submit());
    let seen = http.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].url, crate::google_cloud::TOKEN_URI);
    let req = &seen[1];
    assert_eq!(
        req.url,
        "https://aiplatform.googleapis.com/v1/projects/nodecraft-test/locations/global/publishers/google/models/gemini-2.5-flash:generateContent"
    );
    assert!(req.headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Bearer {TOKEN}")));
    assert!(!req.url.contains(TOKEN));
    assert!(!format!("{req:?}").contains(TOKEN));
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(body["contents"][0]["parts"][0]["text"], "Hello");
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Be brief");
    assert_eq!(body["generationConfig"]["maxOutputTokens"], 64);
}
#[test]
fn streamed_vertex_reply_delivers_text_and_final_usage() {
    let mut last = answer(json!([{"text":" world"}]));
    last["candidates"][0]["finishReason"] = json!("STOP");
    let body = format!("data: {}\n\ndata: {last}\n\n", json!({"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}));
    let http = transport(vec![HttpResponse { status: 200, headers: vec![("content-type".into(), "text/event-stream".into())], body: body.into_bytes() }]);
    let mut pieces = String::new();
    let reply = block_on(client(http.clone()).chat_stream(&request(), &mut |t| pieces.push_str(t))).unwrap();
    assert_eq!(reply.text, "Hello world");
    assert_eq!(pieces, "Hello world");
    assert_eq!(reply.usage.output_tokens, Some(5));
    assert!(http.seen.lock().unwrap()[1].url.ends_with(":streamGenerateContent?alt=sse"));
}
#[test]
fn function_turn_replays_signature_once_and_answers_by_name() {
    let native = json!([{"functionCall":{"id":"vertex_call_1","name":"weather","args":{"city":"Berlin"}},"thoughtSignature":"c2lnbg=="}]);
    let http = transport(vec![ok(answer(native.clone())), ok(answer(json!([{"text":"Sunny"}])))]);
    let client = client(http.clone());
    let mut req = request();
    req.model = "gemini-3.8-flash".into();
    req.tools = vec![super::super::Tool {
        name: "weather".into(),
        description: "Weather".into(),
        input_schema: json!({"type":"object","properties":{"city":{"type":"string"}}}),
        strict: false,
    }];
    let first = block_on(client.chat(&req)).unwrap();
    assert_eq!(first.stop, StopReason::ToolUse);
    let id = first.tool_calls().next().unwrap().id.to_string();
    req.messages.push(first.message);
    req.messages.push(super::super::Message { role: Role::User, parts: vec![Part::ToolResult { id, content: "Sunny".into(), is_error: false }] });
    assert_eq!(block_on(client.chat(&req)).unwrap().text, "Sunny");
    let seen = http.seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "OAuth token is reused");
    let body: Value = serde_json::from_slice(&seen[2].body).unwrap();
    assert_eq!(body["contents"][1]["parts"], native, "{body}");
    assert_eq!(body["contents"][2]["parts"][0]["functionResponse"]["name"], "weather", "{body}");
    assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "weather");
}
#[test]
fn invalid_region_model_and_gif_are_refused_before_oauth() {
    for region in ["", "us-central1.evil.test", "x/y", "https://example.com", "GLOBAL", "-west1", "us-central1-"] {
        assert!(LlmClient::vertex(region, &stored(), Arc::new(Transport::default()), fixed_clock(NOW)).is_err());
    }
    let http = Arc::new(Transport::default());
    let client = client(http.clone());
    for model in ["gemini-x?key=secret", "gemini-x/else", "gpt-test"] {
        let mut req = request();
        req.model = model.into();
        assert!(block_on(client.chat(&req)).is_err());
    }
    let mut req = request();
    req.messages[0].parts.push(Part::Image { mime: "image/gif".into(), bytes: Arc::from(&b"GIF"[..]) });
    assert!(block_on(client.chat(&req)).is_err());
    assert!(http.seen.lock().unwrap().is_empty());
}
#[test]
fn auth_errors_are_redacted_and_refresh_without_replaying_the_paid_request() {
    let http = transport(vec![
        HttpResponse { status: 403, headers: vec![], body: format!("{{\"error\":{{\"message\":\"{TOKEN}\"}}}}").into_bytes() },
        granted("fresh-public-token").unwrap(),
        ok(answer(json!([{"text":"ok"}]))),
    ]);
    let client = client(http.clone());
    let err = block_on(client.chat(&request())).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Auth);
    assert!(!err.message.contains(TOKEN));
    assert_eq!(http.seen.lock().unwrap().len(), 2, "no automatic retry");
    block_on(client.chat(&request())).unwrap();
    assert_eq!(http.seen.lock().unwrap().len(), 4);
}

#[test]
fn whole_and_streamed_refusals_and_in_band_quota_errors_keep_their_kind() {
    for (value, kind) in [
        (json!({"promptFeedback":{"blockReason":"SAFETY"}}), ErrorKind::BlockedContent),
        (json!({"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":TOKEN}}), ErrorKind::RateLimited),
    ] {
        for streamed in [false, true] {
            let response = if streamed {
                HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "text/event-stream".into())],
                    body: format!("data: {value}\n\n").into_bytes(),
                }
            } else {
                ok(value.clone())
            };
            let client = client(transport(vec![response]));
            let error = if streamed { block_on(client.chat_stream(&request(), &mut |_| {})) } else { block_on(client.chat(&request())) }.unwrap_err();
            assert_eq!(error.kind, kind, "{error:?}, streamed={streamed}");
            assert!(!error.message.contains(TOKEN));
        }
    }
}

#[test]
fn thought_only_previous_turn_does_not_shift_later_signed_call_replay() {
    let http = transport(vec![ok(answer(json!([{"text":"done"}])))]);
    let client = client(http.clone());
    let mut req = request();
    req.tools = vec![super::super::Tool { name: "weather".into(), description: "Weather".into(), input_schema: json!({"type":"object"}), strict: false }];
    let thought = json!([{"text":"thinking","thought":true,"thoughtSignature":"c2lnbg=="}]);
    req.messages
        .push(super::super::Message { role: Role::Assistant, parts: vec![Part::Opaque { dialect: Dialect::Vertex, block: json!({"parts":thought.clone()}) }] });
    req.messages.push(super::super::Message::user("Continue"));
    let call = json!([{"functionCall":{"name":"weather","args":{}},"thoughtSignature":"c2lnbg=="}]);
    req.messages.push(super::super::Message {
        role: Role::Assistant,
        parts: vec![
            Part::ToolUse { id: "call-1".into(), name: "weather".into(), input: json!({}) },
            Part::Opaque { dialect: Dialect::Vertex, block: json!({"parts":call.clone()}) },
        ],
    });
    req.messages
        .push(super::super::Message { role: Role::User, parts: vec![Part::ToolResult { id: "call-1".into(), content: "Sunny".into(), is_error: false }] });
    block_on(client.chat(&req)).unwrap();
    let seen = http.seen.lock().unwrap();
    let body: Value = serde_json::from_slice(&seen[1].body).unwrap();
    let model_turns: Vec<_> = body["contents"].as_array().unwrap().iter().filter(|t| t["role"] == "model").collect();
    assert_eq!(model_turns.len(), 2, "{body}");
    assert_eq!(model_turns[0]["parts"], thought);
    assert_eq!(model_turns[1]["parts"], call);
    assert_eq!(body["contents"][4]["parts"][0]["functionResponse"]["name"], "weather", "{body}");
    drop(seen);
    req.tools.clear();
    let error = block_on(client.chat(&req)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnsupportedInput);
    assert_eq!(http.seen.lock().unwrap().len(), 2, "missing declarations never send another request");
}

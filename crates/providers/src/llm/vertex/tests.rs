//! Vertex AI Gemini chat contract tests against synthetic, public-safe fixtures (#1151): never
//! Google. The service account is the throwaway test key of `google_cloud::tests`, and the access
//! token a made-up string.

use super::*;
use crate::google_cloud::tests::{NOW, PROJECT, fixed_clock, stored};
use crate::http::{ChunkSink, HttpRequest, HttpResponse};
use crate::llm::{Message, Tool};
use crate::provider::BoxFuture;
use futures::executor::block_on;
use std::collections::VecDeque;
use std::sync::Mutex;

const TOKEN: &str = "ya29.vertex-fixture-token";
const MODEL: &str = "gemini-3.8-flash";

/// The token exchange on `send_no_redirect`; Vertex AI's replies, in order, on the other paths,
/// a 2xx body handed over in pieces of `piece` bytes when streamed.
struct Fake {
    tokens: Mutex<VecDeque<HttpResponse>>,
    replies: Mutex<VecDeque<HttpResponse>>,
    piece: usize,
    token_requests: Mutex<usize>,
    seen: Mutex<Vec<HttpRequest>>,
}

fn reply(status: u16, body: &str) -> HttpResponse {
    HttpResponse { status, headers: vec![], body: body.as_bytes().to_vec() }
}

fn granted() -> HttpResponse {
    reply(200, &json!({"access_token": TOKEN, "expires_in": 3599, "token_type": "Bearer"}).to_string())
}

impl Fake {
    fn new(replies: Vec<HttpResponse>) -> Arc<Self> {
        Arc::new(Self {
            tokens: Mutex::new(vec![granted(), granted()].into()),
            replies: Mutex::new(replies.into()),
            piece: 7,
            token_requests: Mutex::new(0),
            seen: Mutex::new(vec![]),
        })
    }
    fn next(&self, request: HttpRequest) -> HttpResponse {
        self.seen.lock().unwrap().push(request);
        self.replies.lock().unwrap().pop_front().unwrap_or_else(|| reply(500, "no fixture reply left"))
    }
    fn request(&self, at: usize) -> HttpRequest {
        self.seen.lock().unwrap()[at].clone()
    }
    fn body(&self, at: usize) -> Value {
        serde_json::from_slice(&self.request(at).body).unwrap()
    }
}

impl Http for Fake {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let r = self.next(request);
        Box::pin(async move { Ok(r) })
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        assert_eq!(request.url, crate::google_cloud::TOKEN_URI, "only the token exchange goes without redirects here");
        *self.token_requests.lock().unwrap() += 1;
        let r = self.tokens.lock().unwrap().pop_front().unwrap_or_else(|| reply(400, r#"{"error":"invalid_grant"}"#));
        Box::pin(async move { Ok(r) })
    }
    fn send_streaming<'a>(&'a self, request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        let r = self.next(request);
        Box::pin(async move {
            if !r.is_success() {
                return Ok(r);
            }
            for chunk in r.body.chunks(self.piece) {
                on_chunk(chunk)?;
            }
            Ok(HttpResponse { body: vec![], ..r })
        })
    }
}

fn client(http: Arc<Fake>) -> LlmClient {
    LlmClient::google_vertex(&stored(), VERTEX_GLOBAL, http, fixed_clock(NOW)).unwrap()
}

fn request() -> ChatRequest {
    ChatRequest {
        model: MODEL.into(),
        system: Some("be brief".into()),
        messages: vec![Message::user("hi")],
        max_tokens: Some(100),
        temperature: Some(0.5),
        tools: vec![],
    }
}

fn weather() -> Tool {
    Tool {
        name: "weather".into(),
        description: "Looks up the weather".into(),
        input_schema: json!({"type":"object","properties":{"city":{"type":"string"}}}),
        strict: false,
    }
}

fn header<'a>(r: &'a HttpRequest, name: &str) -> Option<&'a str> {
    r.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// A server-sent event of `value`, CRLF-ended as Google's front end writes them.
fn sse(values: &[Value]) -> String {
    values.iter().map(|v| format!("data: {v}\r\n\r\n")).collect()
}

const WHOLE: &str = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello","thoughtSignature":"c2lnbmVk"},{"text":" there."}]},"finishReason":"STOP"}],
 "usageMetadata":{"promptTokenCount":12,"candidatesTokenCount":3,"thoughtsTokenCount":40,"totalTokenCount":55},"modelVersion":"gemini-3.8-flash","responseId":"r1"}"#;

#[test]
fn a_whole_chat_goes_to_the_projects_global_model_with_the_token_as_a_secret_header() {
    let http = Fake::new(vec![reply(200, WHOLE)]);
    let mut req = request();
    req.tools = vec![weather()];
    let response = block_on(client(http.clone()).chat(&req)).unwrap();
    assert_eq!(response.text, "Hello there.");
    assert_eq!(response.stop, StopReason::EndTurn);
    assert_eq!(response.usage.input_tokens, Some(12));
    assert_eq!(response.usage.output_tokens, Some(43), "thinking tokens are billed as output");
    let sent = http.request(0);
    assert_eq!(sent.url, format!("https://aiplatform.googleapis.com/v1/projects/{PROJECT}/locations/global/publishers/google/models/{MODEL}:generateContent"));
    assert_eq!(header(&sent, "authorization"), Some(format!("Bearer {TOKEN}").as_str()));
    assert!(sent.secret_headers.iter().any(|n| n.eq_ignore_ascii_case("authorization")), "the token is a secret header");
    assert!(!sent.url.contains("key="), "no key in the query");
    let body = http.body(0);
    assert!(!body.to_string().contains(TOKEN));
    assert_eq!(body["contents"], json!([{"role": "user", "parts": [{"text": "hi", "thought": false}]}]));
    assert_eq!(body["systemInstruction"], json!({"parts": [{"text": "be brief", "thought": false}]}), "no role");
    assert_eq!(body["generationConfig"], json!({"temperature": 0.5, "maxOutputTokens": 100}));
    assert_eq!(
        body["tools"],
        json!([{"functionDeclarations": [{"name": "weather", "description": "Looks up the weather", "parametersJsonSchema": weather().input_schema}]}])
    );
    for absent in ["safetySettings", "toolConfig"] {
        assert!(body.get(absent).is_none(), "{absent} isn't sent as null");
    }
    assert!(!body.to_string().contains("null"), "{body}");
    assert!(!format!("{:?}", client(http.clone())).contains(TOKEN));
}

#[test]
fn a_streamed_chat_asks_for_server_sent_events_and_arrives_piece_by_piece() {
    let events = sse(&[
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"Hel","thoughtSignature":"c2ln"}]}}],"usageMetadata":{"promptTokenCount":9}}),
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"lo"}]}}]}),
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"!"}]},"finishReason":"STOP"}],
               "usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":3,"thoughtsTokenCount":20,"totalTokenCount":32}}),
    ]);
    let http = Fake::new(vec![reply(200, &events)]);
    let mut pieces = Vec::new();
    let response = block_on(client(http.clone()).chat_stream(&request(), &mut |t| pieces.push(t.to_owned()))).unwrap();
    assert_eq!(pieces.concat(), "Hello!");
    assert!(pieces.len() >= 2, "{pieces:?}");
    assert_eq!(response.text, "Hello!");
    assert_eq!((response.usage.input_tokens, response.usage.output_tokens), (Some(9), Some(23)));
    assert!(http.request(0).url.ends_with(&format!("/models/{MODEL}:streamGenerateContent?alt=sse")));
}

#[test]
fn tool_calls_keep_their_signature_and_their_results_go_back_in_call_order() {
    let calls = r#"{"candidates":[{"content":{"role":"model","parts":[
        {"functionCall":{"name":"weather","args":{"city":"Oslo"}},"thoughtSignature":"Zmlyc3Q="},
        {"functionCall":{"name":"weather","args":{"city":"Rome"}}}]},"finishReason":"STOP"}],
        "usageMetadata":{"promptTokenCount":20,"candidatesTokenCount":10}}"#;
    let http = Fake::new(vec![reply(200, calls), reply(200, WHOLE)]);
    let client = client(http.clone());
    let mut req = request();
    req.tools = vec![weather()];
    let first = block_on(client.chat(&req)).unwrap();
    assert_eq!(first.stop, StopReason::ToolUse);
    let got: Vec<(String, String, Value)> = first.tool_calls().map(|c| (c.id.to_owned(), c.name.to_owned(), c.input.clone())).collect();
    assert_eq!(got, [("call_0".into(), "weather".into(), json!({"city": "Oslo"})), ("call_1".into(), "weather".into(), json!({"city": "Rome"}))]);
    // The next turn: the reply as it came, then the results, answered out of order.
    req.messages.push(first.message.clone());
    req.messages.push(Message {
        role: Role::User,
        parts: vec![
            Part::ToolResult { id: "call_1".into(), content: "sunny".into(), is_error: false },
            Part::ToolResult { id: "call_0".into(), content: "no such city".into(), is_error: true },
        ],
    });
    let second = block_on(client.chat(&req)).unwrap();
    assert_eq!(second.text, "Hello there.");
    assert_eq!(*http.token_requests.lock().unwrap(), 1, "the token is held for the next request");
    let body = http.body(1);
    assert_eq!(
        body["contents"][1],
        json!({"role": "model", "parts": [
            {"functionCall": {"name": "weather", "args": {"city": "Oslo"}}, "thoughtSignature": "Zmlyc3Q="},
            {"functionCall": {"name": "weather", "args": {"city": "Rome"}}},
        ]}),
        "the signature goes back as it came, no ids, no placeholder on the unsigned parallel call"
    );
    let responses: Vec<&Value> = body["contents"][2]["parts"].as_array().unwrap().iter().map(|p| &p["functionResponse"]).collect();
    assert_eq!(responses, [&json!({"name": "weather", "response": {"error": "no such city"}}), &json!({"name": "weather", "response": {"output": "sunny"}})]);
}

#[test]
fn a_call_from_another_model_carries_the_placeholder_gemini_3_requires() {
    let http = Fake::new(vec![reply(200, WHOLE)]);
    let mut req = request();
    req.tools = vec![weather()];
    req.messages.push(Message { role: Role::Assistant, parts: vec![Part::ToolUse { id: "toolu_1".into(), name: "weather".into(), input: json!({}) }] });
    req.messages.push(Message { role: Role::User, parts: vec![Part::ToolResult { id: "toolu_1".into(), content: "sunny".into(), is_error: false }] });
    block_on(client(http.clone()).chat(&req)).unwrap();
    let body = http.body(0);
    assert_eq!(body["contents"][1]["parts"][0]["thoughtSignature"], "skip_thought_signature_validator");
    assert!(body["contents"][1]["parts"][0]["functionCall"].get("id").is_none());
}

#[test]
fn a_streamed_tool_call_ends_the_turn_as_a_tool_use() {
    let events = sse(&[
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"Checking."}]}}]}),
        json!({"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"weather","args":{"city":"Oslo"}},"thoughtSignature":"c2ln"}]},"finishReason":"STOP"}],
               "usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":8}}),
    ]);
    let http = Fake::new(vec![reply(200, &events)]);
    let mut req = request();
    req.tools = vec![weather()];
    let response = block_on(client(http).chat_stream(&req, &mut |_| {})).unwrap();
    assert_eq!(response.stop, StopReason::ToolUse);
    assert_eq!(response.text, "Checking.");
    assert_eq!(response.tool_calls().map(|c| c.input.clone()).collect::<Vec<_>>(), [json!({"city": "Oslo"})]);
    assert!(response.message.parts.iter().any(|p| matches!(p, Part::Opaque { block, .. } if block.get(CALL_SIGNATURE).is_some())));
}

#[test]
fn finish_reasons_map_to_stops_and_filters_to_blocked_content() {
    let ended = |reason: &str| {
        let body = json!({"candidates":[{"content":{"role":"model","parts":[{"text":"partial"}]},"finishReason":reason}]}).to_string();
        block_on(client(Fake::new(vec![reply(200, &body)])).chat(&request()))
    };
    assert_eq!(ended("MAX_TOKENS").unwrap().stop, StopReason::MaxTokens);
    for filter in ["SAFETY", "RECITATION", "PROHIBITED_CONTENT", "BLOCKLIST", "SPII", "MODEL_ARMOR"] {
        let e = ended(filter).unwrap_err();
        assert_eq!(e.kind, ErrorKind::BlockedContent, "{filter}: {e:?}");
        assert!(e.message.contains(filter) && !e.message.contains("partial"), "{}", e.message);
    }
    assert_eq!(ended("MALFORMED_FUNCTION_CALL").unwrap().stop, StopReason::Other("MALFORMED_FUNCTION_CALL".into()));
    // Streamed too.
    let events = sse(&[json!({"candidates":[{"content":{"role":"model","parts":[{"text":"x"}]},"finishReason":"SAFETY"}]})]);
    let e = block_on(client(Fake::new(vec![reply(200, &events)])).chat_stream(&request(), &mut |_| {})).unwrap_err();
    assert_eq!(e.kind, ErrorKind::BlockedContent, "{e:?}");
}

#[test]
fn a_blocked_prompt_is_blocked_content_whole_and_streamed() {
    let blocked = r#"{"promptFeedback":{"blockReason":"PROHIBITED_CONTENT"},"usageMetadata":{"promptTokenCount":7}}"#;
    let e = block_on(client(Fake::new(vec![reply(200, blocked)])).chat(&request())).unwrap_err();
    assert_eq!(e.kind, ErrorKind::BlockedContent, "{e:?}");
    assert!(e.message.contains("PROHIBITED_CONTENT"), "{}", e.message);
    let e = block_on(client(Fake::new(vec![reply(200, &sse(&[serde_json::from_str(blocked).unwrap()]))])).chat_stream(&request(), &mut |_| {})).unwrap_err();
    assert_eq!(e.kind, ErrorKind::BlockedContent, "{e:?}");
}

fn google_error(status: u16, canonical: &str, message: &str, details: Value) -> HttpResponse {
    reply(status, &json!({"error": {"code": status, "message": message, "status": canonical, "details": details}}).to_string())
}

#[test]
fn googles_errors_map_to_kinds_and_a_refused_token_is_dropped() {
    let failed = |r: HttpResponse| block_on(client(Fake::new(vec![r])).chat(&request())).unwrap_err();
    assert_eq!(failed(google_error(403, "PERMISSION_DENIED", "denied", json!([]))).kind, ErrorKind::Auth);
    let billing = json!([{"@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "BILLING_DISABLED"}]);
    assert_eq!(failed(google_error(403, "PERMISSION_DENIED", "billing", billing)).kind, ErrorKind::Budget);
    assert_eq!(failed(google_error(404, "NOT_FOUND", "Publisher model not found", json!([]))).kind, ErrorKind::NotFound);
    let mut limited = google_error(429, "RESOURCE_EXHAUSTED", "Resource exhausted. Please try again later.", json!([]));
    limited.headers.push(("Retry-After".into(), "7".into()));
    let e = failed(limited);
    assert_eq!((e.kind, e.retry_after_ms), (ErrorKind::RateLimited, Some(7000)));
    let daily = json!([{"@type": "type.googleapis.com/google.rpc.QuotaFailure", "violations": [{"subject": "generate_content_requests_per_day"}]}]);
    assert_eq!(failed(google_error(429, "RESOURCE_EXHAUSTED", "Quota exceeded", daily)).kind, ErrorKind::Budget);
    let long =
        failed(google_error(400, "INVALID_ARGUMENT", "The input token count (2000000) exceeds the maximum number of tokens allowed (1048576).", json!([])));
    assert!(long.is_context_limit(), "{long:?}");
    let bad = failed(google_error(400, "INVALID_ARGUMENT", "Invalid JSON payload received.", json!([])));
    assert_eq!(bad.kind, ErrorKind::UnsupportedInput);
    assert_eq!(failed(google_error(503, "UNAVAILABLE", "overloaded", json!([]))).kind, ErrorKind::Busy);
    assert_eq!(failed(reply(500, "<html>oops</html>")).kind, ErrorKind::Server);

    // A 401 drops the held token: the next request asks for a new one.
    let http = Fake::new(vec![google_error(401, "UNAUTHENTICATED", "Request had invalid authentication credentials.", json!([])), reply(200, WHOLE)]);
    let client = client(http.clone());
    assert_eq!(block_on(client.chat(&request())).unwrap_err().kind, ErrorKind::Auth);
    block_on(client.chat(&request())).unwrap();
    assert_eq!(*http.token_requests.lock().unwrap(), 2);
}

#[test]
fn an_error_never_shows_the_token() {
    let echo = google_error(400, "INVALID_ARGUMENT", &format!("bad header Bearer {TOKEN}"), json!([]));
    let e = block_on(client(Fake::new(vec![echo])).chat(&request())).unwrap_err();
    assert!(!e.message.contains(TOKEN), "{}", e.message);
    let event = sse(&[json!({"error": {"code": 500, "message": format!("internal {TOKEN}"), "status": "INTERNAL"}})]);
    let e = block_on(client(Fake::new(vec![reply(200, &event)])).chat_stream(&request(), &mut |_| {})).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Server, "{e:?}");
    assert!(!e.message.contains(TOKEN), "{}", e.message);
}

#[test]
fn a_refused_token_exchange_sends_nothing_to_vertex() {
    let http = Fake::new(vec![reply(200, WHOLE)]);
    *http.tokens.lock().unwrap() = vec![reply(400, r#"{"error":"invalid_grant"}"#)].into();
    let e = block_on(client(http.clone()).chat(&request())).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth);
    assert!(http.seen.lock().unwrap().is_empty());
}

#[test]
fn hostile_streams_are_refused_before_the_crate_reads_them() {
    let refused = |event: Value| {
        let http = Fake::new(vec![reply(200, &sse(&[event]))]);
        let e = block_on(client(http).chat_stream(&request(), &mut |_| {})).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Malformed, "{e:?}");
        e.message
    };
    let many: Vec<Value> = (0..9).map(|i| json!({"index": i, "content": {"parts": [{"text": "x"}]}})).collect();
    assert!(refused(json!({"candidates": many})).contains("candidates"));
    assert!(refused(json!({"candidates": [{"index": 4_000_000_000u64, "content": {"parts": [{"text": "x"}]}}]})).contains("candidates"));
    assert!(refused(json!({"candidates": [{"index": -1, "content": {"parts": [{"text": "x"}]}}]})).contains("candidates"));
    let grounding = json!({"groundingSupports": [{"segment": {"startIndex": 0, "endIndex": 1, "text": "x"}, "groundingChunkIndices": [0]}]});
    assert!(refused(json!({"candidates": [{"content": {"parts": [{"text": "x"}]}, "groundingMetadata": grounding}]})).contains("grounding"));
    let citations: Vec<Value> = (0..=MAX_CITATIONS).map(|_| json!({"uri": "https://example.com"})).collect();
    assert!(refused(json!({"candidates": [{"content": {"parts": [{"text": "x"}]}, "citationMetadata": {"citations": citations}}]})).contains("citations"));
    let calls: Vec<Value> = (0..=MAX_REPLY_BLOCKS).map(|_| json!({"functionCall": {"name": "weather", "args": {}}})).collect();
    assert!(refused(json!({"candidates": [{"content": {"parts": calls}}]})).contains("too many parts"));
    // Many small text pieces are one block: a long answer is fine.
    let pieces: Vec<Value> = (0..2000).map(|_| json!({"candidates": [{"content": {"parts": [{"text": "a"}]}}]})).collect();
    let mut events = sse(&pieces);
    events.push_str(&sse(&[json!({"candidates": [{"content": {"parts": [{"text": "."}]}, "finishReason": "STOP"}]})]));
    let response = block_on(client(Fake::new(vec![reply(200, &events)])).chat_stream(&request(), &mut |_| {})).unwrap();
    assert_eq!(response.text.len(), 2001);
}

#[test]
fn a_whole_reply_loses_its_grounding_before_the_crate_reads_it() {
    let body = json!({"candidates": [{"content": {"parts": [{"text": "x"}]}, "finishReason": "STOP",
        "groundingMetadata": {"groundingChunks": [{"web": {"uri": "https://example.com"}}],
            "groundingSupports": [{"segment": {"startIndex": 0, "endIndex": 1, "text": "x"}, "groundingChunkIndices": [0, 99]}]}}]});
    let response = block_on(client(Fake::new(vec![reply(200, &body.to_string())])).chat(&request())).unwrap();
    assert_eq!(response.text, "x");
}

#[test]
fn requests_vertex_would_refuse_are_refused_before_sending() {
    let http = Fake::new(vec![]);
    let client = client(http.clone());
    let mut gif = request();
    gif.messages[0].parts.push(Part::Image { mime: "image/gif".into(), bytes: Arc::from(&b"GIF89a"[..]) });
    assert_eq!(block_on(client.chat(&gif)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    let mut prefill = request();
    prefill.messages.push(Message::assistant("Sure"));
    assert_eq!(block_on(client.chat(&prefill)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    for model in ["", "../other:generateContent", "Gemini-3", "gemini?x=1"] {
        let mut bad = request();
        bad.model = model.into();
        assert_eq!(block_on(client.chat(&bad)).unwrap_err().kind, ErrorKind::UnsupportedInput, "{model:?}");
    }
    assert!(http.seen.lock().unwrap().is_empty());
    assert_eq!(*http.token_requests.lock().unwrap(), 0, "not even the token is asked for");
    assert_eq!(block_on(client.list_models()).unwrap_err().kind, ErrorKind::UnsupportedInput);
}

#[test]
fn locations_are_checked_and_pick_the_regional_host() {
    for ok in ["global", "us-central1", "europe-west4", "northamerica-northeast1", "me-central2"] {
        validate_vertex_location(ok).unwrap();
    }
    for bad in ["", "us", "US-central1", "us-central", "us-central1/x", "us.central1", "a-b-c-d1", "global ", "us--central1"] {
        assert!(validate_vertex_location(bad).is_err(), "{bad:?}");
    }
    let http = Fake::new(vec![reply(200, WHOLE)]);
    let regional = LlmClient::google_vertex(&stored(), "europe-west4", http.clone(), fixed_clock(NOW)).unwrap();
    block_on(regional.chat(&request())).unwrap();
    assert_eq!(
        http.request(0).url,
        format!("https://europe-west4-aiplatform.googleapis.com/v1/projects/{PROJECT}/locations/europe-west4/publishers/google/models/{MODEL}:generateContent")
    );
    assert!(LlmClient::google_vertex(&stored(), "nowhere", http, fixed_clock(NOW)).is_err());
}

#[test]
fn express_mode_sends_its_key_as_a_header_to_the_global_publisher_path() {
    let http = Fake::new(vec![reply(200, WHOLE)]);
    let client = LlmClient::google_vertex_express(ApiKey::new("express-fixture-key"), http.clone());
    assert_eq!(block_on(client.chat(&request())).unwrap().text, "Hello there.");
    let sent = http.request(0);
    assert_eq!(sent.url, format!("https://aiplatform.googleapis.com/v1/publishers/google/models/{MODEL}:generateContent"));
    assert_eq!(header(&sent, "x-goog-api-key"), Some("express-fixture-key"));
    assert!(header(&sent, "authorization").is_none());
    assert_eq!(*http.token_requests.lock().unwrap(), 0);
}

#[test]
fn thoughts_and_other_parts_go_back_as_they_came() {
    let body = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"thinking","thought":true,"thoughtSignature":"dGg="},{"text":"Done."},{"executableCode":{"language":"PYTHON","code":"1+1"}}]},"finishReason":"STOP"}]}"#;
    let http = Fake::new(vec![reply(200, body), reply(200, WHOLE)]);
    let client = client(http.clone());
    let first = block_on(client.chat(&request())).unwrap();
    assert_eq!(first.text, "Done.");
    let mut req = request();
    req.messages.push(first.message);
    req.messages.push(Message::user("and?"));
    block_on(client.chat(&req)).unwrap();
    let parts = &http.body(1)["contents"][1]["parts"];
    assert_eq!(parts[0], json!({"text": "thinking", "thought": true, "thoughtSignature": "dGg="}));
    assert_eq!(parts[2], json!({"executableCode": {"language": "PYTHON", "code": "1+1"}}));
}

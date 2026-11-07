//! The Anthropic path end to end against canned replies: what is sent, and how replies, streams
//! and errors read (#1122 keeps every one of these as it was before the move to rig).

use super::super::tests::{Canned, request};
use super::super::{ApiKey, Endpoint, Message, Tool};
use super::*;
use crate::http::{ChunkSink, Http, HttpRequest, HttpResponse};
use crate::provider::BoxFuture;
use futures::executor::block_on;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

const OK: &str = r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#;

fn client(http: Arc<dyn Http>) -> LlmClient {
    LlmClient::new(Endpoint::anthropic(ApiKey::new("sk-ant")), http)
}

/// The body `req` goes out as.
fn sent(req: &ChatRequest) -> Value {
    let http = Canned::new(200, OK);
    block_on(client(http.clone()).chat(req)).unwrap();
    http.body()
}

fn whole(reply: &str) -> Result<ChatResponse, ProviderError> {
    let mut r = request();
    r.temperature = None;
    block_on(client(Canned::new(200, reply)).chat(&r))
}

/// Streams `body` back in pieces of `piece` bytes; an error status sends its body whole.
struct Pieces {
    body: Vec<u8>,
    piece: usize,
    status: u16,
    headers: Vec<(String, String)>,
}

impl Http for Pieces {
    fn send(&self, _request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::Server, "only streamed requests are expected")) })
    }
    fn send_streaming<'a>(&'a self, _request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        Box::pin(async move {
            if !(200..300).contains(&self.status) {
                return Ok(HttpResponse { status: self.status, headers: self.headers.clone(), body: self.body.clone() });
            }
            for chunk in self.body.chunks(self.piece.max(1)) {
                on_chunk(chunk)?;
            }
            Ok(HttpResponse { status: self.status, headers: self.headers.clone(), body: Vec::new() })
        })
    }
}

/// `events` as an SSE body, each `(name, data)`.
fn sse(events: &[(&str, &str)]) -> Vec<u8> {
    events.iter().map(|(name, data)| format!("event: {name}\ndata: {data}\n\n")).collect::<String>().into_bytes()
}

fn streamed_in(events: &[(&str, &str)], piece: usize) -> (Result<ChatResponse, ProviderError>, Vec<String>) {
    let http = Arc::new(Pieces { body: sse(events), piece, status: 200, headers: vec![] });
    let mut pieces = Vec::new();
    let mut r = request();
    r.temperature = None;
    let reply = block_on(client(http).chat_stream(&r, &mut |t| pieces.push(t.to_owned())));
    (reply, pieces)
}

fn streamed(events: &[(&str, &str)]) -> (Result<ChatResponse, ProviderError>, Vec<String>) {
    streamed_in(events, 7)
}

const START: (&str, &str) = ("message_start", r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":12,"output_tokens":1}}}"#);
const STOP: (&str, &str) = ("message_stop", r#"{"type":"message_stop"}"#);

#[test]
fn request_shape() {
    let http = Canned::new(200, OK);
    let c = client(http.clone());
    let mut r = request();
    r.model = DEFAULT_MODEL.into();
    r.temperature = None;
    block_on(c.chat(&r)).unwrap();
    let req = http.request();
    assert_eq!(req.url, "https://api.anthropic.com/v1/messages");
    assert!(req.headers.contains(&("x-api-key".into(), "sk-ant".into())));
    assert!(req.secret_headers.contains(&"x-api-key".to_owned()), "the key is a secret header");
    assert!(req.headers.contains(&("anthropic-version".into(), VERSION.into())));
    assert!(!req.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("authorization")));
    let names: Vec<String> = req.headers.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
    assert!(!names.iter().any(|n| n == "user-agent" || n == "anthropic-beta" || n.contains("session")), "{names:?}");
    assert_eq!(names.iter().filter(|n| *n == "x-api-key").count(), 1, "one key header");
    let b = http.body();
    assert_eq!(b["model"], "claude-opus-5-5");
    assert_eq!(b["system"], "be brief");
    assert_eq!(b["messages"][0]["content"][0], json!({"type":"text","text":"hi"}), "the user's order is kept");
    assert_eq!(b["messages"][0]["content"][1], json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"UE5H"}}));
    assert_eq!(b["max_tokens"], 100);
    assert!(b.get("stream").is_none());
    let unset = sent(&ChatRequest { max_tokens: None, temperature: None, ..request() });
    assert_eq!(unset["max_tokens"], super::super::DEFAULT_MAX_TOKENS, "required by the API");
    assert!(rejects_sampling("claude-opus-5-5") && rejects_sampling("anthropic/claude-sonnet-5-5") && !rejects_sampling("claude-3-5-haiku"));
    assert!(b.get("temperature").is_none() && b.get("thinking").is_none() && b.get("tool_choice").is_none());
    let warm = sent(&ChatRequest { model: "claude-3-5-haiku".into(), temperature: Some(0.7), ..request() });
    assert_eq!(warm["temperature"].to_string(), "0.7", "the temperature as it was written");
}

#[test]
fn a_custom_base_url_keeps_its_own_path() {
    let http = Canned::new(200, OK);
    let base = crate::base_url::BaseUrl::parse("https://gateway.example/claude").unwrap();
    let c = LlmClient::with_base_url(Endpoint::anthropic(ApiKey::new("k")), &base, http.clone());
    let mut r = request();
    r.temperature = None;
    block_on(c.chat(&r)).unwrap();
    assert_eq!(http.request().url, "https://gateway.example/claude/messages");
}

#[test]
fn replies() {
    let r = whole(r#"{"content":[{"type":"thinking","thinking":""},{"type":"text","text":"Hello "},{"type":"text","text":"there"}],"stop_reason":"max_tokens","usage":{"input_tokens":4,"output_tokens":9}}"#).unwrap();
    assert_eq!((r.text.as_str(), r.stop, r.usage.input_tokens, r.usage.output_tokens), ("Hello there", StopReason::MaxTokens, Some(4), Some(9)));
    let refused =
        whole(r#"{"content":[{"type":"text","text":"partial"}],"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"}}"#).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::BlockedContent);
    assert_eq!(whole(r#"{"type":"message"}"#).unwrap_err().kind, ErrorKind::Malformed);
    assert_eq!(whole("<html>").unwrap_err().kind, ErrorKind::Malformed);
    let odd = whole(r#"{"content":[{"type":"text","text":"x"}],"stop_reason":"pause_turn"}"#).unwrap();
    assert_eq!(odd.stop, StopReason::Other("pause_turn".into()));
    let many = json!({"content": (0..=MAX_REPLY_BLOCKS).map(|_| json!({"type": "text", "text": "x"})).collect::<Vec<_>>(), "stop_reason": "end_turn"});
    assert_eq!(whole(&many.to_string()).unwrap_err().kind, ErrorKind::Malformed, "too many parts");
}

#[test]
fn errors() {
    let e = |s, b: &str| error(s, b.as_bytes()).kind;
    assert_eq!(e(529, r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#), ErrorKind::Busy);
    assert_eq!(e(529, ""), ErrorKind::Busy);
    assert_eq!(e(429, r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow"}}"#), ErrorKind::RateLimited);
    assert_eq!(e(402, r#"{"type":"error","error":{"type":"billing_error","message":"pay"}}"#), ErrorKind::Budget);
    assert_eq!(e(401, r#"{"type":"error","error":{"type":"authentication_error","message":"bad key"}}"#), ErrorKind::Auth);
    assert_eq!(e(404, r#"{"type":"error","error":{"type":"not_found_error","message":"model"}}"#), ErrorKind::NotFound);
    assert_eq!(e(400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#), ErrorKind::UnsupportedInput);
    assert_eq!(e(500, r#"{"type":"error","error":{"type":"api_error","message":"oops"}}"#), ErrorKind::Server);
    let long = error(400, br#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#);
    assert!(long.is_context_limit() && long.message.contains("210000 tokens"), "{long}");
    assert!(!error(400, br#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#).is_context_limit());
    assert!(ErrorKind::Busy.is_transient() && !ErrorKind::Budget.is_transient());
}

/// Error replies come back through the client with the same kinds, and their Retry-After.
#[test]
fn error_replies_through_the_client() {
    let mut r = request();
    r.temperature = None;
    let limited =
        Canned::with_headers(429, r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow"}}"#, vec![("retry-after".into(), "3".into())]);
    let e = block_on(client(limited).chat(&r)).unwrap_err();
    assert_eq!((e.kind, e.retry_after_ms, e.message.as_str()), (ErrorKind::RateLimited, Some(3000), "slow"));
    let e = block_on(client(Canned::new(529, "")).chat(&r)).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Busy);
    let http =
        Arc::new(Pieces { body: br#"{"type":"error","error":{"type":"billing_error","message":"pay"}}"#.to_vec(), piece: 3, status: 402, headers: vec![] });
    assert_eq!(block_on(client(http).chat_stream(&r, &mut |_| {})).unwrap_err().kind, ErrorKind::Budget);
    struct Down;
    impl Http for Down {
        fn send(&self, _request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "fixture down")) })
        }
    }
    let e = block_on(client(Arc::new(Down)).chat(&r)).unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Network, "fixture down"));
}

#[test]
fn streaming_events() {
    let (r, pieces) = streamed(&[
        START,
        ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
        ("ping", r#"{"type":"ping"}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}"#),
        STOP,
    ]);
    let r = r.unwrap();
    assert_eq!(pieces, ["Hel", "lo"]);
    assert_eq!((r.text.as_str(), r.stop, r.usage.input_tokens, r.usage.output_tokens), ("Hello", StopReason::EndTurn, Some(12), Some(12)));
    let (e, _) = streamed(&[START, ("error", r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#)]);
    assert_eq!(e.unwrap_err().kind, ErrorKind::Busy, "an error event keeps its kind");
    let (e, _) = streamed(&[
        START,
        ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"no"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#),
        STOP,
    ]);
    assert_eq!(e.unwrap_err().kind, ErrorKind::BlockedContent);
    // Cut off before `message_stop`.
    let (e, pieces) = streamed(&[
        START,
        ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"cut"}}"#),
    ]);
    let e = e.unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Network, "the reply stream ended early"));
    assert_eq!(pieces, ["cut"]);
}

/// Pieces split anywhere, even inside a line or a multi-byte character, read the same.
#[test]
fn streams_split_anywhere() {
    let events = [
        START,
        ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Grüße ☔"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#),
        STOP,
    ];
    for piece in [1, 2, 5, 64, 4096] {
        let (r, pieces) = streamed_in(&events, piece);
        assert_eq!(r.unwrap().text, "Grüße ☔", "{piece}");
        assert_eq!(pieces.concat(), "Grüße ☔", "{piece}");
    }
}

#[test]
fn streamed_requests_and_spend() {
    let headers = vec![("x-litellm-response-cost".to_owned(), "0.0012".to_owned())];
    let body = sse(&[START, ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#), STOP]);
    let seen = Arc::new(Mutex::new(Vec::<HttpRequest>::new()));
    struct Seen(Arc<Mutex<Vec<HttpRequest>>>, Pieces);
    impl Http for Seen {
        fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            self.1.send(request)
        }
        fn send_streaming<'a>(&'a self, request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
            self.0.lock().unwrap().push(request.clone());
            self.1.send_streaming(request, on_chunk)
        }
    }
    let http = Arc::new(Seen(seen.clone(), Pieces { body, piece: 9, status: 200, headers }));
    let mut r = request();
    r.temperature = None;
    let reply = block_on(client(http).chat_stream(&r, &mut |_| {})).unwrap();
    assert_eq!(reply.usage.cost, Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 1200)));
    let req = seen.lock().unwrap()[0].clone();
    let b: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(b["stream"], true);
    assert_eq!(req.timeout_ms, super::super::STREAM_TIMEOUT_MS);
    assert_eq!(req.max_response_bytes, super::super::MAX_REPLY_BYTES, "100 tokens: the floor");
}

fn weather() -> Tool {
    Tool {
        name: "get_weather".into(),
        description: "Current weather for a city".into(),
        input_schema: json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"], "additionalProperties": false}),
        strict: true,
    }
}

fn tool_turns() -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        system: None,
        messages: vec![
            Message::user("Weather in Oslo?"),
            Message {
                role: Role::Assistant,
                parts: vec![
                    Part::Text("Let me check.".into()),
                    Part::ToolUse { id: "call_1".into(), name: "get_weather".into(), input: json!({"city": "Oslo"}) },
                ],
            },
            Message {
                role: Role::User,
                parts: vec![Part::Text("Thanks".into()), Part::ToolResult { id: "call_1".into(), content: "4 °C, rain".into(), is_error: false }],
            },
        ],
        max_tokens: Some(100),
        temperature: None,
        tools: vec![weather()],
    }
}

#[test]
fn tool_requests() {
    let mut r = tool_turns();
    r.messages[1].parts.insert(0, Part::Opaque { dialect: Dialect::Anthropic, block: json!({"type": "thinking", "thinking": "plan", "signature": "sig"}) });
    r.messages[2].parts.push(Part::ToolResult { id: "call_2".into(), content: "again".into(), is_error: true });
    r.messages[1].parts.push(Part::ToolUse { id: "call_2".into(), name: "get_weather".into(), input: json!("not an object") });
    let b = sent(&r);
    assert_eq!(
        b["tools"][0],
        json!({"name": "get_weather", "description": "Current weather for a city", "input_schema": weather().input_schema, "strict": true})
    );
    assert!(b.get("tool_choice").is_none(), "auto: current Claude models reject forced calls");
    let assistant = &b["messages"][1]["content"];
    assert_eq!(assistant[0], json!({"type": "thinking", "thinking": "plan", "signature": "sig"}), "thinking goes back unchanged");
    assert_eq!(assistant[1], json!({"type": "text", "text": "Let me check."}));
    assert_eq!(assistant[2], json!({"type": "tool_use", "id": "call_1", "name": "get_weather", "input": {"city": "Oslo"}}));
    assert_eq!(assistant[3]["input"], json!({}), "non-object input can't be sent to Claude");
    let user = &b["messages"][2]["content"];
    assert_eq!(user[0], json!({"type": "tool_result", "tool_use_id": "call_1", "content": "4 °C, rain"}), "results first");
    assert_eq!(user[1], json!({"type": "tool_result", "tool_use_id": "call_2", "content": "again", "is_error": true}));
    assert_eq!(user[2], json!({"type": "text", "text": "Thanks"}));
    // An empty failed result goes as it was written.
    let mut r = tool_turns();
    r.messages[2].parts[1] = Part::ToolResult { id: "call_1".into(), content: String::new(), is_error: true };
    assert_eq!(sent(&r)["messages"][2]["content"][0], json!({"type": "tool_result", "tool_use_id": "call_1", "content": "", "is_error": true}));
}

#[test]
fn tool_replies_keep_thinking() {
    let r = whole(
        r#"{"content":[
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
    assert_eq!(whole(r#"{"content":[{"type":"tool_use","name":"f","input":{}}],"stop_reason":"tool_use"}"#).unwrap_err().kind, ErrorKind::Malformed);
}

#[test]
fn streamed_tool_calls_and_thinking() {
    let (r, pieces) = streamed(&[
        ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":30}}}"#),
        ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Use the "}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"tool."}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQB"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Checking."}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":1}"#),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#,
        ),
        ("content_block_delta", r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"city\": \"Os"}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"lo\"}"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":2}"#),
        ("content_block_start", r#"{"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"toolu_2","name":"get_time","input":{}}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":3}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":40}}"#),
        STOP,
    ]);
    let r = r.unwrap();
    assert_eq!(pieces.concat(), "Checking.");
    assert_eq!(
        r.message.parts,
        vec![
            Part::Opaque { dialect: Dialect::Anthropic, block: json!({"type": "thinking", "thinking": "Use the tool.", "signature": "EqQB"}) },
            Part::Text("Checking.".into()),
            Part::ToolUse { id: "toolu_1".into(), name: "get_weather".into(), input: json!({"city": "Oslo"}) },
            Part::ToolUse { id: "toolu_2".into(), name: "get_time".into(), input: json!({}) },
        ]
    );
    assert_eq!((r.usage.input_tokens, r.usage.output_tokens, r.stop), (Some(30), Some(40), StopReason::ToolUse));
    // Server tool blocks keep their streamed input.
    let (r, _) = streamed(&[
        START,
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srv","name":"web_search","input":{}}}"#,
        ),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"oslo\"}"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#),
        STOP,
    ]);
    assert!(matches!(&r.unwrap().message.parts[0], Part::Opaque { block, .. } if block["input"] == json!({"query": "oslo"})));
    // An unnamed call is malformed.
    let (e, _) = streamed(&[
        START,
        ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#),
        STOP,
    ]);
    assert!(e.is_err(), "{e:?}");
}

#[test]
fn pdfs_are_sent_as_documents() {
    let mut r = request();
    r.temperature = None;
    r.messages[0].parts = vec![Part::Text("read this".into()), Part::Document { mime: "application/pdf".into(), bytes: Arc::from(vec![b'%'; 3]) }];
    assert_eq!(
        sent(&r)["messages"][0]["content"][1],
        json!({"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "JSUl"}})
    );
}

/// Long streamed replies stay linear.
#[test]
fn long_streams_are_linear() {
    let mut events = vec![START, ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#)];
    let delta = ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"abcdefgh"}}"#);
    events.extend(std::iter::repeat_n(delta, 50_000));
    events.extend([
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#),
        STOP,
    ]);
    let started = std::time::Instant::now();
    let http = Arc::new(Pieces { body: sse(&events), piece: 64 * 1024, status: 200, headers: vec![] });
    let mut r = request();
    r.temperature = None;
    r.max_tokens = Some(128_000);
    let reply = block_on(client(http).chat_stream(&r, &mut |_| {})).unwrap();
    assert!(matches!(&reply.message.parts[0], Part::Opaque { block, .. } if block["thinking"].as_str().map(str::len) == Some(400_000)));
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
}

/// Hands the body over a piece at a time, letting the reader run between pieces and after the
/// last one before the send ends, as a real connection does.
struct Slow {
    body: Vec<u8>,
    headers: Vec<(String, String)>,
    finished: AtomicBool,
}

async fn yield_once() {
    let mut yielded = false;
    futures::future::poll_fn(|cx| {
        if yielded {
            return std::task::Poll::Ready(());
        }
        yielded = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    })
    .await;
}

impl Http for Slow {
    fn send(&self, _request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::Server, "only streamed requests are expected")) })
    }
    fn send_streaming<'a>(&'a self, _request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        Box::pin(async move {
            for piece in self.body.chunks(11) {
                yield_once().await;
                on_chunk(piece)?;
            }
            yield_once().await;
            yield_once().await;
            self.finished.store(true, Ordering::SeqCst);
            Ok(HttpResponse { status: 200, headers: self.headers.clone(), body: Vec::new() })
        })
    }
}

#[test]
fn a_stream_read_to_its_last_event_still_ends_its_send() {
    let slow = || {
        let body = sse(&[
            START,
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#),
            STOP,
        ]);
        Arc::new(Slow { body, headers: vec![("x-litellm-response-cost".into(), "0.0012".into())], finished: AtomicBool::new(false) })
    };
    let mut r = request();
    r.temperature = None;
    let http = slow();
    let mut pieces = Vec::new();
    let reply = block_on(client(http.clone()).chat_stream(&r, &mut |t| pieces.push(t.to_owned()))).unwrap();
    assert_eq!((reply.text.as_str(), pieces.len()), ("Hello", 2));
    assert_eq!(reply.usage.cost, Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 1200)));
    assert!(http.finished.load(Ordering::SeqCst));
    let http = slow();
    let c = client(http.clone());
    let context = crate::SubmitContext::new();
    context.arm();
    block_on(c.streamed(&r, &mut |_| {}, Some(&context))).unwrap();
    assert!(http.finished.load(Ordering::SeqCst), "the send ran to its end");
    assert!(context.finish_error().is_none(), "a sent request may be billed");
}

/// Hands over `body`, then never ends; notes when its send is dropped.
struct Stuck {
    body: Vec<u8>,
    dropped: Arc<AtomicBool>,
}

struct Noted(Arc<AtomicBool>);

impl Drop for Noted {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Http for Stuck {
    fn send(&self, _request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::Server, "only streamed requests are expected")) })
    }
    fn send_streaming<'a>(&'a self, _request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        let noted = Noted(Arc::clone(&self.dropped));
        Box::pin(async move {
            let _noted = noted;
            on_chunk(&self.body)?;
            futures::future::pending::<()>().await;
            Ok(HttpResponse { status: 200, headers: vec![], body: Vec::new() })
        })
    }
}

#[test]
fn a_stream_that_fails_or_is_cancelled_leaves_nothing_behind() {
    let mut r = request();
    r.temperature = None;
    // An error event while the connection stays open.
    let dropped = Arc::new(AtomicBool::new(false));
    let body = sse(&[START, ("error", r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#)]);
    let http = Arc::new(Stuck { body, dropped: dropped.clone() });
    let c = client(http.clone());
    let context = crate::SubmitContext::new();
    context.arm();
    assert_eq!(block_on(c.streamed(&r, &mut |_| {}, Some(&context))).unwrap_err().kind, ErrorKind::Busy);
    assert!(dropped.load(Ordering::SeqCst), "the send was dropped");
    assert_eq!(Arc::strong_count(&http), 2, "only this test and the client hold the transport");
    assert!(context.finish_error().is_none(), "possibly billed");
    // Cancelled mid-stream.
    let dropped = Arc::new(AtomicBool::new(false));
    let body = sse(&[
        START,
        ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#),
    ]);
    let http = Arc::new(Stuck { body, dropped: dropped.clone() });
    let c = client(http.clone());
    let context = crate::SubmitContext::new();
    context.arm();
    let mut pieces = Vec::new();
    {
        let mut on_text = |t: &str| pieces.push(t.to_owned());
        let mut work = Box::pin(c.streamed(&r, &mut on_text, Some(&context)));
        let waker = futures::task::noop_waker();
        for _ in 0..20 {
            assert!(work.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
        }
    }
    assert_eq!(pieces, ["Hel"]);
    assert!(dropped.load(Ordering::SeqCst), "the send was dropped");
    assert_eq!(Arc::strong_count(&http), 2);
    assert!(context.finish_error().is_none(), "possibly billed");
}

/// A block index from the wire is refused past the reply's limit before anything is sized by it:
/// one small hostile event must not allocate the index's worth of slots.
#[test]
fn a_huge_block_index_is_refused_not_allocated() {
    for index in ["100000000000", "1024", "4294967295"] {
        let start = format!(r#"{{"type":"content_block_start","index":{index},"content_block":{{"type":"text","text":""}}}}"#);
        let (e, _) = streamed(&[START, ("content_block_start", &start), STOP]);
        let e = e.unwrap_err();
        assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Malformed, "the reply has too many parts"), "{index}");
    }
    let delta = r#"{"type":"content_block_delta","index":18446744073709551615,"delta":{"type":"text_delta","text":"x"}}"#;
    assert_eq!(streamed(&[START, ("content_block_delta", delta), STOP]).0.unwrap_err().kind, ErrorKind::Malformed);
}

/// A gap in the block indexes leaves no empty block to send back.
#[test]
fn a_gap_in_block_indexes_adds_no_block() {
    let (r, _) = streamed(&[
        START,
        ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"a"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ("content_block_start", r#"{"type":"content_block_start","index":2,"content_block":{"type":"thinking","thinking":"t","signature":"s"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":2}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#),
        STOP,
    ]);
    let r = r.unwrap();
    assert_eq!(
        r.message.parts,
        vec![Part::Text("a".into()), Part::Opaque { dialect: Dialect::Anthropic, block: json!({"type": "thinking", "thinking": "t", "signature": "s"}) }]
    );
}

/// A gateway that sends `"stop_reason": null` and then `message_stop` ends the turn, as before.
#[test]
fn a_stream_without_a_stop_reason_ends_at_message_stop() {
    let (r, pieces) = streamed(&[
        START,
        ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":null},"usage":{"output_tokens":7}}"#),
        STOP,
    ]);
    let r = r.unwrap();
    assert_eq!(pieces, ["done"]);
    assert_eq!((r.text.as_str(), r.stop, r.usage.input_tokens, r.usage.output_tokens), ("done", StopReason::EndTurn, Some(12), Some(7)));
}

/// The ways an index could reach the wire's reader unchecked: a lone CR between events, a BOM
/// opening the stream, an index written as a float or a string. Each is refused by the guard.
#[test]
fn an_index_cannot_slip_past_the_guard() {
    let huge = r#"{"type":"content_block_start","index":100000000000,"content_block":{"type":"text","text":""}}"#;
    let refused = |body: Vec<u8>| {
        let http = Arc::new(Pieces { body, piece: 5, status: 200, headers: vec![] });
        let mut r = request();
        r.temperature = None;
        let e = block_on(client(http).chat_stream(&r, &mut |_| {})).unwrap_err();
        assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Malformed, "the reply has too many parts"));
    };
    let start = START.1;
    refused(format!("data: {start}\r\rdata: {{\"type\":\"ping\"}}\r\rdata: {huge}\r\r\n\n").into_bytes());
    refused(format!("\u{feff}data: {huge}\n\n").into_bytes());
    for index in ["1e11", "100000000000.0", "\"100000000000\"", "\" 5000 \"", "-1", "null"] {
        let event = format!(r#"{{"type":"content_block_start","index":{index},"content_block":{{"type":"text","text":""}}}}"#);
        refused(sse(&[START, ("content_block_start", &event), STOP]));
    }
}

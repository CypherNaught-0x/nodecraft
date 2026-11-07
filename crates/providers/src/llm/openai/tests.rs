//! The OpenAI-compatible path end to end against canned replies: what is sent, and how replies,
//! streams and errors read (#1122 keeps every one of these as it was before the move to rig).

use super::super::tests::{Canned, request};
use super::super::{ApiKey, Dialect, Endpoint, LlmClient, Message, Tool};
use super::*;
use crate::http::{ChunkSink, Http, HttpRequest, HttpResponse};
use crate::provider::BoxFuture;
use futures::executor::block_on;
use serde_json::json;
use std::sync::Mutex;

fn client(http: Arc<Canned>) -> LlmClient {
    LlmClient::new(Endpoint::openai_compatible("vllm", "http://127.0.0.1:8000/v1", None), http)
}

/// The body `req` goes out as to an OpenAI-compatible server (`max_tokens`), or to OpenAI.
fn sent(req: &ChatRequest, openai: bool) -> Value {
    let http = Canned::new(200, r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}"#);
    let c = if openai { LlmClient::new(Endpoint::openai(ApiKey::new("sk-1")), http.clone()) } else { client(http.clone()) };
    block_on(c.chat(req)).unwrap();
    http.body()
}

/// A whole reply, read.
pub(crate) fn whole(reply: &str) -> Result<ChatResponse, ProviderError> {
    block_on(client(Canned::new(200, reply)).chat(&request()))
}

/// Streams `body` back in pieces of `piece` bytes.
struct Pieces {
    body: Vec<u8>,
    piece: usize,
    status: u16,
    headers: Vec<(String, String)>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Http for Pieces {
    fn send(&self, _request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::Server, "only streamed requests are expected")) })
    }
    fn send_streaming<'a>(&'a self, request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        self.seen.lock().unwrap().push(request);
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

/// A stream of `events` (each one SSE `data:`), read; and the text as it arrived.
fn streamed_in(events: &[&str], piece: usize) -> (Result<ChatResponse, ProviderError>, Vec<String>) {
    let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    let http = Arc::new(Pieces { body: body.into_bytes(), piece, status: 200, headers: vec![], seen: Mutex::default() });
    let c = LlmClient::new(Endpoint::openai(ApiKey::new("k")), http);
    let mut pieces = Vec::new();
    let reply = block_on(c.chat_stream(&request(), &mut |t| pieces.push(t.to_owned())));
    (reply, pieces)
}

fn streamed(events: &[&str]) -> (Result<ChatResponse, ProviderError>, Vec<String>) {
    streamed_in(events, 7)
}

#[test]
fn request_shape() {
    let http = Canned::new(200, r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}"#);
    let client = LlmClient::new(Endpoint::openai(ApiKey::new("sk-1")), http.clone());
    block_on(client.chat(&request())).unwrap();
    let req = http.request();
    assert_eq!(req.url, "https://api.openai.com/v1/chat/completions");
    assert!(req.headers.contains(&("Authorization".into(), "Bearer sk-1".into())));
    assert!(req.secret_headers.contains(&"authorization".to_owned()), "the key is a secret header");
    let names: Vec<String> = req.headers.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
    assert!(!names.iter().any(|n| n == "user-agent" || n == "originator" || n.contains("session")), "no header of the client's own identity: {names:?}");
    assert_eq!((req.timeout_ms, req.max_response_bytes), (super::super::CHAT_TIMEOUT_MS, super::super::MAX_REPLY_BYTES));
    let b = http.body();
    assert_eq!(b["model"], "m");
    assert_eq!(b["messages"][0], json!({"role": "system", "content": "be brief"}));
    assert_eq!(b["messages"][1]["content"][1], json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,UE5H"}}));
    assert_eq!(b["max_completion_tokens"], 100, "OpenAI itself takes max_completion_tokens");
    assert!(b.get("max_tokens").is_none());
    assert_eq!(b["temperature"], json!(0.5));
    let text_only = sent(&ChatRequest { messages: vec![Message::user("hi")], max_tokens: None, ..request() }, false);
    assert_eq!(text_only["messages"][1]["content"], "hi", "text-only content is a plain string");
    assert!(text_only.get("max_tokens").is_none(), "no limit unless the job sets one");
    let other = sent(&request(), false);
    assert_eq!(other["max_tokens"], 100);
    assert!(other.get("max_completion_tokens").is_none());
    assert!(b.get("stream").is_none());
    let joined =
        sent(&ChatRequest { messages: vec![Message { role: Role::User, parts: vec![Part::Text("a".into()), Part::Text("b".into())] }], ..request() }, false);
    assert_eq!(joined["messages"][1]["content"], "a\n\nb", "several text parts are one string");
    let decimal = sent(&ChatRequest { temperature: Some(0.7), ..request() }, false);
    assert_eq!(decimal["temperature"].to_string(), "0.7", "the temperature as it was written");
    let local = LlmClient::new(Endpoint::ollama(None), http.clone());
    assert_eq!(local.endpoint().base_url, "http://127.0.0.1:11434/v1");
    let keyless = Canned::new(200, r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}"#);
    block_on(LlmClient::new(Endpoint::ollama(None), keyless.clone()).chat(&request())).unwrap();
    assert!(!keyless.request().headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("authorization")), "no key, no Authorization");
    assert_eq!(keyless.request().url, "http://127.0.0.1:11434/v1/chat/completions");
}

#[test]
fn replies_and_finish_reasons() {
    let r = whole(r#"{"choices":[{"message":{"content":"Hi"},"finish_reason":"length"}],"usage":{"prompt_tokens":5,"completion_tokens":7}}"#).unwrap();
    assert_eq!((r.text.as_str(), r.stop, r.usage.input_tokens, r.usage.output_tokens), ("Hi", StopReason::MaxTokens, Some(5), Some(7)));
    assert_eq!(r.message.role, Role::Assistant);
    assert_eq!(whole(r#"{"choices":[{"message":{"content":null},"finish_reason":"content_filter"}]}"#).unwrap_err().kind, ErrorKind::BlockedContent);
    assert_eq!(whole(r#"{"choices":[]}"#).unwrap_err().kind, ErrorKind::Malformed);
    assert_eq!(whole("<html>").unwrap_err().kind, ErrorKind::Malformed);
    let odd = whole(r#"{"choices":[{"message":{"content":"x"},"finish_reason":"eos_token"}]}"#).unwrap();
    assert_eq!(odd.stop, StopReason::Other("eos_token".into()), "an unknown reason is kept, not refused");
    assert_eq!(whole(r#"{"choices":[{"message":{"content":"x"}}]}"#).unwrap().stop, StopReason::EndTurn);
}

#[test]
fn errors() {
    let e = |s, b: &str| error(s, b.as_bytes()).kind;
    assert_eq!(e(429, r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#), ErrorKind::Budget);
    assert_eq!(e(429, r#"{"error":{"message":"slow down","type":"requests","code":"rate_limit_exceeded"}}"#), ErrorKind::RateLimited);
    assert_eq!(e(400, r#"{"error":{"message":"too long","code":"context_length_exceeded"}}"#), ErrorKind::UnsupportedInput);
    let long = error(400, br#"{"error":{"message":"This model's maximum context length is 8192 tokens.","code":"context_length_exceeded"}}"#);
    assert!(long.is_context_limit() && long.message.contains("8192 tokens"), "named as a context limit, keeping the provider's words: {long}");
    assert!(!error(400, br#"{"error":{"message":"bad","code":null}}"#).is_context_limit());
    assert_eq!(e(401, r#"{"error":{"message":"bad key","code":"invalid_api_key"}}"#), ErrorKind::Auth);
    assert_eq!(e(404, r#"{"error":{"message":"no such model","code":"model_not_found"}}"#), ErrorKind::NotFound);
    assert_eq!(e(503, "Service Unavailable"), ErrorKind::Busy);
    assert_eq!(
        e(401, r#"{"error":{"message":"You didn't provide an API key.","type":"invalid_request_error","code":null}}"#),
        ErrorKind::Auth,
        "status wins over type"
    );
    assert_eq!(e(400, r#"{"error":{"message":"bad","type":"invalid_request_error","code":null}}"#), ErrorKind::UnsupportedInput);
    let long = format!(r#"{{"error":{{"message":"{}"}}}}"#, "x".repeat(100_000));
    assert!(error(500, long.as_bytes()).message.len() <= 300, "messages are capped");
    assert_eq!(e(500, r#"{"error":"boom"}"#), ErrorKind::Server);
}

/// Error replies come back through the client with the same kinds, and their Retry-After.
#[test]
fn error_replies_through_the_client() {
    let limited = Canned::with_headers(429, r#"{"error":{"message":"slow down","code":"rate_limit_exceeded"}}"#, vec![("retry-after".into(), "7".into())]);
    let e = block_on(client(limited).chat(&request())).unwrap_err();
    assert_eq!((e.kind, e.retry_after_ms, e.message.as_str()), (ErrorKind::RateLimited, Some(7000), "slow down"));
    let e = block_on(client(Canned::new(401, r#"{"error":{"message":"bad key","code":"invalid_api_key"}}"#)).chat(&request())).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth);
    let e = block_on(client(Canned::new(400, r#"{"error":{"message":"8192 tokens","code":"context_length_exceeded"}}"#)).chat(&request())).unwrap_err();
    assert!(e.is_context_limit());
    // Streamed, the error reply's body is read whole and mapped the same way.
    let http = Arc::new(Pieces {
        body: br#"{"error":{"message":"no","code":"insufficient_quota"}}"#.to_vec(),
        piece: 3,
        status: 429,
        headers: vec![],
        seen: Mutex::default(),
    });
    let c = LlmClient::new(Endpoint::openai(ApiKey::new("k")), http);
    assert_eq!(block_on(c.chat_stream(&request(), &mut |_| {})).unwrap_err().kind, ErrorKind::Budget);
    // A transport failure is the transport's own error.
    struct Down;
    impl Http for Down {
        fn send(&self, _request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            Box::pin(async { Err(ProviderError::new(ErrorKind::Timeout, "fixture timeout")) })
        }
    }
    let down = LlmClient::new(Endpoint::openai(ApiKey::new("k")), Arc::new(Down));
    let e = block_on(down.chat(&request())).unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Timeout, "fixture timeout"));
    assert_eq!(block_on(down.chat_stream(&request(), &mut |_| {})).unwrap_err().kind, ErrorKind::Timeout);
}

#[test]
fn streaming_events() {
    let (r, pieces) = streamed(&[
        r#"{"choices":[{"delta":{"role":"assistant","content":""}}]}"#,
        r#"{"choices":[{"delta":{"content":"Hel"}}]}"#,
        r#"{"choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#,
        "[DONE]",
    ]);
    let r = r.unwrap();
    assert_eq!(pieces.concat(), "Hello");
    assert_eq!((r.text.as_str(), r.stop, r.usage.input_tokens, r.usage.output_tokens), ("Hello", StopReason::EndTurn, Some(3), Some(2)));
    let (e, _) = streamed(&[r#"{"choices":[{"delta":{"content":"a"}}]}"#, r#"{"error":{"message":"overloaded","code":"rate_limit_exceeded"}}"#]);
    assert_eq!(e.unwrap_err().kind, ErrorKind::RateLimited, "an error event keeps its kind");
    assert!(streamed(&["{"]).0.is_err());
    // Some servers end the stream without `[DONE]`: a finish reason means the reply is whole.
    let (r, _) = streamed(&[r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"stop"}]}"#]);
    assert_eq!(r.unwrap().text, "x");
    // And `[DONE]` without a finish reason ends it too.
    let (r, _) = streamed(&[r#"{"choices":[{"delta":{"content":"y"}}]}"#, "[DONE]"]);
    assert_eq!(r.unwrap().stop, StopReason::EndTurn);
    // Neither: cut off.
    let (e, pieces) = streamed(&[r#"{"choices":[{"delta":{"content":"cut"}}]}"#]);
    let e = e.unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Network, "the reply stream ended early"));
    assert_eq!(pieces, ["cut"], "what streamed before the cut was reported");
    let (e, _) = streamed(&[r#"{"choices":[{"delta":{"content":"no"},"finish_reason":"content_filter"}]}"#, "[DONE]"]);
    assert_eq!(e.unwrap_err().kind, ErrorKind::BlockedContent);
}

/// Pieces split anywhere, even inside a line or a multi-byte character, read the same.
#[test]
fn streams_split_anywhere() {
    let events = [r#"{"choices":[{"delta":{"content":"Grüße, "}}]}"#, r#"{"choices":[{"delta":{"content":"Oslo ☔"},"finish_reason":"stop"}]}"#, "[DONE]"];
    for piece in [1, 2, 3, 5, 64, 4096] {
        let (r, pieces) = streamed_in(&events, piece);
        assert_eq!(r.unwrap().text, "Grüße, Oslo ☔", "{piece}");
        assert_eq!(pieces.concat(), "Grüße, Oslo ☔", "{piece}");
    }
}

#[test]
fn streamed_requests() {
    let http = Arc::new(Pieces { body: b"data: [DONE]\n\n".to_vec(), piece: 64, status: 200, headers: vec![], seen: Mutex::default() });
    let c = LlmClient::new(Endpoint::openai(ApiKey::new("k")), http.clone());
    let _ = block_on(c.chat_stream(&request(), &mut |_| {}));
    let req = http.seen.lock().unwrap()[0].clone();
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!((body["stream"].clone(), body["stream_options"].clone()), (json!(true), json!({"include_usage": true})));
    assert_eq!(req.timeout_ms, super::super::STREAM_TIMEOUT_MS);
    assert_eq!(req.max_response_bytes, super::super::MAX_REPLY_BYTES, "100 tokens: the floor");
    assert!(req.headers.contains(&("accept".into(), "text/event-stream".into())), "{:?}", req.headers);
}

/// The reply's spend header is read once the stream ends.
#[test]
fn streamed_spend_is_reported() {
    let body = b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec();
    let headers = vec![("x-litellm-response-cost".to_owned(), "0.0012".to_owned())];
    let http = Arc::new(Pieces { body, piece: 9, status: 200, headers, seen: Mutex::default() });
    let c = LlmClient::new(Endpoint::litellm("http://proxy:4000", None), http);
    let r = block_on(c.chat_stream(&request(), &mut |_| {})).unwrap();
    assert_eq!(r.usage.cost, Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 1200)));
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
    let b = sent(&tool_turns(), true);
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
    let b = sent(&r, false);
    assert_eq!(b["messages"][1]["content"], Value::Null, "a turn of only calls has null content");
    assert!(b["messages"][1].as_object().unwrap().contains_key("content"));
    assert_eq!(b["messages"][1]["tool_calls"][0]["function"]["arguments"], "{city: Oslo", "unparsed arguments go back as they came");
    assert_eq!(b["messages"][2]["content"], "Error: unknown city");
    assert_eq!(b["messages"].as_array().unwrap().len(), 3, "no empty user message after the results");
    assert!(b["tools"][0]["function"].get("strict").is_none(), "strict only when asked");
    assert_eq!(b["tools"][0]["function"]["parameters"], weather().input_schema, "schemas go as written");
}

#[test]
fn tool_replies() {
    let r = whole(
        r#"{"choices":[{"message":{"content":null,"tool_calls":[
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
    assert!(whole(r#"{"choices":[{"message":{"tool_calls":[{"id":"x","function":{}}]}}]}"#).is_err(), "a call without a name");
}

#[test]
fn streamed_tool_calls() {
    let (r, pieces) = streamed(&[
        r#"{"choices":[{"delta":{"role":"assistant","content":"Checking"}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"get_time","arguments":"{}"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"ci"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"get_weather","arguments":"ty\":\"Oslo\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "[DONE]",
    ]);
    let r = r.unwrap();
    assert_eq!(pieces.concat(), "Checking");
    assert_eq!(
        r.message.parts,
        vec![
            Part::Text("Checking".into()),
            Part::ToolUse { id: "call_a".into(), name: "get_weather".into(), input: json!({"city": "Oslo"}) },
            Part::ToolUse { id: "call_b".into(), name: "get_time".into(), input: json!({}) },
        ]
    );
    assert_eq!(r.stop, StopReason::ToolUse);
    // Calls without an index, one per chunk, stay separate.
    let (r, _) = streamed(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"id":"a","function":{"name":"f","arguments":"{\"x\":"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"1}"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"id":"b","function":{"name":"g","arguments":"{}"}}]}}]}"#,
        "[DONE]",
    ]);
    assert_eq!(
        r.unwrap().message.parts,
        vec![Part::ToolUse { id: "a".into(), name: "f".into(), input: json!({"x": 1}) }, Part::ToolUse { id: "b".into(), name: "g".into(), input: json!({}) }]
    );
    // An unnamed call is malformed.
    let (e, _) = streamed(&[r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"arguments":"{}"}}]}}]}"#, "[DONE]"]);
    assert!(e.is_err(), "{e:?}");
}

/// A server can't make endless calls.
#[test]
fn replies_are_bounded() {
    let flood: Vec<Value> =
        (0..=MAX_REPLY_BLOCKS as u64).map(|i| json!({"index": i, "id": format!("x{i}"), "function": {"name": "f", "arguments": "{}"}})).collect();
    let flood = json!({"choices": [{"delta": {"tool_calls": flood}, "finish_reason": "tool_calls"}]}).to_string();
    let (e, _) = streamed(&[&flood, "[DONE]"]);
    assert_eq!(e.unwrap_err().kind, ErrorKind::Malformed);
}

#[test]
fn pdfs_are_sent_as_documents() {
    let pdf = |n: usize| Part::Document { mime: "application/pdf".into(), bytes: Arc::from(vec![b'%'; n]) };
    let mut r = request();
    r.messages[0].parts = vec![Part::Text("read this".into()), pdf(3), pdf(3)];
    let gpt = sent(&r, true);
    assert_eq!(
        gpt["messages"][1]["content"][1],
        json!({"type": "file", "file": {"filename": "document-1.pdf", "file_data": "data:application/pdf;base64,JSUl"}})
    );
    assert_eq!(gpt["messages"][1]["content"][2]["file"]["filename"], "document-2.pdf", "several PDFs get distinct names");
    assert_eq!(gpt["messages"][1]["content"][0], json!({"type": "text", "text": "read this"}));
}

/// The request is checked before it is sent: nothing goes out for a request either dialect
/// would refuse.
#[test]
fn refused_requests_send_nothing() {
    let http = Canned::new(200, "{}");
    let c = client(http.clone());
    let mut r = request();
    r.model = " ".into();
    assert_eq!(block_on(c.chat(&r)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert!(http.seen.lock().unwrap().is_empty());
    assert!(r.validate(Dialect::OpenAi).is_err());
}

/// The agent loop on this dialect: ask, run the tool, send the reply back as it came plus the
/// result.
#[test]
fn a_tool_loop_round_trips() {
    let first = r#"{"choices":[{"message":{"content":"Let me look.","tool_calls":[{"id":"call_9","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Oslo\"}"}}]},"finish_reason":"tool_calls"}]}"#;
    let second = r#"{"choices":[{"message":{"content":"4 °C and rain."},"finish_reason":"stop"}],"usage":{"prompt_tokens":60,"completion_tokens":10}}"#;
    let http = Arc::new(Canned {
        replies: Mutex::new(vec![
            HttpResponse { status: 200, headers: vec![], body: second.as_bytes().to_vec() },
            HttpResponse { status: 200, headers: vec![], body: first.as_bytes().to_vec() },
        ]),
        seen: Mutex::new(vec![]),
    });
    let c = client(http.clone());
    let mut req = ChatRequest { messages: vec![Message::user("Weather in Oslo?")], ..tool_turns() };
    let reply = block_on(c.chat(&req)).unwrap();
    assert_eq!(reply.stop, StopReason::ToolUse);
    let results: Vec<Part> = reply.tool_calls().map(|c| Part::ToolResult { id: c.id.into(), content: "4 °C, rain".into(), is_error: false }).collect();
    req.messages.push(reply.message.clone());
    req.messages.push(Message { role: Role::User, parts: results });
    let done = block_on(c.chat(&req)).unwrap();
    assert_eq!(done.text, "4 °C and rain.");
    let sent: Value = serde_json::from_slice(&http.seen.lock().unwrap()[1].body).unwrap();
    assert_eq!(
        sent["messages"][1],
        json!({"role": "assistant", "content": "Let me look.", "tool_calls": [{"id": "call_9", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Oslo\"}"}}]})
    );
    assert_eq!(sent["messages"][2], json!({"role": "tool", "tool_call_id": "call_9", "content": "4 °C, rain"}));
}

/// Hands the body over a piece at a time, letting the reader run between pieces and after the
/// last one before the send ends, as a real connection does: the client stops reading at the
/// last event, before the transport has finished.
struct Slow {
    body: Vec<u8>,
    headers: Vec<(String, String)>,
    finished: std::sync::atomic::AtomicBool,
}

/// Pending once, so whoever polls gets to run.
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
            // The connection closes a moment after the last event.
            yield_once().await;
            yield_once().await;
            self.finished.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(HttpResponse { status: 200, headers: self.headers.clone(), body: Vec::new() })
        })
    }
}

#[test]
fn a_stream_read_to_its_last_event_still_ends_its_send() {
    let slow = || {
        let body = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec();
        Arc::new(Slow { body, headers: vec![("x-litellm-response-cost".into(), "0.0012".into())], finished: Default::default() })
    };
    // The spend the head reports is read once the send ends.
    let http = slow();
    let c = LlmClient::new(Endpoint::litellm("http://proxy:4000", None), http.clone());
    let mut pieces = Vec::new();
    let r = block_on(c.chat_stream(&request(), &mut |t| pieces.push(t.to_owned()))).unwrap();
    assert_eq!((r.text.as_str(), pieces.len()), ("Hello", 2));
    assert_eq!(r.usage.cost, Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 1200)));
    assert!(http.finished.load(std::sync::atomic::Ordering::SeqCst));
    // Through a runner's scope, the request is one of its sends, finished by the transport.
    let http = slow();
    let c = LlmClient::new(Endpoint::litellm("http://proxy:4000", None), http.clone());
    let context = crate::SubmitContext::new();
    context.arm();
    let r = block_on(c.streamed(&request(), &mut |_| {}, Some(&context))).unwrap();
    assert_eq!(r.usage.cost, Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 1200)));
    assert!(http.finished.load(std::sync::atomic::Ordering::SeqCst), "the send ran to its end");
    assert!(context.finish_error().is_none(), "a sent request may be billed");
}

/// Arguments that are JSON but not an object go back as they came, not as `{}`.
#[test]
fn non_object_arguments_go_back_as_they_came() {
    for (input, sent_as) in [(json!([1, 2]), "[1,2]"), (json!(3), "3"), (json!(true), "true"), (Value::Null, "null"), (json!("{x"), "{x")] {
        let mut r = tool_turns();
        r.messages[1].parts = vec![Part::ToolUse { id: "call_1".into(), name: "get_weather".into(), input }];
        r.messages[2].parts = vec![Part::ToolResult { id: "call_1".into(), content: "ok".into(), is_error: false }];
        assert_eq!(sent(&r, false)["messages"][1]["tool_calls"][0]["function"]["arguments"], sent_as);
    }
}

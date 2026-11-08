//! Converse contract tests against synthetic, public-safe fixtures (#1150): never AWS.

use super::event_stream::encode;
use super::*;
use crate::http::{ChunkSink, Http, HttpResponse};
use crate::llm::tests::Canned;
use crate::llm::{ApiKey, Endpoint, Message, Tool};
use crate::provider::BoxFuture;
use futures::executor::block_on;
use std::sync::{Arc, Mutex};

const KEY: &str = "bedrock-public-fixture-key";

fn client(http: Arc<dyn Http>) -> LlmClient {
    LlmClient::new(Endpoint::bedrock_converse("us-west-2", ApiKey::new(KEY)).unwrap(), http)
}

fn request() -> ChatRequest {
    ChatRequest {
        model: "us.anthropic.claude-sonnet-4-6".into(),
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

/// Sends a streamed reply in pieces of `piece` bytes, recording requests.
struct Chunked {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    piece: usize,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Chunked {
    fn new(body: Vec<u8>, piece: usize) -> Arc<Self> {
        Arc::new(Self { status: 200, headers: vec![], body, piece, seen: Mutex::new(vec![]) })
    }
}

impl Http for Chunked {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.seen.lock().unwrap().push(request);
        let r = HttpResponse { status: self.status, headers: self.headers.clone(), body: self.body.clone() };
        Box::pin(async move { Ok(r) })
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
            Ok(HttpResponse { status: self.status, headers: self.headers.clone(), body: vec![] })
        })
    }
}

fn event(kind: &str, payload: Value) -> Vec<u8> {
    encode(&[(":event-type", kind), (":content-type", "application/json"), (":message-type", "event")], payload.to_string().as_bytes())
}

fn exception(kind: &str, message: &str) -> Vec<u8> {
    encode(
        &[(":exception-type", kind), (":content-type", "application/json"), (":message-type", "exception")],
        json!({ "message": message }).to_string().as_bytes(),
    )
}

/// A streamed reply: reasoning with a signature, text, a tool call whose input arrives in
/// pieces, then the stop and the counts. Bedrock pads events with a `p` field; it is ignored.
fn tool_stream() -> Vec<u8> {
    [
        event("messageStart", json!({"role":"assistant","p":"abc"})),
        event("contentBlockDelta", json!({"contentBlockIndex":0,"delta":{"reasoningContent":{"text":"Think"}}})),
        event("contentBlockDelta", json!({"contentBlockIndex":0,"delta":{"reasoningContent":{"text":"ing."}}})),
        event("contentBlockDelta", json!({"contentBlockIndex":0,"delta":{"reasoningContent":{"signature":"sig-1"}}})),
        event("contentBlockStop", json!({"contentBlockIndex":0})),
        event("contentBlockDelta", json!({"contentBlockIndex":1,"delta":{"text":"Check"},"p":"abcdefgh"})),
        event("contentBlockDelta", json!({"contentBlockIndex":1,"delta":{"text":"ing."}})),
        event("contentBlockStop", json!({"contentBlockIndex":1})),
        event("contentBlockStart", json!({"contentBlockIndex":2,"start":{"toolUse":{"toolUseId":"tooluse_1","name":"weather"}}})),
        event("contentBlockDelta", json!({"contentBlockIndex":2,"delta":{"toolUse":{"input":"{\"city\":"}}})),
        event("contentBlockDelta", json!({"contentBlockIndex":2,"delta":{"toolUse":{"input":"\"Oslo\"}"}}})),
        event("contentBlockStop", json!({"contentBlockIndex":2})),
        event("messageStop", json!({"stopReason":"tool_use"})),
        event("metadata", json!({"usage":{"inputTokens":21,"outputTokens":9,"totalTokens":30},"metrics":{"latencyMs":5}})),
    ]
    .concat()
}

#[test]
fn converse_whole_requests_go_to_the_regional_runtime_with_a_secret_bearer_key() {
    let reply = json!({
        "output": {"message": {"role": "assistant", "content": [
            {"reasoningContent": {"reasoningText": {"text": "Hmm.", "signature": "sig"}}},
            {"text": "Let me check."},
            {"toolUse": {"toolUseId": "tooluse_1", "name": "weather", "input": {"city": "Oslo"}}}
        ]}},
        "stopReason": "tool_use",
        "usage": {"inputTokens": 30, "outputTokens": 12, "totalTokens": 42},
        "metrics": {"latencyMs": 100}
    });
    let http = Canned::new(200, &reply.to_string());
    let mut req = request();
    req.tools = vec![weather(), Tool { description: String::new(), strict: true, ..weather() }];
    req.tools[1].name = "other".into();
    req.messages[0].parts.push(Part::Image { mime: "image/jpeg".into(), bytes: Arc::from(&b"JPG"[..]) });
    req.messages[0].parts.push(Part::Document { mime: "application/pdf".into(), bytes: Arc::from(&b"%PDF"[..]) });
    let out = block_on(client(http.clone()).chat(&req)).unwrap();
    assert_eq!(out.text, "Let me check.");
    assert_eq!(out.stop, StopReason::ToolUse);
    assert_eq!((out.usage.input_tokens, out.usage.output_tokens, out.usage.cost), (Some(30), Some(12), None), "tokens only: money is the catalog's");
    let calls: Vec<_> = out.tool_calls().collect();
    assert_eq!((calls[0].id, calls[0].name, calls[0].input), ("tooluse_1", "weather", &json!({"city":"Oslo"})));
    assert!(
        matches!(&out.message.parts[0], Part::Opaque { dialect: Dialect::Converse, block } if block["reasoningContent"]["reasoningText"]["signature"] == "sig")
    );

    let sent = http.request();
    assert_eq!(sent.url, "https://bedrock-runtime.us-west-2.amazonaws.com/model/us.anthropic.claude-sonnet-4-6/converse");
    assert!(sent.headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Bearer {KEY}")));
    assert!(sent.secret_headers.iter().any(|n| n == "authorization"), "the key never follows a redirect");
    assert!(!sent.headers.iter().any(|(n, _)| n.to_ascii_lowercase().starts_with("x-amz")), "no SigV4");
    assert!(!format!("{sent:?} {:?}", client(http.clone())).contains(KEY), "the key is never printed");
    assert!(sent.max_response_bytes <= 8 * 1024 * 1024);
    let body = http.body();
    assert_eq!(body["system"], json!([{"text":"be brief"}]));
    assert_eq!(body["inferenceConfig"], json!({"maxTokens":100,"temperature":0.5}));
    assert_eq!(body["messages"][0]["role"], "user");
    let content = &body["messages"][0]["content"];
    assert_eq!(content[0], json!({"text":"hi"}));
    assert_eq!(content[1], json!({"image":{"format":"jpeg","source":{"bytes":"SlBH"}}}));
    assert_eq!(content[2], json!({"document":{"format":"pdf","name":"Document 1","source":{"bytes":"JVBERg=="}}}));
    let tools = &body["toolConfig"]["tools"];
    assert_eq!(tools[0], json!({"toolSpec":{"name":"weather","description":"Looks up the weather","inputSchema":{"json":weather().input_schema}}}));
    assert!(tools[1]["toolSpec"].get("description").is_none(), "Converse refuses an empty description");
    assert_eq!(tools[1]["toolSpec"]["strict"], true);
    assert!(body.get("modelId").is_none() && body.get("model").is_none(), "the model goes in the path");
}

#[test]
fn model_ids_and_arns_are_one_encoded_path_segment() {
    let http = Canned::new(200, r#"{"output":{"message":{"content":[{"text":"ok"}]}},"stopReason":"end_turn"}"#);
    let mut req = request();
    req.model = "arn:aws:bedrock:us-west-2:123456789012:inference-profile/us.anthropic.claude-sonnet-4-6".into();
    block_on(client(http.clone()).chat(&req)).unwrap();
    assert_eq!(
        http.request().url,
        "https://bedrock-runtime.us-west-2.amazonaws.com/model/arn%3Aaws%3Abedrock%3Aus-west-2%3A123456789012%3Ainference-profile%2Fus.anthropic.claude-sonnet-4-6/converse"
    );
    let mut req = request();
    req.model = "anthropic.claude-3-haiku-20240307-v1:0".into();
    req.max_tokens = None;
    req.temperature = None;
    req.system = None;
    let http = Canned::new(200, r#"{"output":{"message":{"content":[{"text":"ok"}]}},"stopReason":"end_turn"}"#);
    block_on(client(http.clone()).chat(&req)).unwrap();
    assert!(http.request().url.ends_with("/model/anthropic.claude-3-haiku-20240307-v1%3A0/converse"));
    let body = http.body();
    assert!(body.get("inferenceConfig").is_none() && body.get("system").is_none() && body.get("toolConfig").is_none(), "{body}");
}

#[test]
fn converse_streams_text_tool_calls_and_reasoning_in_pieces_of_any_size() {
    let stream = tool_stream();
    for piece in [1, 3, 12, 13, 50, 4096] {
        let http = Chunked::new(stream.clone(), piece);
        let mut req = request();
        req.tools = vec![weather()];
        let mut pieces = Vec::new();
        let out = block_on(client(http.clone()).chat_stream(&req, &mut |p| pieces.push(p.to_owned()))).unwrap();
        assert_eq!(pieces, ["Check", "ing."], "pieces of {piece}: reasoning is not streamed as text");
        assert_eq!(out.text, "Checking.");
        assert_eq!(out.stop, StopReason::ToolUse);
        assert_eq!((out.usage.input_tokens, out.usage.output_tokens), (Some(21), Some(9)));
        let calls: Vec<_> = out.tool_calls().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!((calls[0].id, calls[0].name, calls[0].input), ("tooluse_1", "weather", &json!({"city":"Oslo"})));
        assert_eq!(
            out.message.parts[0],
            Part::Opaque { dialect: Dialect::Converse, block: json!({"reasoningContent":{"reasoningText":{"text":"Thinking.","signature":"sig-1"}}}) }
        );
        let sent = http.seen.lock().unwrap()[0].clone();
        assert!(sent.url.ends_with("/model/us.anthropic.claude-sonnet-4-6/converse-stream"));
        assert!(sent.headers.iter().any(|(n, v)| n == "Accept" && v == "application/vnd.amazon.eventstream"));
        assert!(sent.max_response_bytes >= 8 * 1024 * 1024 && sent.max_response_bytes <= 128 * 1024 * 1024);
    }
}

#[test]
fn a_reply_goes_back_in_the_next_turn_as_it_came() {
    let http = Chunked::new(tool_stream(), 64);
    let mut req = request();
    req.tools = vec![weather()];
    let out = block_on(client(http).chat_stream(&req, &mut |_| {})).unwrap();
    req.messages.push(out.message.clone());
    req.messages.push(Message {
        role: Role::User,
        parts: vec![Part::Text("thanks".into()), Part::ToolResult { id: "tooluse_1".into(), content: "rain".into(), is_error: true }],
    });
    let http = Canned::new(200, r#"{"output":{"message":{"content":[{"text":"Rain."}]}},"stopReason":"end_turn"}"#);
    block_on(client(http.clone()).chat(&req)).unwrap();
    let body = http.body();
    let assistant = &body["messages"][1];
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["content"][0], json!({"reasoningContent":{"reasoningText":{"text":"Thinking.","signature":"sig-1"}}}));
    assert_eq!(assistant["content"][1], json!({"text":"Checking."}));
    assert_eq!(assistant["content"][2], json!({"toolUse":{"toolUseId":"tooluse_1","name":"weather","input":{"city":"Oslo"}}}));
    // Tool results come first in their message.
    assert_eq!(body["messages"][2]["content"][0], json!({"toolResult":{"toolUseId":"tooluse_1","content":[{"text":"rain"}],"status":"error"}}));
    assert_eq!(body["messages"][2]["content"][1], json!({"text":"thanks"}));
}

#[test]
fn a_failed_tool_result_has_a_status_only_where_aws_documents_one() {
    let turn = |model: &str| {
        let mut req = request();
        req.model = model.into();
        req.tools = vec![weather()];
        req.messages.push(Message { role: Role::Assistant, parts: vec![Part::ToolUse { id: "t1".into(), name: "weather".into(), input: json!({}) }] });
        req.messages.push(Message {
            role: Role::User,
            parts: vec![
                Part::ToolResult { id: "t1".into(), content: "no such city".into(), is_error: true },
                Part::ToolResult { id: "t2".into(), content: "sunny".into(), is_error: false },
            ],
        });
        req.messages[1].parts.push(Part::ToolUse { id: "t2".into(), name: "weather".into(), input: json!({}) });
        body(&req)["messages"][2]["content"].clone()
    };
    for model in
        ["us.anthropic.claude-sonnet-4-6", "amazon.nova-pro-v1:0", "arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-3-haiku-20240307-v1:0"]
    {
        let content = turn(model);
        assert_eq!(content[0]["toolResult"], json!({"toolUseId":"t1","content":[{"text":"no such city"}],"status":"error"}), "{model}");
        assert_eq!(content[1]["toolResult"], json!({"toolUseId":"t2","content":[{"text":"sunny"}]}), "{model}");
    }
    for model in ["meta.llama3-1-70b-instruct-v1:0", "mistral.mistral-large-2407-v1:0", "openai.gpt-oss-120b-1:0"] {
        let content = turn(model);
        assert_eq!(content[0]["toolResult"], json!({"toolUseId":"t1","content":[{"text":"Error: no such city"}]}), "{model}");
        assert_eq!(content[1]["toolResult"], json!({"toolUseId":"t2","content":[{"text":"sunny"}]}), "{model}");
    }
}

#[test]
fn streamed_tool_input_that_isnt_an_object_stays_text() {
    let stream = [
        event("contentBlockStart", json!({"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"t1","name":"weather"}}})),
        event("contentBlockDelta", json!({"contentBlockIndex":0,"delta":{"toolUse":{"input":"{not json"}}})),
        event("contentBlockStart", json!({"contentBlockIndex":1,"start":{"toolUse":{"toolUseId":"t2","name":"weather"}}})),
        event("messageStop", json!({"stopReason":"tool_use"})),
    ]
    .concat();
    let out = block_on(client(Chunked::new(stream, 7)).chat_stream(&request(), &mut |_| {})).unwrap();
    let calls: Vec<_> = out.tool_calls().collect();
    assert_eq!(calls[0].input, &json!("{not json"));
    assert_eq!(calls[1].input, &json!({}), "a call with no input takes no arguments");
}

#[test]
fn converse_stop_reasons_are_mapped() {
    let reply = |reason: &str| json!({"output":{"message":{"content":[{"text":"partial"}]}},"stopReason":reason});
    for (reason, expected) in [
        ("end_turn", Ok(StopReason::EndTurn)),
        ("stop_sequence", Ok(StopReason::EndTurn)),
        ("max_tokens", Ok(StopReason::MaxTokens)),
        ("tool_use", Ok(StopReason::ToolUse)),
        ("model_context_window_exceeded", Ok(StopReason::MaxTokens)),
        ("something_new", Ok(StopReason::Other("something_new".into()))),
        ("guardrail_intervened", Err(ErrorKind::BlockedContent)),
        ("content_filtered", Err(ErrorKind::BlockedContent)),
        ("malformed_model_output", Err(ErrorKind::Malformed)),
        ("malformed_tool_use", Err(ErrorKind::Malformed)),
    ] {
        let got = super::reply(&reply(reason)).map(|r| r.stop).map_err(|e| e.kind);
        assert_eq!(got, expected, "{reason}");
        // The same through a stream.
        let stream =
            [event("contentBlockDelta", json!({"contentBlockIndex":0,"delta":{"text":"partial"}})), event("messageStop", json!({"stopReason":reason}))]
                .concat();
        let got = block_on(client(Chunked::new(stream, 5)).chat_stream(&request(), &mut |_| {})).map(|r| r.stop).map_err(|e| e.kind);
        assert_eq!(got, expected, "streamed {reason}");
    }
    assert_eq!(super::reply(&json!({"output":{"message":{"content":[]}}})).unwrap_err().kind, ErrorKind::Malformed, "no stop reason");
}

#[test]
fn converse_error_statuses_map_to_actionable_kinds() {
    let typed = |name: &str| {
        vec![
            ("x-amzn-ErrorType".to_owned(), format!("{name}:http://internal.amazon.com/coral/com.amazon.bedrock/")),
            ("Retry-After".to_owned(), "3".to_owned()),
        ]
    };
    for (status, name, message, kind) in [
        (429, "ThrottlingException", "Too many requests, please wait before trying again.", ErrorKind::RateLimited),
        (429, "ModelNotReadyException", "not ready", ErrorKind::Busy),
        (403, "AccessDeniedException", "You don't have access to the model with the specified model ID.", ErrorKind::Auth),
        (403, "UnrecognizedClientException", "The security token included in the request is invalid.", ErrorKind::Auth),
        (400, "ValidationException", "The model returned the following errors: bad", ErrorKind::UnsupportedInput),
        (404, "ResourceNotFoundException", "Model not found", ErrorKind::NotFound),
        (408, "ModelTimeoutException", "too slow", ErrorKind::Timeout),
        (424, "ModelErrorException", "model failed", ErrorKind::Server),
        (500, "InternalServerException", "oops", ErrorKind::Server),
        (503, "ServiceUnavailableException", "later", ErrorKind::Busy),
        (418, "SomethingNewException", "new", ErrorKind::Server),
    ] {
        let body = json!({ "message": message }).to_string();
        let e = super::error(status, &typed(name), body.as_bytes());
        assert_eq!(e.kind, kind, "{name}");
        assert_eq!(e.message, message);
        assert_eq!(e.retry_after_ms, Some(3000), "{name}");
        // Through the client: one request, never retried, the key never echoed.
        let http = Chunked { status, headers: typed(name), body: body.clone().into_bytes(), piece: 8, seen: Mutex::new(vec![]) };
        let http = Arc::new(http);
        for streamed in [false, true] {
            let e =
                if streamed { block_on(client(http.clone()).chat_stream(&request(), &mut |_| {})) } else { block_on(client(http.clone()).chat(&request())) };
            let e = e.unwrap_err();
            assert_eq!(e.kind, kind, "{name} streamed={streamed}");
            assert!(!e.message.contains(KEY));
        }
        assert_eq!(http.seen.lock().unwrap().len(), 2, "one request per call: a possibly paid request is never resent");
    }
    // A used-up quota isn't a burst: not transient, and says what to do.
    let e = super::error(400, &typed("ServiceQuotaExceededException"), br#"{"message":"quota"}"#);
    assert_eq!(e.kind, ErrorKind::Budget);
    assert!(!e.is_transient());
    assert!(e.message.contains("raise it in Service Quotas") && e.message.contains("quota"), "{}", e.message);
    let stream = exception("serviceQuotaExceededException", "quota");
    let e = block_on(client(Chunked::new(stream, 9)).chat_stream(&request(), &mut |_| {})).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Budget);
    // The name in the body's `__type`, a namespace in front.
    let e = super::error(400, &[], br#"{"__type":"com.amazon.coral.validate#ValidationException","message":"Input is too long for requested model."}"#);
    assert!(e.is_context_limit(), "{e:?}");
    // No name: the status decides, with the server's message.
    let e = super::error(401, &[], br#"{"Message":"Invalid API Key format"}"#);
    assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Auth, "Invalid API Key format"));
    assert_eq!(super::error(502, &[], b"<html>").kind, ErrorKind::Server);
}

#[test]
fn exceptions_inside_a_stream_map_to_kinds() {
    for (name, kind) in [
        ("throttlingException", ErrorKind::RateLimited),
        ("validationException", ErrorKind::UnsupportedInput),
        ("serviceUnavailableException", ErrorKind::Busy),
        ("modelStreamErrorException", ErrorKind::Server),
        ("internalServerException", ErrorKind::Server),
    ] {
        let stream = [event("contentBlockDelta", json!({"contentBlockIndex":0,"delta":{"text":"Hel"}})), exception(name, "stream failed")].concat();
        let mut pieces = String::new();
        let e = block_on(client(Chunked::new(stream, 9)).chat_stream(&request(), &mut |p| pieces.push_str(p))).unwrap_err();
        assert_eq!(e.kind, kind, "{name}");
        assert_eq!(e.message, "stream failed");
        assert_eq!(pieces, "Hel", "what arrived before is still shown");
    }
    let error = encode(&[(":message-type", "error"), (":error-code", "InternalFailure"), (":error-message", "boom")], b"");
    let e = block_on(client(Chunked::new(error, 4)).chat_stream(&request(), &mut |_| {})).unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Server, "boom"));
}

#[test]
fn hostile_streams_end_in_named_errors() {
    let good = tool_stream();
    let mut corrupt = good.clone();
    let at = good.len() / 2;
    corrupt[at] ^= 0x55;
    let mut oversized = event("messageStart", json!({}));
    oversized.extend_from_slice(&u32::MAX.to_be_bytes());
    oversized.extend_from_slice(&0u32.to_be_bytes());
    let crc = event_stream::crc32(&oversized[oversized.len() - 8..]);
    oversized.extend_from_slice(&crc.to_be_bytes());
    let unended = [event("contentBlockDelta", json!({"contentBlockIndex":0,"delta":{"text":"Hel"}}))].concat();
    let cut = good[..good.len() - 3].to_vec();
    let big_index = event("contentBlockDelta", json!({"contentBlockIndex": MAX_REPLY_BLOCKS, "delta":{"text":"x"}}));
    let float_index = event("contentBlockDelta", json!({"contentBlockIndex": 0.5, "delta":{"text":"x"}}));
    let orphan_input = event("contentBlockDelta", json!({"contentBlockIndex":0,"delta":{"toolUse":{"input":"{}"}}}));
    let mixed = [
        event("contentBlockDelta", json!({"contentBlockIndex":0,"delta":{"text":"x"}})),
        event("contentBlockDelta", json!({"contentBlockIndex":0,"delta":{"reasoningContent":{"text":"y"}}})),
    ]
    .concat();
    let twice = [
        event("contentBlockStart", json!({"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"a","name":"weather"}}})),
        event("contentBlockStart", json!({"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"b","name":"weather"}}})),
    ]
    .concat();
    let nameless = event("contentBlockStart", json!({"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"a"}}}));
    let not_json = encode(&[(":event-type", "messageStop"), (":message-type", "event")], b"{");
    let untyped = encode(&[(":event-type", "messageStop")], br#"{"stopReason":"end_turn"}"#);
    for (what, stream, kind) in [
        ("corrupt", corrupt, ErrorKind::Malformed),
        ("oversized", oversized, ErrorKind::Malformed),
        ("garbage", b"data: {\"choices\":[]}\n\n".to_vec(), ErrorKind::Malformed),
        ("unended", unended, ErrorKind::Network),
        ("cut", cut, ErrorKind::Network),
        ("empty", vec![], ErrorKind::Network),
        ("big index", big_index, ErrorKind::Malformed),
        ("float index", float_index, ErrorKind::Malformed),
        ("orphan input", orphan_input, ErrorKind::Malformed),
        ("mixed", mixed, ErrorKind::Malformed),
        ("twice", twice, ErrorKind::Malformed),
        ("nameless", nameless, ErrorKind::Malformed),
        ("not json", not_json, ErrorKind::Malformed),
        ("untyped", untyped, ErrorKind::Malformed),
    ] {
        let e = block_on(client(Chunked::new(stream, 10)).chat_stream(&request(), &mut |_| {})).unwrap_err();
        assert_eq!(e.kind, kind, "{what}: {e:?}");
    }
}

#[test]
fn hostile_whole_replies_are_malformed_not_panics() {
    for body in [
        "<html>".to_owned(),
        "null".to_owned(),
        r#"{"stopReason":"end_turn"}"#.to_owned(),
        r#"{"stopReason":"end_turn","output":{"message":{"content":{}}}}"#.to_owned(),
        r#"{"stopReason":"end_turn","output":{"message":{"content":[1]}}}"#.to_owned(),
        r#"{"stopReason":"end_turn","output":{"message":{"content":[{"text":7}]}}}"#.to_owned(),
        r#"{"stopReason":"tool_use","output":{"message":{"content":[{"toolUse":{"name":"x"}}]}}}"#.to_owned(),
        json!({"stopReason":"end_turn","output":{"message":{"content": vec![json!({"text":"x"}); MAX_REPLY_BLOCKS + 1]}}}).to_string(),
    ] {
        let e = block_on(client(Canned::new(200, &body)).chat(&request())).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Malformed, "{body:.80}");
    }
}

#[test]
fn converse_requests_are_checked_before_anything_is_sent() {
    let refused = |req: ChatRequest| {
        let http = Canned::new(200, "{}");
        let e = block_on(client(http.clone()).chat(&req)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::UnsupportedInput, "{e:?}");
        assert!(http.seen.lock().unwrap().is_empty(), "nothing sent");
        e.message
    };
    let mut r = request();
    r.messages.insert(0, Message::assistant("prefill"));
    assert!(refused(r).contains("start with a user message"));
    let mut r = request();
    r.temperature = Some(1.5);
    assert!(refused(r).contains("between 0 and 1"));
    let mut r = request();
    r.messages[0].parts.push(Part::Image { mime: "image/png".into(), bytes: Arc::from(vec![0u8; CONVERSE_MAX_IMAGE_BYTES + 1]) });
    assert!(refused(r).contains("Bedrock accepts"));
    let mut r = request();
    r.messages[0].parts.push(Part::Document { mime: "application/pdf".into(), bytes: Arc::from(vec![b'%'; CONVERSE_MAX_DOCUMENT_BYTES + 1]) });
    assert!(refused(r).contains("Bedrock accepts"));
    let mut r = request();
    r.messages[0].parts = vec![Part::Document { mime: "application/pdf".into(), bytes: Arc::from(&b"%PDF"[..]) }];
    assert!(refused(r).contains("text next to a PDF"));
    let mut r = request();
    r.messages = vec![
        Message::user("go"),
        Message { role: Role::Assistant, parts: vec![Part::ToolUse { id: "call id with spaces".into(), name: "weather".into(), input: json!({}) }] },
        Message { role: Role::User, parts: vec![Part::ToolResult { id: "call id with spaces".into(), content: "ok".into(), is_error: false }] },
    ];
    r.tools = vec![weather()];
    assert!(refused(r).contains("tool call id"));
    let mut r = request();
    r.model = "x".repeat(MAX_MODEL_ID + 1);
    refused(r);
    // At the limits, it goes.
    let mut r = request();
    r.messages[0].parts.push(Part::Image { mime: "image/png".into(), bytes: Arc::from(vec![0u8; CONVERSE_MAX_IMAGE_BYTES]) });
    assert!(validate(&r).is_ok());
}

#[test]
fn other_dialects_reasoning_is_left_out_and_converse_endpoints_list_no_models() {
    let mut req = request();
    req.messages.push(Message {
        role: Role::Assistant,
        parts: vec![Part::Text("ok".into()), Part::Opaque { dialect: Dialect::Anthropic, block: json!({"type":"thinking","thinking":"x","signature":"s"}) }],
    });
    req.messages.push(Message::user("more"));
    let http = Canned::new(200, r#"{"output":{"message":{"content":[{"text":"ok"}]}},"stopReason":"end_turn"}"#);
    block_on(client(http.clone()).chat(&req)).unwrap();
    assert_eq!(http.body()["messages"][1]["content"], json!([{"text":"ok"}]));
    let e = block_on(client(Canned::new(200, "{}")).list_models()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::UnsupportedInput);
}

#[test]
fn bedrock_converse_shares_the_bedrock_region_rules_and_key() {
    let endpoint = Endpoint::bedrock_converse(" eu-central-1 ", ApiKey::new(KEY)).unwrap();
    assert_eq!((endpoint.id.as_str(), endpoint.dialect), (crate::llm::BEDROCK, Dialect::Converse));
    assert_eq!(endpoint.base_url, "https://bedrock-runtime.eu-central-1.amazonaws.com");
    for region in ["", "cn-north-1", "us-east-1.evil.example", "US-EAST-1"] {
        assert_eq!(Endpoint::bedrock_converse(region, ApiKey::new(KEY)).unwrap_err().kind, ErrorKind::UnsupportedInput, "{region}");
    }
    // The OpenAI-compatible route stays as it was.
    assert_eq!(Endpoint::bedrock("eu-central-1", ApiKey::new(KEY)).unwrap().dialect, Dialect::OpenAi);
}

mod delivery {
    use super::*;
    use crate::{CancelSignal, Clock, JobInput, Output, Provider, ProviderJob, RunLimits, RunUpdate, Submission};

    struct Never;
    impl CancelSignal for Never {
        fn is_cancelled(&self) -> bool {
            false
        }
        fn cancelled(&self) -> BoxFuture<'_, ()> {
            Box::pin(futures::future::pending())
        }
    }
    struct Instant;
    impl Clock for Instant {
        fn now_ms(&self) -> u64 {
            0
        }
        fn sleep(&self, _: u64) -> BoxFuture<'static, ()> {
            Box::pin(async {})
        }
    }

    fn job() -> ProviderJob {
        ProviderJob {
            model: "us.anthropic.claude-sonnet-4-6".into(),
            params: [("max_tokens".into(), json!(64))].into_iter().collect(),
            inputs: vec![JobInput::Text { role: "prompt".into(), text: "A harbour".into() }],
            idempotency_key: "public-fixture-1150".into(),
        }
    }

    #[test]
    fn a_submit_finishes_at_once_and_is_never_resent() {
        let http = Canned::new(
            200,
            r#"{"output":{"message":{"content":[{"text":"A quiet harbour."}]}},"stopReason":"end_turn","usage":{"inputTokens":5,"outputTokens":4}}"#,
        );
        let client = client(http.clone());
        assert_eq!(client.id(), "bedrock");
        assert!(!client.retry_ambiguous_submit(), "Converse offers no idempotency");
        let Submission::Finished(done) = block_on(client.submit(&job())).unwrap() else { panic!("synchronous reply") };
        assert_eq!(done.outputs, vec![Output::Text("A quiet harbour.".into())]);
        assert_eq!((done.usage.input_tokens, done.usage.output_tokens, done.usage.cost), (Some(5), Some(4), None));
        assert_eq!(http.body()["inferenceConfig"]["maxTokens"], 64);
    }

    #[test]
    fn a_run_streams_through_the_delivery_scope() {
        let client = client(Chunked::new(tool_stream(), 33));
        let mut updates = Vec::new();
        let limits = RunLimits { stream_text: true, ..RunLimits::default() };
        let done = block_on(crate::run_to_completion(&client, &job(), &Never, &Instant, limits, |u| updates.push(u))).unwrap();
        assert_eq!(done.outputs, vec![Output::Text("Checking.".into())]);
        assert!(updates.iter().any(|u| matches!(u, RunUpdate::Text(t) if t.contains("Check"))), "{updates:?}");
    }

    #[test]
    fn errors_after_the_request_went_out_may_be_billed() {
        // A broken stream, an error event, an error status, a garbled whole reply: each was sent,
        // so each may have been billed and is never resent.
        let mut broken = tool_stream();
        broken.truncate(broken.len() / 2);
        let cases: Vec<(Arc<dyn Http>, bool)> = vec![
            (Chunked::new(broken, 16), true),
            (Chunked::new(exception("throttlingException", "slow down"), 16), true),
            (Canned::new(500, r#"{"message":"oops"}"#), false),
            (Canned::new(200, "<html>"), false),
        ];
        for (http, stream_text) in cases {
            let client = client(http);
            let limits = RunLimits { stream_text, ..RunLimits::default() };
            let f = block_on(crate::run_to_completion(&client, &job(), &Never, &Instant, limits, |_| {})).unwrap_err();
            assert!(f.uncertain, "{f:?}");
        }
        // Refused before sending: certainly not billed.
        let mut bad = job();
        bad.inputs.clear();
        let client = client(Canned::new(200, "{}"));
        let f = block_on(crate::run_to_completion(&client, &bad, &Never, &Instant, RunLimits::default(), |_| {})).unwrap_err();
        assert_eq!(f.error.kind, ErrorKind::UnsupportedInput);
        assert!(!f.uncertain, "{f:?}");
    }
}

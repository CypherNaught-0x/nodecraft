//! The chat client's HTTP seam keeps the transport's guarantees.

use super::*;
use crate::SubmitContext;
use crate::http::ChunkSink;
use futures::executor::block_on;

/// Records requests; answers with `status`, `headers` and `body`, streaming the body in pieces.
struct Fixture {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    fail: Option<ProviderError>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Fixture {
    fn new(status: u16, body: &str) -> Arc<Self> {
        Arc::new(Self { status, headers: vec![("x-fixture".into(), "1".into())], body: body.as_bytes().to_vec(), fail: None, seen: Mutex::default() })
    }
    fn failing(error: ProviderError) -> Arc<Self> {
        Arc::new(Self { status: 0, headers: vec![], body: vec![], fail: Some(error), seen: Mutex::default() })
    }
    fn seen(&self) -> HttpRequest {
        self.seen.lock().unwrap()[0].clone()
    }
}

impl Http for Fixture {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.seen.lock().unwrap().push(request);
        let reply = match &self.fail {
            Some(e) => Err(e.clone()),
            None => Ok(HttpResponse { status: self.status, headers: self.headers.clone(), body: self.body.clone() }),
        };
        Box::pin(async move { reply })
    }
    fn send_streaming<'a>(&'a self, request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        self.seen.lock().unwrap().push(request);
        Box::pin(async move {
            if let Some(e) = &self.fail {
                return Err(e.clone());
            }
            if !(200..300).contains(&self.status) {
                return Ok(HttpResponse { status: self.status, headers: self.headers.clone(), body: self.body.clone() });
            }
            for piece in self.body.chunks(3) {
                on_chunk(piece)?;
            }
            Ok(HttpResponse { status: self.status, headers: self.headers.clone(), body: Vec::new() })
        })
    }
}

fn post() -> Request<Bytes> {
    Request::post("https://api.example/v1/chat/completions")
        .header("Content-Type", "application/json")
        .header("User-Agent", "rig/0.46 (linux x86_64; someone)")
        .header("originator", "someone")
        .header("Authorization", "Bearer from-the-crate")
        .body(Bytes::from_static(br#"{"a":1}"#))
        .unwrap()
}

#[test]
fn requests_carry_our_headers_and_none_of_the_crates_own() {
    let http = Fixture::new(200, "{}");
    let t = RigHttp::new(http.clone(), 1234, 5678).with_secret("Authorization", "Bearer ours".into()).with_header("x-mine", "yes");
    let reply = block_on(t.send::<Bytes, Bytes>(post())).unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(block_on(reply.into_body()).unwrap(), Bytes::from_static(b"{}"));
    let sent = http.seen();
    assert_eq!(sent.method, HttpMethod::Post);
    assert_eq!(sent.url, "https://api.example/v1/chat/completions");
    assert_eq!(
        sent.headers,
        vec![("content-type".to_owned(), "application/json".to_owned()), ("x-mine".into(), "yes".into()), ("Authorization".into(), "Bearer ours".into())],
        "the crate's User-Agent, identity and credential headers are dropped"
    );
    assert_eq!(sent.secret_headers, ["authorization"]);
    assert_eq!((sent.timeout_ms, sent.max_response_bytes), (1234, 5678));
    assert_eq!(sent.body, br#"{"a":1}"#);
    assert!(matches!(t.exchange(), Exchange::Answered { status: 200, .. }));
    assert!(!format!("{t:?}").contains("ours"), "Debug shows no key");
    // A dialect can name the crate's headers it needs.
    let http = Fixture::new(200, "{}");
    let t = RigHttp::new(http.clone(), 1, 1).forwarding(&["originator"]);
    block_on(t.send::<Bytes, Bytes>(post())).unwrap();
    assert_eq!(http.seen().headers, vec![("originator".to_owned(), "someone".to_owned())]);
}

#[test]
fn a_body_fix_rewrites_the_json() {
    let http = Fixture::new(200, "{}");
    let t = RigHttp::new(http.clone(), 1, 1).with_fix(Box::new(|body| body["b"] = serde_json::json!(2)));
    block_on(t.send::<Bytes, Bytes>(post())).unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&http.seen().body).unwrap(), serde_json::json!({"a": 1, "b": 2}));
    // A body that isn't JSON is refused before it is sent.
    let http = Fixture::new(200, "{}");
    let t = RigHttp::new(http.clone(), 1, 1).with_fix(Box::new(|_| {}));
    let bad = Request::post("https://api.example/x").body(Bytes::from_static(b"nope")).unwrap();
    assert!(block_on(t.send::<Bytes, Bytes>(bad)).is_err());
    assert!(http.seen.lock().unwrap().is_empty());
    assert!(matches!(t.exchange(), Exchange::Failed(e) if e.kind == ErrorKind::UnsupportedInput));
}

#[test]
fn errors_are_recorded_as_the_transport_saw_them() {
    let refused = Fixture::new(429, r#"{"error":"slow"}"#);
    let t = RigHttp::new(refused, 1, 1);
    let reply = block_on(t.send::<Bytes, Bytes>(post())).unwrap();
    assert_eq!(reply.status(), StatusCode::TOO_MANY_REQUESTS);
    match t.exchange() {
        Exchange::Answered { status, headers, body } => {
            assert_eq!((status, body.as_slice()), (429, br#"{"error":"slow"}"#.as_slice()));
            assert!(headers.contains(&("x-fixture".into(), "1".into())));
        }
        other => panic!("{other:?}"),
    }
    let down = RigHttp::new(Fixture::failing(ProviderError::new(ErrorKind::Network, "fixture down")), 1, 1);
    assert!(block_on(down.send::<Bytes, Bytes>(post())).is_err());
    assert!(matches!(down.exchange(), Exchange::Failed(e) if e.kind == ErrorKind::Network && e.message == "fixture down"));
    let multipart = RigHttp::new(Fixture::new(200, "{}"), 1, 1);
    let form = Request::post("https://api.example/x").body(MultipartForm::new()).unwrap();
    assert!(block_on(multipart.send_multipart::<Bytes>(form)).is_err());
}

fn drain(response: http_client::StreamingResponse) -> (Vec<u8>, Option<String>) {
    let mut body = response.into_body();
    let mut out = Vec::new();
    while let Some(piece) = block_on(body.next()) {
        match piece {
            Ok(piece) => out.extend_from_slice(&piece),
            Err(e) => return (out, Some(e.to_string())),
        }
    }
    (out, None)
}

#[test]
fn streams_arrive_piece_by_piece_with_the_real_head_at_the_end() {
    let http = Fixture::new(200, "data: one\n\ndata: two\n\n");
    let t = RigHttp::new(http.clone(), 1, 1);
    let response = block_on(t.send_streaming(post())).unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-type").and_then(|v| v.to_str().ok()), Some("text/event-stream"));
    let (body, error) = drain(response);
    assert_eq!((body.as_slice(), error), (b"data: one\n\ndata: two\n\n\n\n".as_slice(), None), "a closing blank line ends an unterminated last event");
    assert!(matches!(t.exchange(), Exchange::Answered { status: 200, headers, .. } if headers.contains(&("x-fixture".into(), "1".into()))));
    // An empty 2xx body is an empty stream.
    let empty = RigHttp::new(Fixture::new(200, ""), 1, 1);
    assert_eq!(drain(block_on(empty.send_streaming(post())).unwrap()).0, b"\n\n");
}

#[test]
fn a_streamed_error_reply_is_an_error_with_its_body() {
    let t = RigHttp::new(Fixture::new(503, "busy"), 1, 1);
    let Err(e) = block_on(t.send_streaming(post())) else { panic!("an error reply") };
    assert_eq!((e.non_success_status(), e.non_success_body()), (Some(StatusCode::SERVICE_UNAVAILABLE), Some("busy")));
    assert!(matches!(t.exchange(), Exchange::Answered { status: 503, body, .. } if body == b"busy"));
    let down = RigHttp::new(Fixture::failing(ProviderError::new(ErrorKind::Timeout, "fixture")), 1, 1);
    assert!(block_on(down.send_streaming(post())).is_err());
    assert!(matches!(down.exchange(), Exchange::Failed(e) if e.kind == ErrorKind::Timeout));
}

/// The body breaks after some of it arrived: what arrived comes first, then the error.
#[test]
fn a_stream_that_breaks_keeps_what_arrived() {
    struct Breaks;
    impl Http for Breaks {
        fn send(&self, _request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            Box::pin(async { Err(ProviderError::new(ErrorKind::Server, "unused")) })
        }
        fn send_streaming<'a>(&'a self, _request: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
            Box::pin(async move {
                on_chunk(b"data: half")?;
                Err(ProviderError::new(ErrorKind::Network, "fixture reset"))
            })
        }
    }
    let t = RigHttp::new(Arc::new(Breaks), 1, 1);
    let (body, error) = drain(block_on(t.send_streaming(post())).unwrap());
    assert_eq!(body, b"data: half");
    assert!(error.is_some());
    assert!(matches!(t.exchange(), Exchange::Failed(e) if e.message == "fixture reset"));
}

/// A request sent through a runner's scope is one of its requests: the submit can no longer be
/// proven unsent. One refused before sending leaves the proof standing.
#[test]
fn requests_through_a_delivery_scope_count_as_sent() {
    let context = SubmitContext::new();
    context.arm();
    let t = RigHttp::new(Fixture::new(200, "{}"), 1, 1).with_delivery(Some(context.link()));
    block_on(t.send::<Bytes, Bytes>(post())).unwrap();
    assert!(context.finish_error().is_none(), "a sent request may be billed");

    let context = SubmitContext::new();
    context.arm();
    let streamed = RigHttp::new(Fixture::new(200, "data: x\n\n"), 1, 1).with_delivery(Some(context.link()));
    drain(block_on(streamed.send_streaming(post())).unwrap());
    assert!(context.finish_error().is_none());

    let context = SubmitContext::new();
    context.arm();
    let refused = RigHttp::new(Fixture::new(200, "{}"), 1, 1).with_delivery(Some(context.link())).with_fix(Box::new(|_| {}));
    let bad = Request::post("https://api.example/x").body(Bytes::from_static(b"not json")).unwrap();
    assert!(block_on(refused.send::<Bytes, Bytes>(bad)).is_err());
    assert!(context.finish_error().is_some(), "nothing was sent");
}

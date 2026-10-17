use super::*;
use crate::{CancelSignal, Clock, ErrorKind, JobHandle, JobProgress, Provider, ProviderJob, RunLimits, Submission};
use futures::executor::block_on;
use std::sync::atomic::{AtomicUsize, Ordering};
fn error() -> ProviderError {
    ProviderError::new(ErrorKind::Network, "same diagnostic")
}
struct Local;
impl Http for Local {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        Box::pin(async { Err(error()) })
    }
    fn send_with_delivery<'a>(&'a self, _: HttpRequest, c: &'a RequestContext) -> BoxFuture<'a, Result<HttpResponse, HttpFailure>> {
        c.arm();
        Box::pin(async move { Err(c.failure(error())) })
    }
}
struct Legacy(AtomicUsize);
impl Http for Legacy {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(error()) })
    }
}
#[test]
fn local_request_proof_is_not_whole_operation_proof_without_audit() {
    let c = SubmitContext::new();
    assert!(block_on(c.send(&Local, HttpRequest::get("http://fixture"))).is_err());
    assert!(c.finish_error().is_none());
    let c = SubmitContext::new();
    c.arm();
    assert!(block_on(c.send(&Local, HttpRequest::get("http://fixture"))).is_err());
    assert!(c.finish_error().is_some());
}
#[test]
fn legacy_constructor_effects_and_prior_possible_delivery_are_sticky() {
    let legacy = Legacy(AtomicUsize::new(0));
    let c = SubmitContext::new();
    c.arm();
    assert!(block_on(c.send(&legacy, HttpRequest::get("http://fixture"))).is_err());
    assert_eq!(legacy.0.load(Ordering::SeqCst), 1);
    assert!(block_on(c.send(&Local, HttpRequest::get("http://fixture"))).is_err());
    assert!(c.finish_error().is_none());
    let c = SubmitContext::new();
    c.arm();
    let child = Child::new(Arc::clone(&c.state)).unwrap();
    child.context.arm();
    assert!(child.finish(Ok(HttpResponse { status: 200, headers: vec![], body: vec![] })).is_ok());
    assert!(block_on(c.send(&Local, HttpRequest::get("http://fixture"))).is_err());
    assert!(c.finish_error().is_none());
}
#[test]
fn foreign_child_root_and_reused_proof_fail_closed() {
    for foreign_root in [false, true] {
        let c = SubmitContext::new();
        c.arm();
        let other = SubmitContext::new();
        other.arm();
        let a = Child::new(Arc::clone(&c.state)).unwrap();
        a.context.arm();
        let b = Child::new(Arc::clone(if foreign_root { &other.state } else { &c.state })).unwrap();
        b.context.arm();
        let stolen = a.context.failure(error());
        let second = a.context.failure(error());
        assert!(second.proof.is_none(), "request-local proof issuance is one-shot");
        assert!(a.finish(Err(second)).is_err());
        assert!(b.finish(Err(stolen)).is_err());
        assert!(if foreign_root { other.finish_error() } else { c.finish_error() }.is_none());
    }
}
#[test]
fn unresolved_dropped_forgotten_children_overflow_poison_and_closed_scope_cannot_certify() {
    let c = SubmitContext::new();
    c.arm();
    let child = Child::new(Arc::clone(&c.state)).unwrap();
    assert!(c.finish_error().is_none());
    drop(child);
    let c = SubmitContext::new();
    c.arm();
    let child = Child::new(Arc::clone(&c.state)).unwrap();
    drop(child);
    assert!(c.finish_error().is_none());
    let c = SubmitContext::new();
    c.arm();
    let child = Child::new(Arc::clone(&c.state)).unwrap();
    std::mem::forget(child);
    assert!(c.finish_error().is_none());
    let c = SubmitContext::new();
    c.arm();
    c.state.lock().unwrap().children = usize::MAX;
    assert!(Child::new(Arc::clone(&c.state)).is_none());
    assert!(c.finish_error().is_none());
    let c = SubmitContext::new();
    c.arm();
    let child = Child::new(Arc::clone(&c.state)).unwrap();
    c.state.lock().unwrap().children = 0;
    child.context.arm();
    let failure = child.context.failure(error());
    let _ = child.finish(Err(failure));
    assert!(c.finish_error().is_none());
    let c = SubmitContext::new();
    c.arm();
    let saved = Arc::clone(&c.state);
    assert!(c.finish_error().is_some());
    assert!(Child::new(saved).is_none(), "no child can start after finalization");
    let c = SubmitContext::new();
    c.arm();
    let saved = Arc::clone(&c.state);
    let _ = std::panic::catch_unwind(move || {
        let _guard = saved.lock().unwrap();
        panic!("poison fixture");
    });
    assert!(c.finish_error().is_none());
}
#[test]
fn unpolled_child_is_registered_before_transport_constructor_and_dropped_as_unknown() {
    let c = SubmitContext::new();
    c.arm();
    let work = c.send(&Local, HttpRequest::get("http://fixture"));
    assert_eq!(c.state.lock().unwrap().children, 1);
    drop(work);
    assert!(c.finish_error().is_none());
}
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
struct Constructor(AtomicUsize);
impl Provider for Constructor {
    fn id(&self) -> &str {
        "legacy"
    }
    fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ProviderError::new(ErrorKind::Auth, "local-looking")) })
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(futures::future::pending())
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}
fn job() -> ProviderJob {
    ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: "public-fixture".into() }
}
#[test]
fn legacy_provider_constructor_is_unknown_for_local_looking_error() {
    let p = Constructor(AtomicUsize::new(0));
    let f = block_on(crate::run_to_completion(&p, &job(), &Never, &Instant, RunLimits::default(), |_| {})).unwrap_err();
    assert!(f.uncertain);
    assert_eq!(p.0.load(Ordering::SeqCst), 1);
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn native_build_is_positive_but_any_execute_attempt_is_unknown() {
    let http = crate::ReqwestHttp::new().unwrap();
    let c = SubmitContext::new();
    c.arm();
    let failure = c.send(&http, HttpRequest::get("http://127.0.0.1:1").header("invalid\nheader", "public")).await.unwrap_err();
    assert_eq!(failure.kind, ErrorKind::Network);
    assert!(c.finish_error().is_some());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let c = SubmitContext::new();
    c.arm();
    let failure = c.send(&http, HttpRequest::get(format!("http://{address}"))).await.unwrap_err();
    assert_eq!(failure.kind, ErrorKind::Network);
    assert!(c.finish_error().is_none());
    let c = SubmitContext::new();
    c.arm();
    let child = Child::new(Arc::clone(&c.state)).unwrap();
    let mut sink = |_: &[u8]| Ok(());
    let result = http.send_streaming_with_delivery(HttpRequest::get("http://127.0.0.1:1").header("invalid\nheader", "public"), &mut sink, &child.context).await;
    let _ = child.finish(result);
    assert!(c.finish_error().is_some());
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
fn served(replies: Vec<Vec<u8>>) -> (String, Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    let task = std::thread::spawn(move || {
        for reply in replies {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < end => std::thread::yield_now(),
                    Err(e) => panic!("bounded accept: {e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
            stream.set_write_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut bytes = 0;
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    return; // The client closed before the header terminator; no complete request.
                }
                bytes += line.len();
                assert!(bytes < 65536);
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse::<usize>().unwrap();
                }
            }
            assert!(length < 65536);
            reader.read_exact(&mut vec![0; length]).unwrap();
            observed.fetch_add(1, Ordering::SeqCst);
            let _ = stream.write_all(&reply);
        }
    });
    (url, count, task)
}
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
fn reply(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut data = format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    data.extend_from_slice(body);
    data
}
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn observed_dispatch_redirect_exhaustion_body_and_sink_failures_stay_unknown() {
    let http = crate::ReqwestHttp::new().unwrap();
    let (url, seen, task) = served(vec![vec![]]);
    let c = SubmitContext::new();
    c.arm();
    let e = c.send(&http, HttpRequest::post_json(url, &serde_json::json!({})).unwrap()).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Network);
    task.join().unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    assert!(c.finish_error().is_none());
    let (url, seen, task) = served((0..=crate::MAX_REDIRECTS).map(|_| reply("307 Temporary Redirect", "Location: /again\r\n", b"")).collect());
    let c = SubmitContext::new();
    c.arm();
    let e = c.send(&http, HttpRequest::post_json(url, &serde_json::json!({})).unwrap()).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Malformed);
    task.join().unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), crate::MAX_REDIRECTS + 1);
    assert!(c.finish_error().is_none());
    let (url, _, task) = served(vec![reply("200 OK", "", b"too large")]);
    let c = SubmitContext::new();
    c.arm();
    assert!(c.send(&http, HttpRequest::get(url).max_response_bytes(1)).await.is_err());
    task.join().unwrap();
    assert!(c.finish_error().is_none());
    let (url, _, task) = served(vec![reply("200 OK", "", b"chunks")]);
    let c = SubmitContext::new();
    c.arm();
    let child = Child::new(Arc::clone(&c.state)).unwrap();
    let mut sink = |_: &[u8]| Err(ProviderError::new(ErrorKind::Malformed, "fixture sink refusal"));
    let result = http.send_streaming_with_delivery(HttpRequest::get(url), &mut sink, &child.context).await;
    assert!(child.finish(result).is_err());
    task.join().unwrap();
    assert!(c.finish_error().is_none());
}
#[cfg(all(feature = "llm", feature = "http", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn actual_llm_provider_local_build_and_post_response_errors_use_evidence() {
    use crate::llm::{ApiKey, Dialect, Endpoint, LlmClient};
    let endpoint = |url: String, key| Endpoint { id: "fixture".into(), dialect: Dialect::OpenAi, base_url: url, key };
    let mut request = job();
    request.inputs = vec![crate::JobInput::Text { role: "user".into(), text: "public".into() }];
    let p = LlmClient::new(endpoint("http://127.0.0.1:1".into(), Some(ApiKey::new("public\ninvalid"))), Arc::new(crate::ReqwestHttp::new().unwrap()));
    let f = crate::run_to_completion(&p, &request, &Never, &Instant, RunLimits::default(), |_| {}).await.unwrap_err();
    assert_eq!(f.error.kind, ErrorKind::Network);
    assert!(!f.uncertain);
    for (status, body, kind) in [("200 OK", b"invalid JSON".as_slice(), ErrorKind::Malformed), ("401 Unauthorized", b"{}".as_slice(), ErrorKind::Auth)] {
        let (url, seen, task) = served(vec![reply(status, "", body)]);
        let p = LlmClient::new(endpoint(url, None), Arc::new(crate::ReqwestHttp::new().unwrap()));
        let f = crate::run_to_completion(&p, &request, &Never, &Instant, RunLimits::default(), |_| {}).await.unwrap_err();
        task.join().unwrap();
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        assert_eq!(f.error.kind, kind);
        assert!(f.uncertain);
    }
}
#[test]
fn request_poison_and_unknown_browser_stage_fail_closed() {
    let c = SubmitContext::new();
    c.arm();
    let child = Child::new(Arc::clone(&c.state)).unwrap();
    child.context.arm();
    child.context.unknown();
    let f = child.context.failure(error());
    let _ = child.finish(Err(f));
    assert!(c.finish_error().is_none());
    let c = SubmitContext::new();
    c.arm();
    let child = Child::new(Arc::clone(&c.state)).unwrap();
    child.context.arm();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _lock = child.context.state.lock().unwrap();
        panic!("request poison fixture");
    }));
    let f = child.context.failure(error());
    let _ = child.finish(Err(f));
    assert!(c.finish_error().is_none());
}

#[test]
fn audited_constructor_cancellation_before_future_poll_still_has_no_nonacceptance_proof() {
    use std::sync::atomic::AtomicBool;
    struct Signal(Arc<AtomicBool>);
    impl CancelSignal for Signal {
        fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
        fn cancelled(&self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }
    struct Cancels {
        signal: Arc<AtomicBool>,
        polled: AtomicUsize,
    }
    impl Provider for Cancels {
        fn id(&self) -> &str {
            "audited-fixture"
        }
        fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            Box::pin(futures::future::pending())
        }
        fn submit_with_delivery<'a>(&'a self, _: &'a ProviderJob, c: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
            c.arm();
            self.signal.store(true, Ordering::SeqCst);
            Box::pin(futures::future::poll_fn(|_| {
                self.polled.fetch_add(1, Ordering::SeqCst);
                std::task::Poll::Pending
            }))
        }
        fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
            Box::pin(futures::future::pending())
        }
        fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async { Ok(()) })
        }
    }
    let signal = Arc::new(AtomicBool::new(false));
    let p = Cancels { signal: signal.clone(), polled: AtomicUsize::new(0) };
    let failure = block_on(crate::run_to_completion(&p, &job(), &Signal(signal), &Instant, RunLimits::default(), |_| {})).unwrap_err();
    assert!(failure.uncertain);
    assert_eq!(failure.error.kind, ErrorKind::Cancelled);
    assert_eq!(p.polled.load(Ordering::SeqCst), 0);
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[test]
fn loopback_fixture_terminates_on_empty_and_partial_header_eof() {
    use std::io::Write;
    for partial in [b"".as_slice(), b"GET / HTTP/1.1\r\nX-Public: partial".as_slice()] {
        let (url, count, task) = served(vec![reply("200 OK", "", b"unused")]);
        let mut client = std::net::TcpStream::connect(url.strip_prefix("http://").unwrap()).unwrap();
        client.write_all(partial).unwrap();
        client.shutdown(std::net::Shutdown::Both).unwrap();
        drop(client);
        let (done, completion) = std::sync::mpsc::channel();
        // Do not join the fixture on this test thread: the old EOF bug would block forever.
        let monitor = std::thread::spawn(move || {
            let _ = done.send(task.join().is_ok());
        });
        assert!(completion.recv_timeout(std::time::Duration::from_secs(4)).expect("fixture must terminate after early header EOF"));
        monitor.join().unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 0, "an incomplete request was not observed as complete");
    }
}

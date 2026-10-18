//! Test-only origin rewrite into two local listeners; production URL admission is unchanged.
// This entire module is cfg(test), including the bounded listener worker helpers.
#![allow(clippy::unwrap_used, clippy::panic)]
use super::*;
use crate::HttpResponse;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Mutex,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
struct Local {
    base: String,
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Local {
    fn new(replies: Vec<(u16, Vec<u8>, Option<String>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let c = calls.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let s = stop.clone();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !s.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
                        stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                        let mut request = Vec::new();
                        let mut chunk = [0; 1024];
                        loop {
                            let n = stream.read(&mut chunk).unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            request.extend_from_slice(&chunk[..n]);
                            assert!(request.len() < 65536);
                            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                                let header = String::from_utf8_lossy(&request[..end]);
                                let length = header
                                    .lines()
                                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length: ").and_then(|n| n.parse::<usize>().ok()))
                                    .unwrap_or(0);
                                if request.len() >= end + 4 + length {
                                    break;
                                }
                            }
                        }
                        captured.lock().unwrap().push(request);
                        let index = c.fetch_add(1, Ordering::SeqCst);
                        if let Some((status, body, location)) = replies.get(index) {
                            let location = location.as_ref().map(|l| format!("Location: {l}\r\n")).unwrap_or_default();
                            let header = format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\n{location}Connection: close\r\n\r\n", body.len());
                            stream.write_all(header.as_bytes()).unwrap();
                            stream.write_all(body).unwrap();
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(2)),
                    Err(_) => break,
                }
            }
        });
        Self { base, calls, requests, stop, worker: Some(worker) }
    }
    fn finish(mut self) -> usize {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
        self.calls.load(Ordering::SeqCst)
    }
}
struct Rewrite {
    origin: String,
    inner: crate::ReqwestHttp,
}
impl Http for Rewrite {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-following path")
    }
    fn send_no_redirect(&self, mut r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let parsed = url::Url::parse(&r.url).unwrap();
        assert!(matches!(parsed.host_str(), Some("api.replicate.com" | "replicate.delivery")));
        r.url = format!("{}{}", self.origin, parsed.path());
        self.inner.send_no_redirect(r)
    }
}
fn prediction(status: &str, output: serde_json::Value) -> Vec<u8> {
    let (m, v) = HELLO.split_once(':').unwrap();
    serde_json::to_vec(&serde_json::json!({"id":"public-id","model":m,"version":v,"status":status,"output":output})).unwrap()
}
#[tokio::test(flavor = "current_thread")]
async fn real_native_create_redirects_are_never_followed() {
    for status in [301, 302, 303, 307, 308] {
        let destination = Local::new(vec![]);
        let source = Local::new(vec![(status, vec![], Some(format!("{}/stolen", destination.base)))]);
        let p = Replicate::new(
            Secret::new("public-fixture-not-secret").unwrap(),
            Arc::new(Rewrite { origin: source.base.clone(), inner: crate::ReqwestHttp::new().unwrap() }),
        );
        let job = ProviderJob {
            model: HELLO.into(),
            params: serde_json::Map::new(),
            inputs: vec![crate::JobInput::Text { role: "text".into(), text: "Hello".into() }],
            idempotency_key: "fixture".into(),
        };
        assert!(p.submit(&job).await.is_err());
        assert_eq!(source.finish(), 1);
        assert_eq!(destination.finish(), 0);
    }
    let source =
        Local::new(vec![(200, prediction("starting", serde_json::Value::Null), None), (200, prediction("succeeded", serde_json::json!("Hello")), None)]);
    let p = Replicate::new(
        Secret::new("public-fixture-not-secret").unwrap(),
        Arc::new(Rewrite { origin: source.base.clone(), inner: crate::ReqwestHttp::new().unwrap() }),
    );
    let job = ProviderJob {
        model: HELLO.into(),
        params: serde_json::Map::new(),
        inputs: vec![crate::JobInput::Text { role: "text".into(), text: "Hello".into() }],
        idempotency_key: "fixture".into(),
    };
    let crate::Submission::Queued(h) = p.submit(&job).await.unwrap() else { panic!("handle") };
    assert!(matches!(p.poll(&h).await.unwrap(), JobProgress::Done(_)));
    assert_eq!(source.finish(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn real_native_download_redirects_are_never_followed() {
    for status in [301, 302, 303, 307, 308] {
        let reply = |state: &str, output: serde_json::Value| {
            let (m, v) = SDXL.split_once(':').unwrap();
            serde_json::to_vec(&serde_json::json!({"id":"public-id","model":m,"version":v,"status":state,"output":output})).unwrap()
        };
        let destination = Local::new(vec![]);
        let source = Local::new(vec![
            (200, reply("starting", serde_json::Value::Null), None),
            (200, reply("succeeded", serde_json::json!(["https://replicate.delivery/result.png"])), None),
            (status, vec![], Some(format!("{}/stolen", destination.base))),
        ]);
        let p = Replicate::new(
            Secret::new("public-fixture-not-secret").unwrap(),
            Arc::new(Rewrite { origin: source.base.clone(), inner: crate::ReqwestHttp::new().unwrap() }),
        );
        let job = ProviderJob {
            model: SDXL.into(),
            params: serde_json::Map::new(),
            inputs: vec![crate::JobInput::Text { role: "prompt".into(), text: "Blue circle".into() }],
            idempotency_key: "fixture".into(),
        };
        let Submission::Queued(h) = p.submit(&job).await.unwrap() else { panic!("handle") };
        assert!(p.poll(&h).await.is_err());
        assert_api_and_delivery_headers(&source);
        assert_eq!(source.finish(), 3);
        assert_eq!(destination.finish(), 0);
    }
}

fn assert_api_and_delivery_headers(local: &Local) {
    let requests = local.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for (index, request) in requests.iter().enumerate() {
        let end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let header = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
        if index < 2 {
            assert!(header.contains("authorization: bearer public-fixture-not-secret"));
        } else {
            assert!(!header.contains("authorization:"));
            assert!(!header.contains("cookie:"));
            assert!(!header.contains("public-fixture-not-secret"));
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn actual_native_delivery_success_has_zero_authentication_headers() {
    let reply = |state: &str, output: serde_json::Value| {
        let (m, v) = SDXL.split_once(':').unwrap();
        serde_json::to_vec(&serde_json::json!({"id":"public-id","model":m,"version":v,"status":state,"output":output})).unwrap()
    };
    let png = nodecraft_media::encode_png(1, 1, &[50, 60, 70, 255], &nodecraft_media::Limits::default()).unwrap();
    let source = Local::new(vec![
        (200, reply("starting", serde_json::Value::Null), None),
        (200, reply("succeeded", serde_json::json!(["https://replicate.delivery/a"])), None),
        (200, png, None),
    ]);
    let p = Replicate::new(
        Secret::new("public-fixture-not-secret").unwrap(),
        Arc::new(Rewrite { origin: source.base.clone(), inner: crate::ReqwestHttp::new().unwrap() }),
    );
    let job = ProviderJob {
        model: SDXL.into(),
        params: Default::default(),
        inputs: vec![crate::JobInput::Text { role: "prompt".into(), text: "public".into() }],
        idempotency_key: "fixture".into(),
    };
    let Submission::Queued(h) = p.submit(&job).await.unwrap() else { panic!("handle") };
    assert!(matches!(p.poll(&h).await.unwrap(), JobProgress::Done(_)));
    assert_api_and_delivery_headers(&source);
    assert_eq!(source.finish(), 3);
}

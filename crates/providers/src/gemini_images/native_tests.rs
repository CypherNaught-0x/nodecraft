//! Loopback parity for the actual request builder and no-redirect native transport.
use super::*;
use crate::{HttpMethod, HttpRequest, HttpResponse, JobInput};
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::{Duration, Instant},
};

struct Local {
    http: crate::ReqwestHttp,
    origin: String,
}
impl Http for Local {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        Box::pin(async { Err(invalid("redirect-capable fallback must not be invoked")) })
    }
    fn send_no_redirect(&self, mut request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        assert_eq!(request.url, "https://generativelanguage.googleapis.com/v1beta/interactions");
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.timeout_ms, REQUEST_TIMEOUT_MS);
        request.url = format!("{}/v1beta/interactions", self.origin);
        request.timeout_ms = 10_000; // Only the bounded local fixture server.
        self.http.send_no_redirect(request)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn actual_native_headers_positive_and_redirect_refusal() {
    for status in [200, 307] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let (mut socket, _) = loop {
                match listener.accept() {
                    Ok(value) => break value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
                    Err(error) => panic!("{error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            socket.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut bytes = Vec::new();
            loop {
                assert!(Instant::now() < deadline);
                let mut chunk = [0; 1024];
                let n = socket.read(&mut chunk).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
                assert!(bytes.len() <= 8192);
                if let Some(pos) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..pos]).to_lowercase();
                    let length =
                        headers.lines().find_map(|line| line.strip_prefix("content-length:").and_then(|value| value.trim().parse::<usize>().ok())).unwrap_or(0);
                    if bytes.len() >= pos + 4 + length {
                        break;
                    }
                }
            }
            let body = if status == 200 { include_bytes!("../../tests/fixtures/gemini_images/success.json").as_slice() } else { b"".as_slice() };
            write!(socket, "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/never\r\nConnection: close\r\n\r\n", body.len())
                .unwrap();
            socket.write_all(body).unwrap();
            bytes
        });
        let provider = GeminiImages::new(Secret::new("fixture-loopback-key").unwrap(), Arc::new(Local { http: crate::ReqwestHttp::new().unwrap(), origin }));
        let job = ProviderJob {
            model: LITE.into(),
            params: Default::default(),
            inputs: vec![JobInput::Text { role: "prompt".into(), text: "Public blue square".into() }],
            idempotency_key: "fixture".into(),
        };
        let result = provider.submit(&job).await;
        let request = worker.join().unwrap();
        let text = String::from_utf8(request).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        let head = head.to_lowercase();
        assert!(head.starts_with("post /v1beta/interactions http/1.1"));
        assert!(head.lines().any(|line| line == "x-goog-api-key: fixture-loopback-key"));
        assert!(head.lines().any(|line| line == format!("user-agent: {}", crate::USER_AGENT.to_lowercase())));
        assert!(!head.lines().any(|line| line.starts_with("authorization:")));
        assert_eq!(serde_json::from_str::<serde_json::Value>(body).unwrap()["model"], LITE);
        if status == 200 {
            let Submission::Finished(done) = result.unwrap() else { panic!() };
            assert!(!done.outputs.is_empty());
        } else {
            assert_eq!(result.unwrap_err().kind, ErrorKind::Malformed);
        }
    }
}

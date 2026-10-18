//! Actual native no-redirect transport with local authored HTTP, not a paid endpoint.
// This module and its bounded worker helpers compile only under cfg(test).
#![allow(clippy::unwrap_used, clippy::panic)]
use super::*;
use crate::{HttpRequest, HttpResponse, JobInput, ReqwestHttp};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::{Duration, Instant},
};
struct Local {
    origin: String,
    http: ReqwestHttp,
}
impl Http for Local {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("no following redirects")
    }
    fn send_no_redirect(&self, mut r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        assert_eq!(r.url, API);
        r.url = format!("{}/images/generations", self.origin);
        r.timeout_ms = 1000;
        self.http.send_no_redirect(r)
    }
}
fn serve(listener: TcpListener, response: Vec<u8>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        let mut socket = loop {
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end => thread::sleep(Duration::from_millis(5)),
                _ => panic!("bounded listener accept"),
            }
        };
        socket.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        socket.set_write_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 1024];
            let n = socket.read(&mut chunk).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
            assert!(bytes.len() < 16384);
            if let Some(pos) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..pos]).to_lowercase();
                let length = header.lines().find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())).unwrap();
                if bytes.len() >= pos + 4 + length {
                    break;
                }
            }
        }
        socket.write_all(&response).unwrap();
        bytes
    })
}
#[tokio::test(flavor = "current_thread")]
async fn real_native_transport_refuses_redirect_and_has_positive_control() {
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    destination.set_nonblocking(true).unwrap();
    let target = destination.local_addr().unwrap();
    for redirect in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let payload = super::tests::body(&super::tests::jpeg(16, 16), 16, 16);
        let response = if redirect {
            format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
        } else {
            let mut r = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len()).into_bytes();
            r.extend_from_slice(&payload);
            r
        };
        let thread = serve(listener, response);
        let p = Seedream::new(Secret::new("fixture-secret-not-real").unwrap(), Arc::new(Local { origin, http: ReqwestHttp::new().unwrap() }));
        let j = ProviderJob {
            model: MODEL.into(),
            params: Default::default(),
            inputs: vec![JobInput::Text { role: "prompt".into(), text: "Circle".into() }],
            idempotency_key: "fixture".into(),
        };
        let result = p.submit(&j).await;
        assert_eq!(result.is_ok(), !redirect);
        if redirect {
            assert_eq!(result.unwrap_err().kind, ErrorKind::Malformed);
        }
        let sent = thread.join().unwrap();
        let header = String::from_utf8_lossy(&sent).to_lowercase();
        assert!(header.contains("authorization: bearer fixture-secret-not-real"));
        assert!(header.contains("user-agent: nodecraft/"));
        assert_eq!(destination.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }
}

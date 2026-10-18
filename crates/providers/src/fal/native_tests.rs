use super::*;
use futures::executor::block_on;
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::{Duration, Instant},
};
#[tokio::test(flavor = "current_thread")]
async fn native_no_redirect_cap_and_positive_control() {
    for (status, body, cap, success) in [(200, b"fixture".as_slice(), 7, true), (200, b"fixture".as_slice(), 6, false), (307, b"".as_slice(), 7, true)] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(10);
            let (mut socket, _) = loop {
                match listener.accept() {
                    Ok(s) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => std::thread::sleep(Duration::from_millis(1)),
                    Err(e) => panic!("{e}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            socket.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut request = [0; 4096];
            let n = socket.read(&mut request).unwrap();
            assert!(n > 0);
            write!(socket, "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/never\r\nConnection: close\r\n\r\n", body.len())
                .unwrap();
            socket.write_all(body).unwrap();
        });
        let result = crate::ReqwestHttp::new()
            .unwrap()
            .send_no_redirect(HttpRequest::get(format!("http://{address}/fixture")).max_response_bytes(cap).timeout_ms(10_000))
            .await;
        worker.join().unwrap();
        assert_eq!(result.is_ok(), success);
        if let Ok(r) = result {
            assert_eq!(r.status, status);
            assert_eq!(r.body, body);
        }
    }
    // Legacy-only transports refuse before invoking their send; adapters never fall back.
    struct Legacy;
    impl Http for Legacy {
        fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            panic!("legacy transport invoked")
        }
    }
    let e = block_on(Fal::new(Secret::new("fixture-key").unwrap(), Arc::new(Legacy)).submit(&tests::job(FLUX))).unwrap_err();
    assert_eq!(e.kind, ErrorKind::UnsupportedInput);
}

#[tokio::test(flavor = "current_thread")]
async fn native_queue_adapter_reaches_complete_png_and_best_effort_cancel() {
    struct Local {
        http: crate::ReqwestHttp,
        origin: String,
    }
    impl Http for Local {
        fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            panic!("no redirect fallback")
        }
        fn send_no_redirect(&self, mut r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            let path = if let Some(p) = r.url.strip_prefix(ORIGIN) {
                assert!(r.headers.iter().any(|(k, _)| k == "Authorization"));
                p.to_owned()
            } else {
                assert_eq!(r.url, "https://fal.media/f");
                assert!(r.headers.is_empty());
                "/media".into()
            };
            r.url = format!("{}{path}", self.origin);
            r.timeout_ms = 10_000;
            self.http.send_no_redirect(r)
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let bodies = vec![
        serde_json::to_vec(&tests::receipt(FLUX)).unwrap(),
        br#"{"status":"COMPLETED"}"#.to_vec(),
        br#"{"images":[{"url":"https://fal.media/f","width":2,"height":2}]}"#.to_vec(),
        tests::png(2, 2),
        br#"{"status":"CANCELLATION_REQUESTED"}"#.to_vec(),
    ];
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for body in bodies {
            let deadline = Instant::now() + Duration::from_secs(10);
            let (mut socket, _) = loop {
                match listener.accept() {
                    Ok(v) => break v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
                    Err(e) => panic!("{e}"),
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
                if let Some(pos) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..pos]).to_lowercase();
                    let len = header.lines().find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())).unwrap_or(0);
                    if bytes.len() >= pos + 4 + len {
                        break;
                    }
                }
            }
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            socket.write_all(&body).unwrap();
            requests.push(bytes);
        }
        requests
    });
    let f = Fal::new(Secret::new("fixture-key-not-real").unwrap(), Arc::new(Local { http: crate::ReqwestHttp::new().unwrap(), origin }));
    let Submission::Queued(h) = f.submit(&tests::job(FLUX)).await.unwrap() else { panic!() };
    let JobProgress::Done(d) = f.poll(&h).await.unwrap() else { panic!() };
    assert_eq!(d.outputs.len(), 1);
    assert_eq!(d.outputs[0], Output::Bytes { mime: "image/png".into(), bytes: tests::png(2, 2).into() });
    f.cancel(&h).await.unwrap();
    let requests = worker.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(String::from_utf8_lossy(&requests[0]).starts_with("POST /fal-ai/flux/schnell"));
    assert!(String::from_utf8_lossy(&requests[4]).starts_with("PUT /fal-ai/flux/requests/r_1/cancel"));
    assert!(!String::from_utf8_lossy(&requests[3]).to_lowercase().contains("authorization:"));
}

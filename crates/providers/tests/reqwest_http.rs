//! `ReqwestHttp` against a local HTTP server (no real providers): User-Agent, size caps and the
//! cross-origin redirect rule.

#![cfg(all(feature = "http", not(target_arch = "wasm32")))]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use nodecraft_providers::{ErrorKind, Http, HttpRequest, ReqwestHttp, USER_AGENT};

/// Serves one canned response per connection and reports each request's head.
fn serve(responses: Vec<Vec<u8>>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for response in responses {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
                head.push_str(&line);
            }
            let mut body = vec![0; content_length];
            let _ = reader.read_exact(&mut body);
            let _ = tx.send(head);
            let _ = stream.write_all(&response);
        }
    });
    (url, rx)
}

fn response(status: &str, extra: &str, body: &[u8]) -> Vec<u8> {
    let mut r = format!("HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    r.extend_from_slice(body);
    r
}

#[tokio::test(flavor = "current_thread")]
async fn sends_the_generic_user_agent_and_reads_the_reply() {
    let (url, heads) = serve(vec![response("200 OK", "X-Thing: 1\r\n", b"{\"ok\":true}")]);
    let http = ReqwestHttp::new().unwrap();
    let r = http.send(HttpRequest::get(format!("{url}/v1/x")).header("Authorization", "Key secret")).await.unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.header("x-thing"), Some("1"));
    assert_eq!(r.json::<serde_json::Value>().unwrap()["ok"], true);
    let head = heads.recv().unwrap().to_ascii_lowercase();
    assert!(head.contains(&format!("user-agent: {}", USER_AGENT.to_ascii_lowercase())), "{head}");
}

#[tokio::test(flavor = "current_thread")]
async fn replies_over_the_cap_are_refused() {
    let big = vec![b'x'; 10_000];
    let (url, _heads) = serve(vec![response("200 OK", "", &big)]);
    let http = ReqwestHttp::new().unwrap();
    let e = http.send(HttpRequest::get(url).max_response_bytes(1000)).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Malformed);
}

#[tokio::test(flavor = "current_thread")]
async fn cross_origin_redirects_are_followed_without_credentials() {
    let (other, other_heads) = serve(vec![response("200 OK", "", b"file")]);
    let (url, heads) = serve(vec![response("302 Found", &format!("Location: {other}/file\r\n"), b"")]);
    let http = ReqwestHttp::new().unwrap();
    let r = http.send(HttpRequest::get(url).secret_header("X-Vendor-Key", "secret").header("Authorization", "Bearer secret2")).await.unwrap();
    assert_eq!((r.status, r.body.as_slice()), (200, &b"file"[..]));
    assert!(heads.recv().unwrap().to_ascii_lowercase().contains("x-vendor-key: secret"), "the origin got its key");
    let away = other_heads.recv().unwrap().to_ascii_lowercase();
    assert!(!away.contains("secret"), "the other host got no credentials: {away}");
}

#[tokio::test(flavor = "current_thread")]
async fn same_origin_redirects_keep_headers_and_hops_are_capped() {
    let (url, heads) = serve(vec![response("307 Temporary Redirect", "Location: /v2\r\n", b""), response("200 OK", "", b"ok")]);
    let http = ReqwestHttp::new().unwrap();
    let r = http.send(HttpRequest::get(format!("{url}/v1")).secret_header("X-Vendor-Key", "k")).await.unwrap();
    assert_eq!(r.body, b"ok");
    heads.recv().unwrap();
    assert!(heads.recv().unwrap().to_ascii_lowercase().contains("x-vendor-key: k"));

    let looping: Vec<Vec<u8>> = (0..10).map(|_| response("302 Found", "Location: /again\r\n", b"")).collect();
    let (url, _heads) = serve(looping);
    let e = http.send(HttpRequest::get(url)).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Malformed);
}

#[tokio::test(flavor = "current_thread")]
async fn streamed_bodies_without_a_length_are_capped() {
    // No Content-Length: the cap must trip while reading, not up front.
    let mut raw = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    raw.extend(std::iter::repeat_n(b'x', 50_000));
    let (url, _heads) = serve(vec![raw]);
    let http = ReqwestHttp::new().unwrap();
    let e = http.send(HttpRequest::get(url).max_response_bytes(1000)).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Malformed);
}

#[tokio::test(flavor = "current_thread")]
async fn slow_servers_time_out() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let _held = listener.accept();
        std::thread::sleep(std::time::Duration::from_secs(3));
    });
    let http = ReqwestHttp::new().unwrap();
    let e = http.send(HttpRequest::get(url).timeout_ms(200)).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Timeout);
    assert!(e.is_transient());
}

#[tokio::test(flavor = "current_thread")]
async fn downloads_through_the_transport_are_capped_and_hashed() {
    let png = b"\x89PNG\r\n\x1a\nIMAGEDATA";
    let (url, _h) = serve(vec![response("200 OK", "", png), response("200 OK", "", png)]);
    let http = ReqwestHttp::new().unwrap();
    let d = nodecraft_providers::fetch_output(&http, &url, Some(nodecraft_media::MediaKind::Image), 1024).await.unwrap();
    assert_eq!(d.hash, nodecraft_media::ContentHash::of(png));
    let e = nodecraft_providers::fetch_output(&http, &url, None, 4).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Malformed);
}

#[tokio::test(flavor = "current_thread")]
async fn connection_failures_are_network_errors_without_secrets() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let http = ReqwestHttp::new().unwrap();
    let e = http.send(HttpRequest::get(format!("http://{addr}/x?token=secret"))).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Network);
    assert!(!e.to_string().contains("secret"), "{e}");
}

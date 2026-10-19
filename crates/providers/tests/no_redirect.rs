//! Real native loopback transport proofs, never an OpenAI connection.
#![cfg(all(feature = "http", not(target_arch = "wasm32")))]
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpRequest, HttpResponse, ProviderError, ReqwestHttp};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
// A failed client must not strand the fixture worker or its parent join forever.
fn accept_within(listener: &TcpListener, wait: Duration) -> std::io::Result<TcpStream> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + wait;
    loop {
        match listener.accept() {
            Ok((socket, _)) => {
                // Preserve the blocking socket read timeout on platforms that inherit flags.
                socket.set_nonblocking(false)?;
                return Ok(socket);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "fixture received no client before its deadline"));
                }
                std::thread::sleep(Duration::from_millis(2).min(remaining));
            }
            Err(e) => return Err(e),
        }
    }
}
#[test]
fn fixture_accept_without_a_client_terminates_at_its_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let start = Instant::now();
    let wait = Duration::from_millis(20);
    let error = accept_within(&listener, wait).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(start.elapsed() >= wait);
    assert!(start.elapsed() < Duration::from_secs(2));
}
fn read(mut socket: TcpStream) -> (TcpStream, Vec<u8>) {
    socket.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    socket.set_write_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut len = 0;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        assert!(line.len() < 8192);
        if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            len = n.trim().parse().unwrap();
        }
    }
    assert!(len < 4096);
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).unwrap();
    socket.flush().unwrap();
    (socket, bytes)
}
#[tokio::test(flavor = "current_thread")]
async fn no_redirect_never_replays_private_post_at_same_or_other_origin() {
    for status in [301, 302, 303, 307, 308] {
        for same_origin in [false, true] {
            let origin = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = origin.local_addr().unwrap();
            let destination = TcpListener::bind("127.0.0.1:0").unwrap();
            destination.set_nonblocking(true).unwrap();
            let location = if same_origin { format!("http://{address}/second") } else { format!("http://{}/second", destination.local_addr().unwrap()) };
            let worker = std::thread::spawn(move || {
                let socket = accept_within(&origin, Duration::from_secs(2)).unwrap();
                let (mut socket, body) = read(socket);
                write!(socket, "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                drop(socket);
                origin.set_nonblocking(true).unwrap();
                (origin, body)
            });
            let request = HttpRequest::post_json(format!("http://{address}/first"), &serde_json::json!({"private_image":"fixture-image"}))
                .unwrap()
                .secret_header("Authorization", "Bearer fixture-not-real")
                .timeout_ms(300);
            let expected = request.body.clone();
            let result = ReqwestHttp::new().unwrap().send_no_redirect(request).await;
            let (origin, body) = worker.join().unwrap();
            assert_eq!(body, expected, "the origin received exactly one complete POST");
            assert_eq!(origin.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock, "same-origin destination received zero requests");
            assert_eq!(destination.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock, "cross-origin destination received zero requests");
            assert_eq!(result.unwrap().status, status);
        }
    }
}
fn serve(body: Vec<u8>, length: bool) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let stream = accept_within(&listener, Duration::from_secs(2)).unwrap();
        let (mut stream, _) = read(stream);
        let header = if length { format!("Content-Length: {}\r\n", body.len()) } else { String::new() };
        write!(stream, "HTTP/1.1 200 OK\r\n{header}Connection: close\r\n\r\n").unwrap();
        let _ = stream.write_all(&body);
    });
    (format!("http://{address}/one"), worker)
}
#[tokio::test(flavor = "current_thread")]
async fn no_redirect_success_and_both_response_length_paths_obey_the_cap() {
    for length in [false, true] {
        let (url, worker) = serve(b"OK".to_vec(), length);
        let r = ReqwestHttp::new().unwrap().send_no_redirect(HttpRequest::get(url).max_response_bytes(2)).await.unwrap();
        worker.join().unwrap();
        assert_eq!(r.body, b"OK");
        let (url, worker) = serve(vec![b'x'; 4096], length);
        let e = ReqwestHttp::new().unwrap().send_no_redirect(HttpRequest::get(url).max_response_bytes(4)).await.unwrap_err();
        worker.join().unwrap();
        assert_eq!(e.kind, ErrorKind::Malformed);
    }
}
struct Legacy(Arc<AtomicUsize>);
impl Http for Legacy {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "legacy called")) })
    }
}
#[test]
fn unsupported_operation_never_constructs_legacy_send() {
    let count = Arc::new(AtomicUsize::new(0));
    let e = futures::executor::block_on(Legacy(count.clone()).send_no_redirect(HttpRequest::get("https://example.invalid"))).unwrap_err();
    assert_eq!(e.kind, ErrorKind::UnsupportedInput);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

//! The real native streaming no-redirect transport against local authored HTTP, not ElevenLabs.
// This module and its bounded worker helpers compile only under cfg(test).
#![allow(clippy::unwrap_used, clippy::panic)]
use super::stream_tests::{Never, Still, job, previewing, previews, speech};
use super::*;
use crate::http::ChunkSink;
use crate::{HttpRequest, ReqwestHttp, run_to_completion};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

/// Sends the adapter's requests to a local server instead of ElevenLabs.
struct Local {
    origin: String,
    http: ReqwestHttp,
}
impl Http for Local {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("no following redirects")
    }
    fn send_streaming_no_redirect<'a>(&'a self, mut r: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        assert!(r.url.starts_with(API), "{}", r.url);
        r.url = r.url.replacen(API, &self.origin, 1);
        r.timeout_ms = 15_000;
        self.http.send_streaming_no_redirect(r, on_chunk)
    }
}

/// Answers one request with `head` and then `body` in pieces, returning what was asked.
fn serve(listener: TcpListener, head: Vec<u8>, body: Vec<u8>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        let mut socket = loop {
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end => thread::sleep(Duration::from_millis(5)),
                _ => panic!("bounded listener accept"),
            }
        };
        // Accepted streams may inherit nonblocking mode on macOS. Normalize before I/O.
        socket.set_nonblocking(false).unwrap();
        socket.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut asked = Vec::new();
        loop {
            let mut chunk = [0; 1024];
            let n = socket.read(&mut chunk).unwrap();
            assert!(n > 0);
            asked.extend_from_slice(&chunk[..n]);
            assert!(asked.len() < 16384);
            if let Some(pos) = asked.windows(4).position(|v| v == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&asked[..pos]).to_lowercase();
                let length = header.lines().find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())).unwrap();
                if asked.len() >= pos + 4 + length {
                    break;
                }
            }
        }
        socket.write_all(&head).unwrap();
        for piece in body.chunks(32 * 1024) {
            socket.write_all(piece).unwrap();
            socket.flush().unwrap();
            thread::sleep(Duration::from_millis(2));
        }
        asked
    })
}

#[tokio::test(flavor = "current_thread")]
async fn real_native_streaming_refuses_redirects_and_streams_the_whole_mp3() {
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    destination.set_nonblocking(true).unwrap();
    let target = destination.local_addr().unwrap();
    let audio = speech(256 * 1024);
    for redirect in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (head, body) = if redirect {
            (
                format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes(),
                Vec::new(),
            )
        } else {
            (format!("HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", audio.len()).into_bytes(), audio.clone())
        };
        let thread = serve(listener, head, body);
        let p = ElevenLabs::new(Secret::new("fixture-secret-not-real").unwrap(), Arc::new(Local { origin, http: ReqwestHttp::new().unwrap() }));
        let mut updates = Vec::new();
        let result = run_to_completion(&p, &job(V3), &Never, &Still, previewing(), |u| updates.push(u)).await;
        let asked = String::from_utf8_lossy(&thread.join().unwrap()).to_lowercase();
        assert!(asked.starts_with("post /v1/text-to-speech/exavitqu4vr4xnsdxmal/stream?output_format=mp3_44100_128 "), "{asked}");
        assert!(asked.contains("xi-api-key: fixture-secret-not-real"));
        assert!(asked.contains("user-agent: nodecraft/"));
        if redirect {
            assert_eq!(result.unwrap_err().error.kind, ErrorKind::Malformed);
            assert!(updates.is_empty());
        } else {
            let done = result.unwrap();
            let [Output::Bytes { bytes, .. }] = done.outputs.as_slice() else { panic!("one MP3") };
            assert_eq!(&bytes[..], &audio[..]);
            // However the pieces arrive, every preview is the audio so far.
            assert!(previews(&updates).iter().all(|p| audio.starts_with(p) && p.len() >= PREVIEW_FIRST_BYTES));
        }
        assert_eq!(destination.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }
}

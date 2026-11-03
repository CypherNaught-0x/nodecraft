//! The URL import policy and fetch loop, against a scripted transport: no test reaches the network.
use super::*;
use nodecraft_providers::{BoxFuture, ChunkSink, HttpResponse, ProviderError};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR rest of an image";
const PDF: &[u8] = b"%PDF-1.7\n%rest of a document";

/// One scripted reply: status, headers, and the body in pieces.
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    chunks: Vec<Vec<u8>>,
}

fn ok(content_type: &str, body: &[u8]) -> Reply {
    Reply { status: 200, headers: vec![("content-type".into(), content_type.into())], chunks: vec![body.to_vec()] }
}

fn moved(to: &str) -> Reply {
    Reply { status: 302, headers: vec![("location".into(), to.into())], chunks: vec![] }
}

#[derive(Default)]
struct Script {
    replies: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<HttpRequest>>,
    /// Pieces handed to the sink so far.
    delivered: Mutex<usize>,
    /// Set after the first piece, as a person pressing Cancel would.
    cancel_after_first: Option<Arc<AtomicBool>>,
    /// Set while the second request is answered.
    cancel_on_request: Option<Arc<AtomicBool>>,
}

impl Script {
    fn new(replies: Vec<Reply>) -> Self {
        Self { replies: Mutex::new(replies.into()), ..Self::default() }
    }
    fn urls(&self) -> Vec<String> {
        self.seen.lock().unwrap().iter().map(|r| r.url.clone()).collect()
    }
}

impl Http for Script {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        // The URL import must never use a redirect-following send.
        Box::pin(async { Err(ProviderError::new(nodecraft_providers::ErrorKind::Network, "redirect-following send used")) })
    }
    fn send_streaming_no_redirect<'a>(&'a self, _: HttpRequest, _: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        // The URL import must never read a redirect's or refusal's body.
        Box::pin(async { Err(ProviderError::new(nodecraft_providers::ErrorKind::Network, "a body-reading send was used")) })
    }
    fn send_streaming_no_redirect_bodiless_errors<'a>(
        &'a self,
        request: HttpRequest,
        on_chunk: &'a mut ChunkSink<'a>,
    ) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        Box::pin(async move {
            let cap = request.max_response_bytes;
            self.seen.lock().unwrap().push(request);
            if let Some(cancel) = &self.cancel_on_request
                && self.seen.lock().unwrap().len() >= 2
            {
                cancel.store(true, Ordering::Release);
            }
            let reply = self.replies.lock().unwrap().pop_front().expect("a scripted reply");
            if !(200..300).contains(&reply.status) {
                // As the real transport: the body isn't read.
                return Ok(HttpResponse { status: reply.status, headers: reply.headers, body: Vec::new() });
            }
            let mut total = 0usize;
            for chunk in &reply.chunks {
                total += chunk.len();
                if total > cap {
                    return Err(ProviderError::new(nodecraft_providers::ErrorKind::Malformed, "the reply is larger than the cap"));
                }
                on_chunk(chunk)?;
                *self.delivered.lock().unwrap() += 1;
                if let Some(cancel) = &self.cancel_after_first {
                    cancel.store(true, Ordering::Release);
                }
            }
            Ok(HttpResponse { status: reply.status, headers: reply.headers, body: Vec::new() })
        })
    }
}

fn run(script: &Script, address: &str) -> Result<Fetched, Error> {
    futures::executor::block_on(fetch(script, address, &AtomicBool::new(false)))
}

#[test]
fn only_public_https_addresses_pass() {
    for good in ["https://cdn.example.com/a.png", "https://93.184.216.34/x.png", "https://example.com:443/a.png", "  https://example.com/a.png  "] {
        assert!(check(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        "example.com/a.png",
        "http://example.com/a.png",
        "ftp://example.com/a.png",
        "file:///etc/passwd",
        "https://user:pass@example.com/a.png",
        "https://token@example.com/a.png",
        "https://example.com:8443/a.png",
        "https://127.0.0.1/a.png",
        "https://10.1.2.3/a.png",
        "https://192.168.0.10/a.png",
        "https://169.254.169.254/latest/meta-data",
        "https://100.64.0.1/a.png",
        "https://[::1]/a.png",
        "https://[fe80::1]/a.png",
        "https://[fd00::1]/a.png",
        "https://[::ffff:127.0.0.1]/a.png",
        "https://localhost/a.png",
        "https://printer.local/a.png",
        "https://nas.lan/a.png",
        "https://metadata.google.internal/a.png",
        "https://intranet/a.png",
        "https://0x7f000001/a.png",
        "https://2130706433/a.png",
        "https://0.0.0.0/a.png",
        "https://[64:ff9b::7f00:1]/a.png",
        "https://[2002:7f00:1::]/a.png",
        "https://[::ffff:10.0.0.1]/a.png",
        "https://[::]/a.png",
    ] {
        assert!(check(bad).is_err(), "{bad}");
    }
    assert!(check(&format!("https://example.com/{}", "a".repeat(MAX_URL_CHARS))).is_err(), "too long");
}

#[test]
fn the_file_name_is_the_last_segment_cleaned_and_never_the_query() {
    let name = |text: &str| check(text).unwrap().file_name();
    assert_eq!(name("https://cdn.example.com/photos/My%20Beach.png?sig=secret#x"), "My Beach.png");
    assert_eq!(name("https://cdn.example.com/photos/beach.png/"), "beach.png");
    assert_eq!(name("https://cdn.example.com/a/..%2F..%2Fsecret%2Fphoto.jpg"), "photo.jpg");
    assert_eq!(name("https://cdn.example.com/%E2%9C%93%01.png"), "✓.png");
    assert_eq!(name("https://cdn.example.com/"), "Download from cdn.example.com");
    assert_eq!(name("https://cdn.example.com/%ZZ"), "%ZZ", "a stray escape stays as it is");
}

#[test]
fn redirects_are_followed_three_times_and_each_target_is_checked() {
    let script = Script::new(vec![moved("https://b.example.com/two"), moved("/three"), moved("https://c.example.net/final.png"), ok("image/png", PNG)]);
    let fetched = run(&script, "https://a.example.com/shared/photo.png?token=abc").unwrap();
    assert_eq!(fetched.name, "photo.png", "named after the address the person gave");
    assert_eq!(fetched.host, "a.example.com");
    assert_eq!(fetched.kind, Modality::Image);
    assert_eq!(fetched.bytes, PNG);
    assert_eq!(
        script.urls(),
        ["https://a.example.com/shared/photo.png?token=abc", "https://b.example.com/two", "https://b.example.com/three", "https://c.example.net/final.png"]
    );

    let four = Script::new(vec![moved("/1"), moved("/2"), moved("/3"), moved("/4"), ok("image/png", PNG)]);
    assert!(run(&four, "https://a.example.com/x.png").unwrap_err().message.contains("more than 3"));
    assert_eq!(four.urls().len(), 4, "the fourth redirect isn't followed");

    for target in [
        "http://a.example.com/x.png",
        "https://192.168.1.1/x.png",
        "https://[::ffff:10.0.0.1]/x.png",
        "https://[::1]/x.png",
        "https://localhost/x.png",
        "https://a.example.com:8080/x",
    ] {
        let script = Script::new(vec![moved(target), ok("image/png", PNG)]);
        let error = run(&script, "https://a.example.com/x.png").unwrap_err();
        assert!(error.message.starts_with("the site redirected somewhere that isn't imported"), "{target}: {}", error.message);
        assert_eq!(script.urls().len(), 1, "{target} is never requested");
    }
    let nowhere = Script::new(vec![Reply { status: 301, headers: vec![], chunks: vec![] }]);
    assert!(run(&nowhere, "https://a.example.com/x.png").is_err());
}

#[test]
fn requests_carry_no_credentials_and_errors_name_only_the_host() {
    let script = Script::new(vec![Reply { status: 404, headers: vec![], chunks: vec![b"<html>secret page</html>".to_vec()] }]);
    let error = run(&script, "https://files.example.com/private/x.png?sig=s3cr3t").unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
    assert!(error.message.contains("files.example.com"));
    for leaked in ["s3cr3t", "private", "secret page"] {
        assert!(!error.message.contains(leaked), "{}", error.message);
    }
    let seen = script.seen.lock().unwrap();
    let request = seen.first().unwrap();
    let names: Vec<&str> = request.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Accept"], "no cookie, authorization or anything else");
    assert!(request.secret_headers.is_empty() && request.body.is_empty());
    assert_eq!((request.max_response_bytes, request.timeout_ms), (MAX_BYTES, HOP_TIMEOUT_MS));
    drop(seen);

    let signed_in = Script::new(vec![Reply { status: 403, headers: vec![], chunks: vec![] }]);
    assert!(run(&signed_in, "https://files.example.com/x.png").unwrap_err().message.contains("sign-in"));
}

#[test]
fn the_byte_cap_stops_the_stream() {
    let piece = vec![0u8; 1024 * 1024];
    let mut chunks = vec![PNG.to_vec()];
    chunks.extend(std::iter::repeat_n(piece, 20));
    let script = Script::new(vec![Reply { status: 200, headers: vec![("content-type".into(), "image/png".into())], chunks }]);
    let error = run(&script, "https://a.example.com/huge.png").unwrap_err();
    assert!(error.message.contains("larger than 16 MiB"), "{}", error.message);
    assert!(*script.delivered.lock().unwrap() <= 17, "reading stopped at the cap, not at the end");
}

#[test]
fn a_cancel_stops_the_download_between_pieces() {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut script = Script::new(vec![Reply { status: 200, headers: vec![], chunks: vec![PNG.to_vec(), vec![1; 10], vec![2; 10]] }]);
    script.cancel_after_first = Some(Arc::clone(&cancel));
    let error = futures::executor::block_on(fetch(&script, "https://a.example.com/x.png", &cancel)).unwrap_err();
    assert_eq!(error.message, "the download was cancelled");
    assert_eq!(*script.delivered.lock().unwrap(), 1);

    let early = AtomicBool::new(true);
    let untouched = Script::new(vec![ok("image/png", PNG)]);
    assert!(futures::executor::block_on(fetch(&untouched, "https://a.example.com/x.png", &early)).is_err());
    assert!(untouched.urls().is_empty(), "nothing is requested once cancelled");
}

#[test]
fn the_type_and_the_bytes_must_both_say_media() {
    let fetched = |content_type: &str, body: &[u8]| run(&Script::new(vec![ok(content_type, body)]), "https://a.example.com/f");
    assert_eq!(fetched("application/octet-stream", PNG).unwrap().kind, Modality::Image, "a generic type lets the bytes decide");
    assert_eq!(fetched("application/pdf", PDF).unwrap().kind, Modality::Document);
    assert_eq!(fetched("IMAGE/PNG; charset=binary", PNG).unwrap().kind, Modality::Image);
    assert!(fetched("text/html; charset=utf-8", PNG).unwrap_err().message.contains("web page"));
    assert!(fetched("application/json", PNG).is_err());
    assert!(fetched("image/png", PDF).unwrap_err().message.contains("isn't the kind of file the site said"));
    assert!(fetched("application/octet-stream", b"just some words").is_err(), "text is refused");
    assert!(fetched("image/png", b"\x00\x01\x02 not anything").is_err());
    assert!(fetched("image/png", b"").unwrap_err().message.contains("empty"));
    let untyped = Script::new(vec![Reply { status: 200, headers: vec![], chunks: vec![PDF.to_vec()] }]);
    assert_eq!(run(&untyped, "https://a.example.com/doc").unwrap().kind, Modality::Document);
}

#[test]
fn a_large_redirect_body_is_never_read_and_a_cancel_between_hops_stops_the_next() {
    let huge = Reply { status: 302, headers: vec![("location".into(), "/next.png".into())], chunks: vec![vec![0; 20 * 1024 * 1024]] };
    let script = Script::new(vec![huge, ok("image/png", PNG)]);
    let fetched = run(&script, "https://a.example.com/x.png").unwrap();
    assert_eq!(fetched.bytes, PNG, "the redirect was followed though its body was larger than the cap");

    let cancel = Arc::new(AtomicBool::new(false));
    let mut script = Script::new(vec![moved("/two"), moved("/three"), ok("image/png", PNG)]);
    script.cancel_on_request = Some(Arc::clone(&cancel));
    let error = futures::executor::block_on(fetch(&script, "https://a.example.com/x.png", &cancel)).unwrap_err();
    assert_eq!(error.message, "the download was cancelled");
    assert_eq!(script.urls().len(), 2, "no request after the cancel");
}

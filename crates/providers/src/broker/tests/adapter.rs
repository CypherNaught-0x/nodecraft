//! The broker provider against a fake transport: what it sends, where its credential goes, and
//! what a failure after the paid request means.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use futures::executor::block_on;

use super::*;
use crate::http::public_address;
use crate::secrets::Secret;
use crate::{BoxFuture, Http, HttpRequest, HttpResponse, Provider, ProviderError, SubmitContext};

const KEY: &str = "broker-fixture-key-not-real";
const PNG_LINK: &str = "https://cdn.acme.example/out/1.png";

#[derive(Default)]
struct Transport {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
}

impl Transport {
    fn new(replies: Vec<Result<HttpResponse, ProviderError>>) -> Arc<Self> {
        Arc::new(Self { requests: Mutex::new(vec![]), replies: Mutex::new(replies.into()) })
    }
    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("the broker never follows redirects")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(r);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}

fn reply(status: u16, body: Value) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status, headers: vec![], body: serde_json::to_vec(&body).unwrap() })
}

fn png() -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[120; 16], &nodecraft_media::Limits::default()).unwrap()
}

fn file(bytes: Vec<u8>) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: bytes })
}

fn broker(t: Template, http: Arc<Transport>) -> Broker {
    Broker::new("plugin.acme", t, Some(Secret::new(KEY).unwrap()), http).unwrap()
}

fn handle() -> JobHandle {
    JobHandle { remote_id: "j-1".into(), data: json!({"model": "acme/v1"}) }
}

fn carries_key(r: &HttpRequest) -> bool {
    r.headers.iter().any(|(_, v)| v.contains(KEY)) || r.url.contains(KEY)
}

#[test]
fn a_queued_job_runs_on_its_origin_and_its_output_is_fetched_without_the_credential() {
    let http = Transport::new(vec![
        reply(200, json!({"id": "j-1"})),
        reply(200, json!({"status": "running"})),
        reply(200, json!({"status": "succeeded", "output": {"images": [PNG_LINK]}})),
        file(png()),
    ]);
    let b = broker(template(), http.clone());
    let scope = SubmitContext::new();
    let Submission::Queued(h) = block_on(b.submit_with_delivery(&job(), &scope)).unwrap() else { panic!("queued") };
    assert_eq!(h, handle());
    assert!(matches!(block_on(b.poll(&h)).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) = block_on(b.poll(&h)).unwrap() else { panic!("done") };
    let [Output::Bytes { mime, bytes }] = done.outputs.as_slice() else { panic!("{:?}", done.outputs) };
    assert_eq!((mime.as_str(), bytes.as_ref()), ("image/png", png().as_slice()));

    let sent = http.requests();
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[0].url, "https://api.acme.example/v1/models/acme/v1/jobs");
    assert!(sent[0].headers.iter().any(|(n, v)| n.eq_ignore_ascii_case("authorization") && v == &format!("Bearer {KEY}")));
    assert!(sent[0].headers.iter().any(|(n, v)| n.eq_ignore_ascii_case("idempotency-key") && v == "nc-abc123"));
    assert!(sent[1..3].iter().all(|r| r.url == "https://api.acme.example/v1/jobs/j-1" && carries_key(r)));
    assert_eq!(sent[3].url, PNG_LINK);
    assert!(!carries_key(&sent[3]), "an output origin never sees the credential");
    assert_eq!(sent[3].max_response_bytes, crate::broker::adapter::MAX_OUTPUT_BYTES);
    assert_eq!(b.poll_interval_ms(&JobProgress::Queued { position: None }), 2000);
}

#[test]
fn a_job_refused_before_sending_proves_nothing_was_billed() {
    let http = Transport::new(vec![]);
    let scope = SubmitContext::new();
    let other = ProviderJob { model: "acme/v2".into(), ..job() };
    assert_eq!(block_on(broker(template(), http.clone()).submit_with_delivery(&other, &scope)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert!(scope.finish_error().is_some(), "nothing was sent");
    assert!(http.requests().is_empty());
}

#[test]
fn every_failure_after_the_paid_request_may_have_billed() {
    for (n, answer) in [
        Err(ProviderError::new(ErrorKind::Timeout, "lost")),
        reply(500, json!({"error": "boom"})),
        reply(400, json!({"error": "bad prompt"})),
        reply(200, json!({"no": "id"})),
        Ok(HttpResponse { status: 200, headers: vec![], body: b"<html>".to_vec() }),
    ]
    .into_iter()
    .enumerate()
    {
        let http = Transport::new(vec![answer]);
        let scope = SubmitContext::new();
        let error = block_on(broker(template(), http.clone()).submit_with_delivery(&job(), &scope)).unwrap_err();
        assert!(error.message.contains("billed"), "case {n}: {}", error.message);
        assert!(scope.finish_error().is_none(), "case {n}: an unknown service has no documented refusal");
        assert_eq!(http.requests().len(), 1, "case {n}: sent once");
    }
}

#[test]
fn an_ambiguous_submit_is_retried_only_under_an_idempotency_key() {
    assert!(broker(template(), Transport::new(vec![])).retry_ambiguous_submit());
    let plain = decode(&without("/idempotency")).unwrap();
    assert!(!broker(plain, Transport::new(vec![])).retry_ambiguous_submit());
}

#[test]
fn a_redirect_is_never_followed() {
    let to_elsewhere = Ok(HttpResponse { status: 307, headers: vec![("location".into(), "https://evil.example/steal".into())], body: vec![] });
    let http = Transport::new(vec![to_elsewhere]);
    let error = block_on(broker(template(), http.clone()).submit(&job())).unwrap_err();
    assert!(error.message.contains("redirect"), "{}", error.message);
    assert_eq!(http.requests().len(), 1);

    // Nor on a download: the link's origin is checked, its redirect target couldn't be.
    let moved = Ok(HttpResponse { status: 302, headers: vec![("location".into(), "http://169.254.169.254/".into())], body: vec![] });
    let http = Transport::new(vec![reply(200, json!({"status": "succeeded", "output": {"images": [PNG_LINK]}})), moved]);
    let error = block_on(broker(template(), http.clone()).poll(&handle())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Server, "billed, so retryable: {}", error.message);
    assert_eq!(http.requests().len(), 2);
}

#[test]
fn a_reply_cannot_send_the_host_or_the_credential_elsewhere() {
    // An output link off the template's origins, or one with a user name, is never fetched.
    for link in ["https://evil.example/x.png", "https://user@cdn.acme.example/x.png", "http://cdn.acme.example/x.png"] {
        let http = Transport::new(vec![reply(200, json!({"status": "succeeded", "output": {"images": [link]}}))]);
        assert!(block_on(broker(template(), http.clone()).poll(&handle())).is_err(), "{link}");
        assert_eq!(http.requests().len(), 1, "{link}: nothing fetched");
    }
    // A job id or output carrying the key is refused; it would land in the document.
    let http = Transport::new(vec![reply(200, json!({"id": format!("x{KEY}")}))]);
    assert!(block_on(broker(template(), http).submit(&job())).is_err());
    let leaky = JobHandle { remote_id: KEY.into(), ..handle() };
    let http = Transport::new(vec![]);
    assert!(block_on(broker(template(), http.clone()).poll(&leaky)).is_err());
    assert!(http.requests().is_empty());
    let http = Transport::new(vec![reply(200, json!({"status": "succeeded", "output": {"images": [format!("{PNG_LINK}?k={KEY}")]}}))]);
    assert!(block_on(broker(template(), http.clone()).poll(&handle())).is_err());
    assert_eq!(http.requests().len(), 1);
}

#[test]
fn errors_never_echo_the_credential() {
    let echo = |status| reply(status, json!({"error": format!("bad key {KEY}")}));
    let error = block_on(broker(template(), Transport::new(vec![echo(401)])).submit(&job())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Auth);
    assert!(!error.message.contains(KEY), "{}", error.message);
    // Not even a cut-off prefix of it: an echoing body isn't quoted.
    let long = reply(500, json!({"error": format!("{}{KEY}", "x".repeat(280))}));
    let error = block_on(broker(template(), Transport::new(vec![long])).submit(&job())).unwrap_err();
    assert!(!error.message.contains(&KEY[..8]), "{}", error.message);
    let error = block_on(broker(template(), Transport::new(vec![echo(500)])).poll(&handle())).unwrap_err();
    assert!(!error.message.contains(KEY), "{}", error.message);
    let failed = reply(200, json!({"status": "failed", "error": {"message": format!("no: {KEY}")}}));
    let JobProgress::Failed(e) = block_on(broker(template(), Transport::new(vec![failed])).poll(&handle())).unwrap() else { panic!("failed") };
    assert!(!e.message.contains(KEY), "{}", e.message);
    // A failure message with the key where an excerpt would cut it isn't quoted at all.
    let cut = reply(200, json!({"status": "failed", "error": {"message": format!("{}{KEY}", "x".repeat(290))}}));
    let JobProgress::Failed(e) = block_on(broker(template(), Transport::new(vec![cut])).poll(&handle())).unwrap() else { panic!("failed") };
    assert!(!e.message.contains(&KEY[..8]), "{}", e.message);
    // Nor is an output that isn't media: a link on the API origin may serve anything.
    let api_link = "https://api.acme.example/out/1.png";
    let done = reply(200, json!({"status": "succeeded", "output": {"images": [api_link]}}));
    let echoed = file(format!("{}{KEY}", "x".repeat(290)).into_bytes());
    let error = block_on(broker(template(), Transport::new(vec![done, echoed])).poll(&handle())).unwrap_err();
    assert!(!error.message.contains(&KEY[..8]) && !error.message.contains("xxx"), "{}", error.message);
    assert_eq!(error.kind, ErrorKind::Malformed, "{}", error.message);
    let done = reply(200, json!({"status": "succeeded", "output": {"images": [api_link]}}));
    let junk = file(vec![0x01; 400]);
    let error = block_on(broker(template(), Transport::new(vec![done, junk])).poll(&handle())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Malformed, "an output that isn't media won't be on the next try: {}", error.message);
    assert!(error.message.contains("not a known media type"), "{}", error.message);
    let lost = Err(ProviderError::new(ErrorKind::Network, format!("reset by {KEY}")));
    let error = block_on(broker(template(), Transport::new(vec![lost])).submit(&job())).unwrap_err();
    assert!(!error.message.contains(KEY), "{}", error.message);
}

#[test]
fn the_quote_is_the_templates_price_and_asks_no_one() {
    let http = Transport::new(vec![]);
    let b = broker(template(), http.clone());
    assert_eq!(block_on(b.quote(&job())).unwrap().micros, 40_000);
    assert!(block_on(b.quote(&ProviderJob { model: "acme/v2".into(), ..job() })).is_err());
    assert!(http.requests().is_empty());
}

#[test]
fn a_credential_the_template_needs_must_be_given_and_one_it_doesnt_is_dropped() {
    let error = Broker::new("plugin.acme", template(), None, Transport::new(vec![])).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Auth);

    let open = decode(&without("/credential")).unwrap();
    let http = Transport::new(vec![reply(200, json!({"id": "j-1"}))]);
    block_on(broker(open, http.clone()).submit(&job())).unwrap();
    assert!(!carries_key(&http.requests()[0]));
}

#[test]
fn cancel_goes_to_the_api_origin_or_says_it_cannot() {
    let http = Transport::new(vec![reply(200, json!({}))]);
    block_on(broker(template(), http.clone()).cancel(&handle())).unwrap();
    let sent = http.requests();
    assert_eq!((sent[0].method, sent[0].url.as_str()), (HttpMethod::Post, "https://api.acme.example/v1/jobs/j-1/cancel"));
    assert!(carries_key(&sent[0]));

    let http = Transport::new(vec![]);
    let error = block_on(broker(decode(&without("/cancel")).unwrap(), http.clone()).cancel(&handle())).unwrap_err();
    assert!(error.message.contains("billed"), "{}", error.message);
    assert!(http.requests().is_empty());
}

#[test]
fn only_public_unicast_addresses_are_reachable() {
    let public = ["93.184.215.14", "1.1.1.1", "2606:4700:4700::1111", "2002:5db8:d70e::1", "::ffff:93.184.215.14", "64:ff9b::5db8:d70e"];
    for a in public {
        assert!(public_address(a.parse::<IpAddr>().unwrap()), "{a}");
    }
    let private = [
        "0.0.0.0",
        "10.0.0.1",
        "127.0.0.1",
        "100.64.0.1",
        "169.254.169.254",
        "172.16.0.1",
        "172.31.255.255",
        "192.0.0.8",
        "192.0.2.1",
        "192.168.1.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "fc00::1",
        "fd12::1",
        "fe80::1",
        "fec0::1",
        "ff02::1",
        "::ffff:127.0.0.1",
        "::ffff:169.254.169.254",
        "64:ff9b::7f00:1",
        "64:ff9b:1::5db8:d70e",
        "2002:7f00:1::1",
        "2002:a00:1::1",
        "2001::1",
        "2001:db8::1",
        "3fff::1",
        "::127.0.0.1",
    ];
    for a in private {
        assert!(!public_address(a.parse::<IpAddr>().unwrap()), "{a}");
    }
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[test]
fn a_public_only_transport_never_connects_to_a_local_address() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    // `localhost` resolves to the loopback address without a network.
    let request = || HttpRequest::get(format!("http://localhost:{port}/x")).timeout_ms(5_000);
    let error = rt.block_on(crate::ReqwestHttp::public_only().unwrap().send_no_redirect(request())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network, "{error:?}");
    assert!(listener.accept().is_err(), "nothing reached the local listener");
    // An address literal never reaches the resolver, so it is checked on its own.
    for url in [format!("http://127.0.0.1:{port}/x"), format!("http://[::ffff:127.0.0.1]:{port}/x")] {
        let error = rt.block_on(crate::ReqwestHttp::public_only().unwrap().send_no_redirect(HttpRequest::get(url.clone()).timeout_ms(5_000))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Network, "{url}: {error:?}");
        assert!(listener.accept().is_err(), "{url}: nothing reached the local listener");
    }
    // The ordinary transport does reach it, so the refusal above is the resolver's.
    let _ = rt.block_on(crate::ReqwestHttp::new().unwrap().send_no_redirect(request().timeout_ms(500)));
    assert!(listener.accept().is_ok(), "the control request connected");
}

#[test]
fn a_withdrawn_grant_sends_nothing() {
    let http = Transport::new(vec![]);
    let granted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let held = granted.clone();
    let b = broker(template(), http.clone()).granted(Arc::new(move || held.load(std::sync::atomic::Ordering::Acquire)));
    let scope = SubmitContext::new();
    let e = block_on(b.submit_with_delivery(&job(), &scope)).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth);
    assert!(scope.finish_error().is_some(), "nothing was sent, so nothing was billed");
    assert!(http.requests().is_empty());
}

#[test]
fn a_withdrawn_grant_stops_following_a_job_it_sent() {
    let http = Transport::new(vec![]);
    let b = broker(template(), http.clone()).granted(Arc::new(|| false));
    for error in [block_on(b.poll(&handle())).unwrap_err(), block_on(b.cancel(&handle())).unwrap_err()] {
        assert_eq!(error.kind, ErrorKind::Auth, "{error:?}");
    }
    assert!(http.requests().is_empty(), "nothing reached the service");

    // Withdrawn while the finished job was read: its output isn't fetched.
    let http = Transport::new(vec![reply(200, json!({"status": "succeeded", "output": {"images": [PNG_LINK]}})), file(png())]);
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = asked.clone();
    let b = broker(template(), http.clone()).granted(Arc::new(move || count.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0));
    let error = block_on(b.poll(&handle())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Auth, "{error:?}");
    let sent = http.requests();
    assert_eq!(sent.len(), 1, "only the poll went out");
    assert!(sent.iter().all(|r| r.url != PNG_LINK));
}

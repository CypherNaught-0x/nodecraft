use super::*;
use crate::{HttpMethod, JobInput};
use futures::executor::block_on;
use serde_json::{Value, json};
use std::sync::Mutex;

const ID: &str = "fixture-key-id-0001";
const SECRET: &str = "fixture-secret-not-real-0001";
const REQUEST: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
const PUBLIC: &str = "https://upload.cdn.example.com/inputs/frame.png";
const UPLOAD: &str = "https://upload.storage.example.com/inputs/frame.png?X-Amz-Signature=abc";

fn key() -> Secret {
    Secret::new(&format!("{ID}:{SECRET}")).unwrap()
}
#[derive(Default)]
struct Transport {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<std::collections::VecDeque<Result<HttpResponse, ProviderError>>>,
}
impl Transport {
    fn new(replies: Vec<Result<HttpResponse, ProviderError>>) -> Arc<Self> {
        Arc::new(Self { requests: Mutex::new(vec![]), replies: Mutex::new(replies.into()) })
    }
    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
    fn push(&self, replies: Vec<Result<HttpResponse, ProviderError>>) {
        self.replies.lock().unwrap().extend(replies);
    }
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("legacy send must not run")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(r);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}
fn adapter(t: &Arc<Transport>) -> Higgsfield {
    Higgsfield::new(key(), t.clone())
}
fn ok(v: Value) -> Result<HttpResponse, ProviderError> {
    status(200, v)
}
fn status(code: u16, v: Value) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: code, headers: vec![], body: serde_json::to_vec(&v).unwrap() })
}
fn file(bytes: &[u8]) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: bytes.to_vec() })
}
fn empty(code: u16) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: code, headers: vec![], body: vec![] })
}
fn png() -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[120; 16], &nodecraft_media::Limits::default()).unwrap()
}
fn mp4() -> Vec<u8> {
    let mut b = vec![0, 0, 0, 0x18];
    b.extend_from_slice(b"ftypisom\0\0\x02\0isomiso2");
    b.extend_from_slice(&[0, 0, 0, 8, b'f', b'r', b'e', b'e']);
    b
}
fn job(model: &str, params: Value, inputs: Vec<JobInput>) -> ProviderJob {
    ProviderJob { model: model.into(), params: params.as_object().unwrap().clone(), inputs, idempotency_key: "fixture-operation".into() }
}
fn prompt(text: &str) -> JobInput {
    JobInput::Text { role: "prompt".into(), text: text.into() }
}
fn image() -> JobInput {
    JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(png()) }
}
fn soul(params: Value) -> ProviderJob {
    job(SOUL, params, vec![prompt("a lighthouse at dusk")])
}
fn kling() -> ProviderJob {
    job(KLING_I2V, json!({"duration": "10", "cfg_scale": 0.7}), vec![prompt("the waves roll in"), image()])
}
fn receipt() -> Result<HttpResponse, ProviderError> {
    ok(
        json!({"status": "queued", "request_id": REQUEST, "status_url": format!("{API}/requests/{REQUEST}/status"), "cancel_url": format!("{API}/requests/{REQUEST}/cancel")}),
    )
}
fn grant() -> Result<HttpResponse, ProviderError> {
    ok(
        json!({"public_url": PUBLIC, "upload_url": UPLOAD, "content_type": "image/png", "upload_headers": {"Content-Type": "image/png", "x-amz-tagging": "retention=temporary"}}),
    )
}
fn body(r: &HttpRequest) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}
fn header<'a>(r: &'a HttpRequest, name: &str) -> Option<&'a str> {
    r.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}
fn queued(model: &str, count: u64) -> JobHandle {
    JobHandle { remote_id: REQUEST.into(), data: json!({"v": 1, "model": model, "count": count}) }
}
fn no_secret(e: &ProviderError) {
    assert!(!e.message.contains(SECRET) && !e.message.contains(ID), "{e:?}");
}

#[test]
fn soul_submits_explicit_settings_under_the_key_and_the_idempotency_key() {
    let t = Transport::new(vec![receipt()]);
    let job = soul(json!({"aspect_ratio": "16:9", "resolution": "1080p", "batch_size": "4", "seed": 42, "enhance_prompt": true}));
    let Submission::Queued(handle) = block_on(adapter(&t).submit(&job)).unwrap() else { panic!("queued") };
    assert_eq!(handle.remote_id, REQUEST);
    assert_eq!(handle.data, json!({"v": 1, "model": SOUL, "count": 4}));
    let [r] = t.requests().try_into().unwrap();
    assert_eq!((r.method, r.url.as_str()), (HttpMethod::Post, "https://api.higgsfield.ai/higgsfield-ai/soul/v2/standard"));
    assert_eq!(header(&r, "Authorization"), Some(format!("Key {ID}:{SECRET}").as_str()));
    assert!(r.secret_headers.contains(&"authorization".to_owned()));
    assert_eq!(header(&r, "Idempotency-Key"), Some("fixture-operation"));
    assert_eq!(
        body(&r),
        json!({"prompt": "a lighthouse at dusk", "aspect_ratio": "16:9", "resolution": "1080p", "batch_size": 4, "seed": 42, "enhance_prompt": true})
    );
    // Defaults are sent rather than left to Higgsfield, and no seed means none is sent.
    let t = Transport::new(vec![receipt()]);
    block_on(adapter(&t).submit(&soul(json!({})))).unwrap();
    assert_eq!(
        body(&t.requests()[0]),
        json!({"prompt": "a lighthouse at dusk", "aspect_ratio": "1:1", "resolution": "720p", "batch_size": 1, "enhance_prompt": false})
    );
}

#[test]
fn kling_stages_its_image_without_the_key_then_names_the_public_link() {
    let t = Transport::new(vec![grant(), empty(200), receipt()]);
    block_on(adapter(&t).submit(&kling())).unwrap();
    let [g, put, submit] = t.requests().try_into().unwrap();
    assert_eq!(g.url, "https://api.higgsfield.ai/files/generate-upload-url");
    assert_eq!(body(&g), json!({"content_type": "image/png"}));
    assert!(header(&g, "Authorization").is_some());
    assert_eq!((put.method, put.url.as_str()), (HttpMethod::Put, UPLOAD));
    assert_eq!(header(&put, "Authorization"), None, "storage never sees the key");
    assert_eq!(header(&put, "x-amz-tagging"), Some("retention=temporary"));
    assert_eq!(header(&put, "Content-Type"), Some("image/png"));
    assert_eq!(put.body, png());
    assert_eq!(submit.url, "https://api.higgsfield.ai/kling-video/v2.5-turbo/pro/image-to-video");
    assert_eq!(body(&submit), json!({"prompt": "the waves roll in", "image_url": PUBLIC, "duration": 10, "cfg_scale": 0.7, "negative_prompt": ""}));
}

#[test]
fn a_lost_reply_is_replayed_byte_for_byte_without_staging_again() {
    let t = Transport::new(vec![grant(), empty(200), Err(ProviderError::new(ErrorKind::Network, "reset"))]);
    let p = adapter(&t);
    let e = block_on(p.submit(&kling())).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Network);
    assert!(e.message.contains("may have been accepted and billed"));
    t.push(vec![receipt()]);
    block_on(p.submit(&kling())).unwrap();
    let requests = t.requests();
    assert_eq!(requests.len(), 4, "one grant, one upload, two submits");
    let (first, second) = (&requests[2], &requests[3]);
    assert_eq!(first.body, second.body);
    assert_eq!(header(first, "Idempotency-Key"), header(second, "Idempotency-Key"));
    // Another job under the same key is staged afresh, never sent with the old body.
    t.push(vec![grant(), empty(200), receipt()]);
    let mut other = kling();
    other.inputs[0] = prompt("the tide goes out");
    block_on(p.submit(&other)).unwrap();
    assert_eq!(t.requests().len(), 7);
    assert_eq!(body(&t.requests()[6])["prompt"], "the tide goes out");
}

#[test]
fn an_estimate_quotes_the_body_a_later_submit_sends() {
    let t = Transport::new(vec![grant(), empty(200), ok(json!({"credits": "12.5", "usd": "0.0312504"}))]);
    let p = adapter(&t);
    let quote = block_on(p.estimate(&kling())).unwrap();
    assert_eq!(quote.credits, "12.5");
    assert_eq!(quote.usd, nodecraft_types::Money::new(nodecraft_types::Currency::USD, 31_251), "a fraction of a micro rounds up");
    t.push(vec![receipt()]);
    block_on(p.submit(&kling())).unwrap();
    let r = t.requests();
    assert_eq!(r.len(), 4, "the submit reuses the staged upload");
    assert_eq!(r[2].url, "https://api.higgsfield.ai/estimate/kling-video/v2.5-turbo/pro/image-to-video");
    assert_eq!(header(&r[2], "Idempotency-Key"), None);
    assert_eq!(r[2].body, r[3].body);
    // The trait quote uses no key; the run draws its seeds and keys later, so only its files
    // can be reused, and each by one submit.
    let t = Transport::new(vec![grant(), empty(200), ok(json!({"credits": "1", "usd": "0.5"})), ok(json!({"credits": "1", "usd": "0.5"}))]);
    let p = adapter(&t);
    let unkeyed = ProviderJob { idempotency_key: String::new(), ..kling() };
    assert_eq!(block_on(Provider::quote(&p, &unkeyed)).unwrap(), nodecraft_types::Money::new(nodecraft_types::Currency::USD, 500_000));
    block_on(p.estimate(&unkeyed)).unwrap();
    assert_eq!(t.requests().len(), 4, "a second estimate reuses the upload");
    t.push(vec![receipt(), grant(), empty(200), receipt()]);
    let mut run = ProviderJob { idempotency_key: "nodecraft:run:1".into(), ..kling() };
    run.params.insert("cfg_scale".into(), json!(0.5));
    block_on(p.submit(&run)).unwrap();
    let r = t.requests();
    assert_eq!(r.len(), 5, "the submit sends the uploaded file without uploading it again");
    assert_eq!(header(&r[4], "Idempotency-Key"), Some("nodecraft:run:1"));
    assert_eq!(body(&r[4])["image_url"], body(&r[3])["image_url"]);
    assert_eq!(body(&r[4])["cfg_scale"], 0.5);
    block_on(p.submit(&ProviderJob { idempotency_key: "nodecraft:run:2".into(), ..kling() })).unwrap();
    assert_eq!(t.requests().len(), 8, "the next submit uploads afresh");
    for bad in [
        json!({"credits": "1", "usd": "-1"}),
        json!({"credits": "1", "usd": 1.5}),
        json!({"credits": "1.", "usd": "1"}),
        json!({"usd": "1"}),
        json!({"credits": "1", "usd": "1e3"}),
    ] {
        let t = Transport::new(vec![receipt_free_estimate(bad)]);
        assert_eq!(block_on(adapter(&t).estimate(&soul(json!({})))).unwrap_err().kind, ErrorKind::Malformed);
    }
}
fn receipt_free_estimate(v: Value) -> Result<HttpResponse, ProviderError> {
    ok(v)
}

#[test]
fn refusals_are_typed_and_only_documented_ones_prove_nothing_was_accepted() {
    let refusal = |code: u16, detail: Value| HttpResponse { status: code, headers: vec![], body: serde_json::to_vec(&json!({"detail": detail})).unwrap() };
    let busy = refusal(400, json!("Maximum number of concurrent requests (4) has been reached"));
    let e = response::check(&busy, Limits::HARD, |_| false).unwrap_err();
    assert_eq!((e.kind, e.retry_after_ms), (ErrorKind::RateLimited, Some(15_000)));
    assert!(response::refused_submit(&busy));
    for (code, kind) in [
        (401, ErrorKind::Auth),
        (403, ErrorKind::Budget),
        (404, ErrorKind::NotFound),
        (423, ErrorKind::Busy),
        (503, ErrorKind::Busy),
        (422, ErrorKind::UnsupportedInput),
    ] {
        let r = refusal(code, json!([{"loc": ["body", "prompt"], "msg": "field required"}]));
        assert_eq!(response::check(&r, Limits::HARD, |_| false).unwrap_err().kind, kind, "{code}");
        assert!(response::refused_submit(&r), "{code}");
    }
    // A key used before with another body: maybe billed, never a refusal and never retried.
    let reused = refusal(422, json!("Idempotency key was already used with different parameters"));
    let e = response::check(&reused, Limits::HARD, |_| false).unwrap_err();
    assert!(!e.is_transient() && e.message.contains("may have been billed"), "{e:?}");
    assert!(!response::refused_submit(&reused));
    let reworded = refusal(422, json!("Key has already been used with different parameters"));
    assert!(!response::refused_submit(&reworded));
    assert!(response::check(&reworded, Limits::HARD, |_| false).unwrap_err().message.contains("may have been billed"));
    // 5xx and unreadable bodies stay possibly billed.
    assert!(!response::refused_submit(&refusal(500, json!("oops"))));
    assert!(!response::refused_submit(&HttpResponse { status: 400, headers: vec![], body: b"<html>".to_vec() }));
    // A message echoing the credential is withheld.
    let t = Transport::new(vec![status(400, json!({"detail": format!("bad key {ID}:{SECRET}")}))]);
    let e = block_on(adapter(&t).submit(&soul(json!({})))).unwrap_err();
    assert!(e.message.contains("withheld"));
    no_secret(&e);
}

#[test]
fn a_malformed_credential_sends_nothing() {
    for bad in ["only-a-secret", "id:", ":secret", "a:b:c"] {
        let t = Transport::new(vec![]);
        let p = Higgsfield::new(Secret::new(bad).unwrap(), t.clone());
        let e = block_on(p.submit(&soul(json!({})))).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Auth, "{bad}");
        assert!(!e.message.contains(bad));
        assert!(t.requests().is_empty());
    }
}

#[test]
fn hostile_upload_grants_are_refused_before_any_bytes_move() {
    let cases = [
        json!({"public_url": PUBLIC, "upload_url": UPLOAD, "content_type": "image/jpeg"}),
        json!({"public_url": PUBLIC, "upload_url": "http://upload.storage.example.com/x", "content_type": "image/png"}),
        json!({"public_url": "https://127.0.0.1/x", "upload_url": UPLOAD, "content_type": "image/png"}),
        json!({"public_url": PUBLIC, "upload_url": UPLOAD, "content_type": "image/png", "upload_headers": {"Authorization": "Bearer x"}}),
        json!({"public_url": PUBLIC, "upload_url": UPLOAD, "content_type": "image/png", "upload_headers": {"Content-Type": "text/html"}}),
        json!({"public_url": PUBLIC, "upload_url": format!("https://upload.storage.example.com/{SECRET}"), "content_type": "image/png"}),
        json!({"public_url": PUBLIC, "upload_url": UPLOAD, "content_type": "image/png", "upload_headers": ["x"]}),
    ];
    for case in cases {
        let t = Transport::new(vec![ok(case.clone())]);
        let e = block_on(adapter(&t).submit(&kling())).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Malformed, "{case}");
        no_secret(&e);
        assert_eq!(t.requests().len(), 1, "{case}");
    }
    // A failed upload proves nothing was billed and can be retried.
    let t = Transport::new(vec![grant(), empty(503)]);
    let e = block_on(adapter(&t).submit(&kling())).unwrap_err();
    assert!(e.is_transient() && e.message.contains("nothing was billed"));
}

#[test]
fn status_maps_every_state_and_downloads_every_result() {
    let s = |v: Value| {
        ok(json!({"request_id": REQUEST})
            .as_object()
            .unwrap()
            .clone()
            .into_iter()
            .chain(v.as_object().unwrap().clone())
            .collect::<serde_json::Map<_, _>>()
            .into())
    };
    let t = Transport::new(vec![s(json!({"status": "queued"})), s(json!({"status": "in_progress"}))]);
    let p = adapter(&t);
    assert_eq!(block_on(p.poll(&queued(SOUL, 4))).unwrap(), JobProgress::Queued { position: None });
    assert!(matches!(block_on(p.poll(&queued(SOUL, 4))).unwrap(), JobProgress::Running { .. }));
    assert!(t.requests().iter().all(|r| r.url == format!("https://api.higgsfield.ai/requests/{REQUEST}/status") && header(r, "Authorization").is_some()));

    let images = json!({"status": "completed", "images": [{"url": "https://cdn.example.com/a.png"}, {"url": "https://cdn.example.com/b.png"}]});
    let t = Transport::new(vec![s(images), file(&png()), file(&png())]);
    let JobProgress::Done(done) = block_on(adapter(&t).poll(&queued(SOUL, 4))).unwrap() else { panic!("done") };
    assert_eq!(done.outputs.len(), 2);
    assert!(matches!(&done.outputs[0], Output::Bytes { mime, .. } if mime == "image/png"));
    assert!(t.requests()[1..].iter().all(|r| header(r, "Authorization").is_none()), "results download without the key");

    let video = json!({"status": "completed", "video": {"url": "https://cdn.example.com/clip.mp4"}});
    let t = Transport::new(vec![s(video), file(&mp4())]);
    let JobProgress::Done(done) = block_on(adapter(&t).poll(&queued(KLING_I2V, 1))).unwrap() else { panic!("done") };
    assert!(matches!(&done.outputs[..], [Output::Bytes { mime, .. }] if mime == "video/mp4"));

    for (state, kind) in [("failed", ErrorKind::JobFailed), ("nsfw", ErrorKind::BlockedContent), ("canceled", ErrorKind::Cancelled)] {
        let t = Transport::new(vec![s(json!({"status": state, "error": format!("leaked {SECRET}")}))]);
        let JobProgress::Failed(e) = block_on(adapter(&t).poll(&queued(SOUL, 1))).unwrap() else { panic!("{state}") };
        assert_eq!(e.kind, kind);
        no_secret(&e);
    }
}

#[test]
fn hostile_status_replies_are_errors() {
    let cases = [
        json!({"request_id": "another", "status": "queued"}),
        json!({"request_id": REQUEST, "status": "teleported"}),
        json!({"request_id": REQUEST, "status": "completed", "images": []}),
        json!({"request_id": REQUEST, "status": "completed", "images": [{"url": "https://cdn.example.com/a.png"}, {"url": "https://cdn.example.com/b.png"}]}),
        json!({"request_id": REQUEST, "status": "completed", "images": [{"url": "http://cdn.example.com/a.png"}]}),
        json!({"request_id": REQUEST, "status": "completed", "images": [{"url": "https://localhost/a.png"}]}),
        json!({"request_id": REQUEST, "status": "completed", "video": {"url": "https://cdn.example.com/a.mp4"}}),
    ];
    for case in cases {
        let t = Transport::new(vec![ok(case.clone())]);
        assert_eq!(block_on(adapter(&t).poll(&queued(SOUL, 1))).unwrap_err().kind, ErrorKind::Malformed, "{case}");
    }
    // A link the reply names is never followed: the key goes only to the adapter's own URL.
    let t = Transport::new(vec![ok(json!({"request_id": REQUEST, "status": "queued", "status_url": "https://evil.example.com/status"}))]);
    assert_eq!(block_on(adapter(&t).poll(&queued(SOUL, 1))).unwrap(), JobProgress::Queued { position: None });
    assert_eq!(t.requests()[0].url, format!("https://api.higgsfield.ai/requests/{REQUEST}/status"));
    // The wrong kind of media, or a failed fetch, never completes the job; a failed fetch retries.
    let done = json!({"request_id": REQUEST, "status": "completed", "images": [{"url": "https://cdn.example.com/a.png"}]});
    let t = Transport::new(vec![ok(done.clone()), file(&mp4())]);
    assert_eq!(block_on(adapter(&t).poll(&queued(SOUL, 1))).unwrap_err().kind, ErrorKind::Malformed);
    let t = Transport::new(vec![ok(done), empty(403)]);
    let e = block_on(adapter(&t).poll(&queued(SOUL, 1))).unwrap_err();
    assert!(e.is_transient() && e.message.contains("billed"));
    // Handles that don't describe a request this adapter made are refused before any request.
    for handle in [
        JobHandle { remote_id: "../files".into(), data: json!({"v": 1, "model": SOUL, "count": 1}) },
        JobHandle { remote_id: REQUEST.into(), data: json!({"v": 2, "model": SOUL, "count": 1}) },
        JobHandle { remote_id: REQUEST.into(), data: json!({"v": 1, "model": "other", "count": 1}) },
        JobHandle { remote_id: REQUEST.into(), data: json!({"v": 1, "model": KLING_I2V, "count": 4}) },
        JobHandle { remote_id: REQUEST.into(), data: json!({"v": 1, "model": SOUL, "count": 2}) },
    ] {
        let t = Transport::new(vec![]);
        assert!(block_on(adapter(&t).poll(&handle)).is_err());
        assert!(t.requests().is_empty());
    }
}

#[test]
fn cancel_works_only_while_queued() {
    let t = Transport::new(vec![
        empty(202),
        status(400, json!({"detail": "Request is already processing"})),
        status(400, json!({"detail": "Maximum number of concurrent requests (4) has been reached"})),
        empty(400),
    ]);
    let p = adapter(&t);
    block_on(p.cancel(&queued(SOUL, 1))).unwrap();
    let e = block_on(p.cancel(&queued(SOUL, 1))).unwrap_err();
    assert!(e.message.contains("already started") && !e.is_transient());
    for _ in 0..2 {
        assert!(!block_on(p.cancel(&queued(SOUL, 1))).unwrap_err().message.contains("already started"));
    }
    let r = t.requests();
    assert_eq!((r[0].method, r[0].url.as_str()), (HttpMethod::Post, "https://api.higgsfield.ai/requests/3fa85f64-5717-4562-b3fc-2c963f66afa6/cancel"));
}

#[test]
fn jobs_are_checked_before_anything_is_sent() {
    let bad = [
        soul(json!({"batch_size": 2})),
        soul(json!({"seed": 0})),
        soul(json!({"seed": 1_000_001})),
        soul(json!({"aspect_ratio": "21:9"})),
        soul(json!({"resolution": "4K"})),
        soul(json!({"style_id": "x"})),
        job(SOUL, json!({}), vec![prompt("x"), image()]),
        job(SOUL, json!({}), vec![prompt("   ")]),
        job(SOUL, json!({}), vec![]),
        job(KLING_I2V, json!({}), vec![prompt("x")]),
        job(KLING_I2V, json!({"duration": 7}), vec![prompt("x"), image()]),
        job(KLING_I2V, json!({"cfg_scale": 1.5}), vec![prompt("x"), image()]),
        job(KLING_I2V, json!({}), vec![prompt("x"), JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes: Arc::from(png()) }]),
        job("higgsfield-ai/soul/standard", json!({}), vec![prompt("x")]),
        job(SOUL, json!({}), vec![prompt(&"x".repeat(10_001))]),
    ];
    for j in bad {
        let t = Transport::new(vec![]);
        assert_eq!(block_on(adapter(&t).submit(&j)).unwrap_err().kind, ErrorKind::UnsupportedInput, "{:?}", j.params);
        assert!(t.requests().is_empty());
    }
    assert!(Higgsfield::with_limits(key(), Transport::new(vec![]), Limits { input_bytes: 21 * MIB, ..Limits::HARD }).is_err());
    // A key Higgsfield wouldn't take is replaced by a stable hash of it.
    assert_eq!(request::idempotency_key("ok-key"), "ok-key");
    let long = "k".repeat(300);
    assert_eq!(request::idempotency_key(&long), request::idempotency_key(&long));
    assert_eq!(request::idempotency_key("has space").len(), 64);
}

#[test]
fn the_catalog_and_the_adapter_agree() {
    let catalog = nodecraft_catalog::Catalog::builtin().unwrap();
    for (id, model) in [("higgsfield.soul-2", SOUL), ("higgsfield.kling-2.5-turbo-i2v", KLING_I2V)] {
        let entry = catalog.get(id).unwrap();
        assert_eq!((entry.provider.as_str(), entry.provider_model.as_str()), ("higgsfield", model));
        // Every default the catalog fills in is one the adapter takes.
        let params: serde_json::Map<String, Value> = entry
            .params
            .params
            .iter()
            .filter_map(|p| {
                let v = p.default.as_ref()?;
                Some((
                    p.name.clone(),
                    match v {
                        nodecraft_types::Value::Bool(b) => json!(b),
                        nodecraft_types::Value::Text(s) => json!(s),
                        nodecraft_types::Value::Unsigned(n) => json!(n.0),
                        nodecraft_types::Value::Number(n) => json!(n.get()),
                        other => panic!("{other:?}"),
                    },
                ))
            })
            .collect();
        let inputs = if model == SOUL { vec![prompt("x")] } else { vec![prompt("x"), image()] };
        request::prepare(&job(model, Value::Object(params), inputs), Limits::HARD).unwrap();
    }
    for (id, model) in
        [("higgsfield.genjutsu-motion-transfer", MOTION_TRANSFER), ("higgsfield.genjutsu-object-swap", OBJECT_SWAP), ("higgsfield.genjutsu-restyle", RESTYLE)]
    {
        let entry = catalog.get(id).unwrap();
        assert_eq!((entry.provider.as_str(), entry.provider_model.as_str()), ("higgsfield", model));
        // Every resolution the catalog prices is one the adapter sends.
        let nodecraft_catalog::Pricing::PerSourceSecond { prices, .. } = &entry.pricing else { panic!("priced per source second") };
        for resolution in prices.keys() {
            let params = if model == RESTYLE { json!({"resolution": resolution, "preset_id": PRESET}) } else { json!({"resolution": resolution}) };
            request::prepare(&genjutsu(model, params, vec![source(), image()]), Limits::HARD).unwrap();
        }
    }
    // The preset list the restyle contract names is the one the adapter fetches.
    let restyle = catalog.get("higgsfield.genjutsu-restyle").unwrap();
    let contract = restyle.parameter_contract.as_ref().unwrap();
    assert!(contract.rules.iter().any(|r| r.param == "preset_id" && r.options.as_ref().is_some_and(|o| o.id == RESTYLE_PRESETS)));
    // And SOUL 2's Soul ID list is the one the adapter fetches; its strength is sent only with one.
    let soul = catalog.get("higgsfield.soul-2").unwrap();
    let contract = soul.parameter_contract.as_ref().unwrap();
    assert!(contract.rules.iter().any(|r| r.param == "custom_reference_id" && r.options.as_ref().is_some_and(|o| o.id == SOUL_IDS)));
    let mut values = nodecraft_types::ParamValues::new();
    let sent = soul.effective_params(&values, false).unwrap();
    assert!(!sent.contains_key("custom_reference_strength") && !sent.contains_key("seed"), "{sent:?}");
    values.insert("custom_reference_id".into(), nodecraft_types::Value::Text(SOUL_ID.into()));
    let sent = soul.effective_params(&values, false).unwrap();
    assert!(sent.contains_key("custom_reference_strength"), "{sent:?}");
}

fn source() -> JobInput {
    JobInput::Media { role: "source".into(), mime: "video/mp4".into(), bytes: Arc::from(mp4()) }
}
fn genjutsu(model: &str, params: Value, inputs: Vec<JobInput>) -> ProviderJob {
    job(model, params, inputs)
}
fn video_grant() -> Result<HttpResponse, ProviderError> {
    ok(
        json!({"public_url": "https://upload.cdn.example.com/inputs/source.mp4", "upload_url": UPLOAD, "content_type": "video/mp4", "upload_headers": {"Content-Type": "video/mp4"}}),
    )
}
fn image_grant(n: u32) -> Result<HttpResponse, ProviderError> {
    ok(json!({"public_url": format!("https://upload.cdn.example.com/inputs/ref{n}.png"), "upload_url": UPLOAD, "content_type": "image/png"}))
}

#[test]
fn a_genjutsu_edit_stages_its_source_then_names_the_ordered_references() {
    let second = JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(png()) };
    let t = Transport::new(vec![video_grant(), empty(200), image_grant(1), empty(200), image_grant(2), empty(200), receipt()]);
    let edit = genjutsu(MOTION_TRANSFER, json!({"resolution": "1080p"}), vec![prompt(""), source(), image(), second]);
    block_on(adapter(&t).submit(&edit)).unwrap();
    let requests = t.requests();
    assert_eq!(requests.len(), 7);
    assert_eq!(body(&requests[0]), json!({"content_type": "video/mp4"}));
    assert_eq!(requests[1].body, mp4());
    let submit = &requests[6];
    assert_eq!(submit.url, "https://api.higgsfield.ai/higgsfield/genjutsu/motion-transfer/v1.0");
    // The empty prompt is left out: Genjutsu's is optional.
    assert_eq!(
        body(submit),
        json!({
            "video_url": "https://upload.cdn.example.com/inputs/source.mp4",
            "image_urls": ["https://upload.cdn.example.com/inputs/ref1.png", "https://upload.cdn.example.com/inputs/ref2.png"],
            "resolution": "1080p"
        })
    );
    // The default resolution is sent explicitly, as is a prompt.
    let t = Transport::new(vec![video_grant(), empty(200), image_grant(1), empty(200), receipt()]);
    block_on(adapter(&t).submit(&genjutsu(OBJECT_SWAP, json!({}), vec![prompt("a red umbrella"), source(), image()]))).unwrap();
    let submit = body(&t.requests()[4]);
    assert_eq!((submit["resolution"].as_str(), submit["prompt"].as_str()), (Some("720p"), Some("a red umbrella")));
    // A finished edit downloads its MP4.
    let done = json!({"request_id": REQUEST, "status": "completed", "video": {"url": "https://cdn.example.com/out.mp4"}});
    let t = Transport::new(vec![ok(done), file(&mp4())]);
    let JobProgress::Done(done) = block_on(adapter(&t).poll(&queued(OBJECT_SWAP, 1))).unwrap() else { panic!("done") };
    assert_eq!(done.outputs.len(), 1);
}

#[test]
fn genjutsu_jobs_are_checked_before_anything_is_sent() {
    let mov = JobInput::Media { role: "source".into(), mime: "video/quicktime".into(), bytes: Arc::from(mp4()) };
    let mislabelled = JobInput::Media { role: "source".into(), mime: "video/mp4".into(), bytes: Arc::from(png()) };
    let bad = [
        genjutsu(MOTION_TRANSFER, json!({}), vec![source()]),
        genjutsu(MOTION_TRANSFER, json!({}), vec![image()]),
        genjutsu(MOTION_TRANSFER, json!({}), vec![source(), source(), image()]),
        genjutsu(MOTION_TRANSFER, json!({}), vec![mov, image()]),
        genjutsu(MOTION_TRANSFER, json!({}), vec![mislabelled, image()]),
        genjutsu(MOTION_TRANSFER, json!({"resolution": "4K"}), vec![source(), image()]),
        genjutsu(MOTION_TRANSFER, json!({"duration": 5}), vec![source(), image()]),
        genjutsu(OBJECT_SWAP, json!({}), [vec![source()], vec![image(); 9]].concat()),
        genjutsu(OBJECT_SWAP, json!({}), vec![prompt(&"x".repeat(10_001)), source(), image()]),
        job(KLING_I2V, json!({}), vec![prompt("x"), source()]),
    ];
    for j in bad {
        let t = Transport::new(vec![]);
        assert_eq!(block_on(adapter(&t).submit(&j)).unwrap_err().kind, ErrorKind::UnsupportedInput, "{:?}", j.params);
        assert!(t.requests().is_empty());
    }
    let small = Limits { video_bytes: 8, ..Limits::HARD };
    let t = Transport::new(vec![]);
    let e =
        block_on(Higgsfield::with_limits(key(), t.clone(), small).unwrap().submit(&genjutsu(MOTION_TRANSFER, json!({}), vec![source(), image()]))).unwrap_err();
    assert!(e.message.contains("source video must be under"), "{e:?}");
    assert!(Higgsfield::with_limits(key(), Transport::new(vec![]), Limits { video_bytes: 201 * MIB, ..Limits::HARD }).is_err());
}

#[test]
fn uploads_get_longer_the_larger_they_are() {
    assert_eq!(super::upload_timeout_ms(0), 120_000);
    // The engine's 64 MiB media cap at 1 Mbit/s, with two minutes to spare.
    assert_eq!(super::upload_timeout_ms(64 << 20), 120_000 + 524_288);
    assert_eq!(super::upload_timeout_ms(usize::MAX), 120_000u64.saturating_add(u64::try_from(usize::MAX / 1024).unwrap().saturating_mul(8)));
}

const PRESET: &str = "6f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0";

#[test]
fn a_restyle_names_its_preset_and_sends_references_only_when_wired() {
    // No references: no `image_urls` at all, since restyle refuses nulls.
    let t = Transport::new(vec![video_grant(), empty(200), receipt()]);
    block_on(adapter(&t).submit(&genjutsu(RESTYLE, json!({"preset_id": PRESET.to_uppercase()}), vec![prompt(""), source()]))).unwrap();
    let requests = t.requests();
    assert_eq!(requests[2].url, "https://api.higgsfield.ai/higgsfield/genjutsu/restyle/v1.0");
    assert_eq!(body(&requests[2]), json!({"video_url": "https://upload.cdn.example.com/inputs/source.mp4", "preset_id": PRESET, "resolution": "720p"}));
    // References follow the source, in order.
    let t = Transport::new(vec![video_grant(), empty(200), image_grant(1), empty(200), receipt()]);
    block_on(adapter(&t).submit(&genjutsu(RESTYLE, json!({"preset_id": PRESET, "resolution": "480p"}), vec![source(), image()]))).unwrap();
    assert_eq!(body(&t.requests()[4])["image_urls"], json!(["https://upload.cdn.example.com/inputs/ref1.png"]));
    // A finished restyle downloads its MP4.
    let done = json!({"request_id": REQUEST, "status": "completed", "video": {"url": "https://cdn.example.com/out.mp4"}});
    let t = Transport::new(vec![ok(done), file(&mp4())]);
    assert!(matches!(block_on(adapter(&t).poll(&queued(RESTYLE, 1))).unwrap(), JobProgress::Done(_)));
}

#[test]
fn restyle_jobs_are_checked_before_anything_is_sent() {
    let bad = [
        genjutsu(RESTYLE, json!({}), vec![source()]),
        genjutsu(RESTYLE, json!({"preset_id": "not-a-preset"}), vec![source()]),
        genjutsu(RESTYLE, json!({"preset_id": 7}), vec![source()]),
        genjutsu(RESTYLE, json!({"preset_id": PRESET}), vec![image()]),
        genjutsu(RESTYLE, json!({"preset_id": PRESET}), [vec![source()], vec![image(); 6]].concat()),
        // Only restyle takes a preset.
        genjutsu(MOTION_TRANSFER, json!({"preset_id": PRESET}), vec![source(), image()]),
    ];
    for j in bad {
        let t = Transport::new(vec![]);
        assert_eq!(block_on(adapter(&t).submit(&j)).unwrap_err().kind, ErrorKind::UnsupportedInput, "{:?}", j.params);
        assert!(t.requests().is_empty());
    }
}

#[test]
fn the_preset_list_keeps_usable_entries_in_order() {
    let list = json!({"model": RESTYLE, "items": [
        {"id": PRESET, "name": " Watercolor ", "preview_url": "https://cdn.example.com/w.mp4"},
        {"id": "not-a-uuid", "name": "Broken"},
        {"id": "0a1b2c3d-4e5f-4061-8273-94a5b6c7d8e9", "name": ""},
        {"id": PRESET.to_uppercase(), "name": "Watercolor again"},
        {"id": "0a1b2c3d-4e5f-4061-8273-94a5b6c7d8e9", "name": format!("leaks {SECRET}")},
        {"id": "11111111-2222-4333-8444-555555555555", "name": "Claymation"},
    ]});
    let t = Transport::new(vec![ok(list)]);
    let presets = block_on(adapter(&t).restyle_presets()).unwrap();
    let request = &t.requests()[0];
    assert_eq!((request.method, request.url.as_str()), (HttpMethod::Get, "https://api.higgsfield.ai/models/higgsfield/genjutsu/restyle/v1.0/presets"));
    let names: Vec<(&str, &str)> = presets.presets.iter().map(|p| (p.id.as_str(), p.name.as_str())).collect();
    assert_eq!(names, [(PRESET, "Watercolor"), ("11111111-2222-4333-8444-555555555555", "Claymation")]);
    assert_eq!(presets.skipped, 4);
    // A reply that isn't a list, or a list past the cap, is an error.
    for reply in [ok(json!({"items": "no"})), ok(json!({"items": vec![json!({}); 1025]})), status(401, json!({"detail": "bad key"}))] {
        let t = Transport::new(vec![reply]);
        let e = block_on(adapter(&t).restyle_presets()).unwrap_err();
        no_secret(&e);
    }
}

const SOUL_ID: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
fn reference(version: &str, status: &str) -> Value {
    json!({"id": SOUL_ID, "name": "Mara", "model_version": version, "status": status, "thumbnail_url": null, "created_at": "2026-10-20T10:00:00Z", "in_progress_at": null, "fail_reason": null})
}

#[test]
fn a_soul_id_is_confirmed_ready_on_the_account_before_the_paid_submit() {
    let t = Transport::new(vec![ok(reference("v2", "completed")), receipt()]);
    let job = soul(json!({"custom_reference_id": SOUL_ID.to_uppercase(), "custom_reference_strength": 0.654}));
    block_on(adapter(&t).submit(&job)).unwrap();
    let [check, submit] = t.requests().try_into().unwrap();
    assert_eq!((check.method, check.url.as_str()), (HttpMethod::Get, format!("https://api.higgsfield.ai/v1/custom-references/{SOUL_ID}").as_str()));
    assert!(header(&check, "Authorization").is_some());
    assert_eq!(body(&submit)["custom_reference_id"], json!(SOUL_ID));
    assert_eq!(body(&submit)["custom_reference_strength"], json!(0.65));
    // A retry of the same job on the same adapter replays the checked body without asking again.
    let higgsfield = adapter(&t);
    t.push(vec![ok(reference("v2", "completed")), receipt(), receipt()]);
    block_on(higgsfield.submit(&job)).unwrap();
    block_on(higgsfield.submit(&job)).unwrap();
    let urls: Vec<String> = t.requests().iter().skip(2).map(|r| r.url.clone()).collect();
    assert_eq!(urls.iter().filter(|u| u.contains("custom-references")).count(), 1, "{urls:?}");
    // The check reads only what a run needs: a ready identity whose name the list wouldn't show
    // still runs, and a pasted id may carry spaces.
    for name in [json!("x".repeat(101)), json!("Line\nbreak"), json!(""), Value::Null] {
        let t = Transport::new(vec![ok(json!({"id": SOUL_ID, "name": name, "model_version": "v2", "status": "completed"})), receipt()]);
        block_on(adapter(&t).submit(&soul(json!({"custom_reference_id": format!(" {SOUL_ID} ")})))).unwrap();
        assert_eq!(body(&t.requests()[1])["custom_reference_id"], json!(SOUL_ID));
    }
}

#[test]
fn without_a_soul_id_nothing_is_checked_and_the_strength_is_left_out() {
    for params in [json!({"custom_reference_strength": 0.4}), json!({"custom_reference_id": "  ", "custom_reference_strength": 0.4})] {
        let t = Transport::new(vec![receipt()]);
        block_on(adapter(&t).submit(&soul(params))).unwrap();
        let [submit] = t.requests().try_into().unwrap();
        let sent = body(&submit);
        assert!(sent.get("custom_reference_id").is_none() && sent.get("custom_reference_strength").is_none(), "{sent}");
    }
    // A Soul ID alone is sent at full strength, Higgsfield's default.
    let t = Transport::new(vec![ok(reference("v2", "completed")), receipt()]);
    block_on(adapter(&t).submit(&soul(json!({"custom_reference_id": SOUL_ID})))).unwrap();
    assert_eq!(body(&t.requests()[1])["custom_reference_strength"], json!(1.0));
}

#[test]
fn a_soul_id_that_is_not_ready_here_is_refused_before_anything_is_paid() {
    let job = soul(json!({"custom_reference_id": SOUL_ID}));
    let cases = [
        (ok(reference("v2", "in_progress")), "still training"),
        (ok(reference("v2", "queued")), "still training"),
        (ok(reference("v2", "not_ready")), "still training"),
        (ok(reference("v2", "archived")), "Higgsfield reports it as archived"),
        (ok(json!({"id": SOUL_ID, "name": "Mara", "model_version": "v2", "status": "failed", "fail_reason": "faces differ"})), "failed: faces differ"),
        (ok(reference("v2", "failed")), "training failed"),
        (ok(reference("v1", "completed")), "another SOUL version"),
        (status(404, json!({"detail": "Not found"})), "no such Soul ID"),
        (ok(json!({"id": "11111111-2222-4333-8444-555555555555", "name": "Other", "model_version": "v2", "status": "completed"})), "different Soul ID"),
        (ok(json!({"status": "completed"})), "documented reference"),
    ];
    for (reply, says) in cases {
        let t = Transport::new(vec![reply]);
        let e = block_on(adapter(&t).submit(&job)).unwrap_err();
        assert!(e.message.contains(says), "{says}: {e:?}");
        // Only the free check went out.
        assert_eq!(t.requests().len(), 1);
        no_secret(&e);
    }
    // An id that isn't a UUID never reaches a URL.
    for id in [json!("../requests"), json!("7c9e6679-7425-40de-944b-e07fc1f90ae7x"), json!(7)] {
        let t = Transport::new(vec![]);
        let e = block_on(adapter(&t).submit(&soul(json!({"custom_reference_id": id})))).unwrap_err();
        assert_eq!(e.kind, ErrorKind::UnsupportedInput);
        assert!(t.requests().is_empty());
    }
    for strength in [json!(1.5), json!(-0.1), json!("1")] {
        let t = Transport::new(vec![]);
        assert!(block_on(adapter(&t).submit(&soul(json!({"custom_reference_id": SOUL_ID, "custom_reference_strength": strength})))).is_err());
        assert!(t.requests().is_empty());
    }
}

#[test]
fn the_soul_id_list_keeps_ready_soul_2_identities_across_pages() {
    let entry = |id: &str, name: &str, version: &str, status: &str| json!({"id": id, "name": name, "model_version": version, "status": status});
    let mut first: Vec<Value> = (0..50).map(|n| entry(&format!("00000000-0000-4000-8000-{n:012}"), &format!("Person {n}"), "v1", "completed")).collect();
    first[3] = entry(SOUL_ID, " Mara ", "v2", "completed");
    first[4] = entry("11111111-2222-4333-8444-555555555555", "Training", "v2", "in_progress");
    first[5] = json!({"id": "not-a-uuid", "name": "Broken", "model_version": "v2", "status": "completed"});
    first[6] = entry("22222222-2222-4333-8444-555555555555", &format!("leaks {SECRET}"), "v2", "completed");
    let second = vec![entry(SOUL_ID, "Mara again", "v2", "completed"), entry("33333333-2222-4333-8444-555555555555", "Jun", "v2", "completed")];
    // A short page isn't taken for the last: only an empty one ends the list.
    let t = Transport::new(vec![ok(json!({"items": first})), ok(Value::Array(second)), ok(json!({"items": []}))]);
    let ids = block_on(adapter(&t).soul_ids()).unwrap();
    let urls: Vec<String> = t.requests().iter().map(|r| r.url.clone()).collect();
    let page = |n: usize| format!("https://api.higgsfield.ai/v1/custom-references/list?page={n}&page_size=50");
    assert_eq!(urls, [page(1), page(2), page(3)]);
    let names: Vec<(&str, &str)> = ids.ids.iter().map(|s| (s.id.as_str(), s.name.as_str())).collect();
    assert_eq!(names, [(SOUL_ID, "Mara"), ("33333333-2222-4333-8444-555555555555", "Jun")]);
    assert_eq!(ids.skipped, 2);
    // A reply in another shape, a page longer than asked for, or an account past the cap is an
    // error, never a partial list.
    for replies in [vec![ok(json!({"data": []}))], vec![ok(Value::Array(vec![json!({}); 51]))], vec![status(401, json!({"detail": "bad key"}))]] {
        let t = Transport::new(replies);
        no_secret(&block_on(adapter(&t).soul_ids()).unwrap_err());
    }
    // Exactly 1,000 is listed in full; one more past the cap is refused.
    let full = || ok(Value::Array(vec![json!({}); 50]));
    let t = Transport::new((0..20).map(|_| full()).chain([ok(Value::Array(vec![]))]).collect());
    assert_eq!(block_on(adapter(&t).soul_ids()).unwrap().skipped, 1000);
    let t = Transport::new((0..21).map(|_| full()).collect());
    assert!(block_on(adapter(&t).soul_ids()).unwrap_err().message.contains("more Soul IDs"));
    assert_eq!(t.requests().len(), 21);
}

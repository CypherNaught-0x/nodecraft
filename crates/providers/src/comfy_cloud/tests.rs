use super::*;
use crate::{HttpMethod, JobInput, SubmitContext};
use futures::executor::block_on;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

const KEY: &str = "comfyui-fixture-key-not-real";
const JOB: &str = "7d1b6a3e-2c4f-4b8e-9f10-3a5c7e9b1d24";
const ASSET: &str = "0c9e8f7a-6b5d-4c3e-a2f1-0e9d8c7b6a51";
const OUT: &str = "5a4b3c2d-1e0f-4a9b-8c7d-6e5f4a3b2c1d";
const SIGNED: &str = "https://storage.googleapis.com/comfy-out/result.png?X-Goog-Signature=abc";
const OBJECT_INFO: &str = r#"{
  "LoadImage": {"input": {"required": {"image": [["example.png"], {"image_upload": true}]}}, "output": ["IMAGE", "MASK"]},
  "CLIPTextEncode": {"input": {"required": {"text": ["STRING", {"multiline": true}]}}, "output": ["CONDITIONING"]},
  "SaveImage": {"input": {"required": {"images": ["IMAGE", {}], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true},
  "PartnerImage": {"input": {"required": {"prompt": ["STRING", {}]}}, "output": ["IMAGE"], "api_node": true}
}"#;

fn key() -> Secret {
    Secret::new(KEY).unwrap()
}

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
    fn paths(&self) -> Vec<String> {
        self.requests().iter().map(|r| r.url.trim_start_matches(ORIGIN).to_owned()).collect()
    }
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-following send must not run")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(r);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}

fn ok(v: Value) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&v).unwrap() })
}
fn status(code: u16, v: Value) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: code, headers: vec![("Retry-After".into(), "7".into())], body: serde_json::to_vec(&v).unwrap() })
}
fn object_info() -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: OBJECT_INFO.into() })
}
fn redirect(to: &str) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 302, headers: vec![("location".into(), to.into())], body: vec![] })
}
fn file(bytes: &[u8]) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: bytes.to_vec() })
}
fn png() -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[120; 16], &nodecraft_media::Limits::default()).unwrap()
}
fn job_reply(status: &str, extra: Value) -> Result<HttpResponse, ProviderError> {
    let mut v = json!({"id": JOB, "status": status});
    if let (Some(v), Some(extra)) = (v.as_object_mut(), extra.as_object()) {
        v.extend(extra.clone());
    }
    ok(v)
}
fn workflow() -> Value {
    json!({
        "1": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
        "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "old"}},
        "3": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "NodeCraft"}}
    })
}
fn job_with(params: Value, inputs: Vec<JobInput>) -> ProviderJob {
    ProviderJob { model: "comfy-cloud:workflow".into(), params: params.as_object().cloned().unwrap(), inputs, idempotency_key: "nodecraft:run-uuid:1".into() }
}
fn job() -> ProviderJob {
    job_with(
        json!({"workflow": workflow(), "values": {"2.text": "a red fox"}}),
        vec![JobInput::Media { role: "1.image".into(), mime: "image/png".into(), bytes: Arc::from(png()) }],
    )
}
fn provider(http: Arc<Transport>) -> ComfyCloud {
    ComfyCloud::new(key(), http, Arc::new(|| 0))
}
fn bearer(r: &HttpRequest) -> bool {
    r.headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Bearer {KEY}")) && r.secret_headers.contains(&"authorization".to_owned())
}
fn handle() -> JobHandle {
    JobHandle { remote_id: JOB.into(), data: json!({"v": 1}) }
}

#[test]
fn submit_checks_uploads_the_input_as_an_asset_and_creates_one_job_with_a_stable_key() {
    let http = Transport::new(vec![object_info(), ok(json!({"id": ASSET, "name": "x.png"})), job_reply("queued", json!({}))]);
    let scope = SubmitContext::new();
    let submission = block_on(provider(http.clone()).submit_with_delivery(&job(), &scope)).unwrap();
    let Submission::Queued(h) = submission else { panic!("{submission:?}") };
    assert_eq!(h.remote_id, JOB);
    assert_eq!(http.paths(), ["/api/object_info", "/api/v2/assets", "/api/v2/jobs"]);
    let requests = http.requests();
    // object_info takes the key as X-API-Key; the v2 API as a bearer token. Both are secret.
    assert!(requests[0].headers.iter().any(|(n, v)| n == "X-API-Key" && v == KEY) && requests[0].secret_headers.contains(&"x-api-key".to_owned()));
    assert!(bearer(&requests[1]) && bearer(&requests[2]));
    let upload = String::from_utf8_lossy(&requests[1].body);
    assert!(upload.contains("name=\"file\"") && upload.contains("name=\"file_path\"") && upload.contains("image/png"), "{upload}");
    assert!(upload.contains("name=\"tags\"\r\n\r\n[\"input\"]\r\n"), "tags is a JSON array field: {upload}");
    let body: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(body["workflow"]["2"]["inputs"]["text"], "a red fox");
    let reference = &body["workflow"]["1"]["inputs"]["image"];
    assert_eq!(reference["__type"], "core/ASSET");
    assert_eq!(reference["info"]["id"], ASSET);
    assert!(reference["info"]["file_path"].as_str().unwrap().starts_with("nodecraft-"));
    assert!(body.get("extra_data").is_none(), "the key never goes into a request body");
    assert!(!String::from_utf8_lossy(&requests[2].body).contains(KEY));
    let key_header = requests[2].headers.iter().find(|(n, _)| n == "Idempotency-Key").map(|(_, v)| v.clone()).unwrap();
    assert_eq!(key_header, request::idempotency_key("nodecraft:run-uuid:1"));
    assert_eq!(key_header.len(), 36);
    assert_ne!(key_header, request::idempotency_key("nodecraft:run-uuid:2"));
    assert!(scope.finish_error().is_none(), "the job was created");
}

#[test]
fn bad_workflows_and_inputs_are_refused_before_anything_is_uploaded_or_paid_for() {
    let png = || JobInput::Media { role: "1.image".into(), mime: "image/png".into(), bytes: Arc::from(png()) };
    let cases = [
        job_with(json!({}), vec![]),
        job_with(json!({"workflow": {"1": {"class_type": "NotInstalled", "inputs": {}}}}), vec![]),
        job_with(json!({"workflow": workflow(), "values": {"2.missing": 1}}), vec![]),
        job_with(json!({"workflow": workflow(), "values": {"nodot": 1}}), vec![]),
        job_with(
            json!({"workflow": workflow()}),
            vec![JobInput::Media { role: "2.text".into(), mime: "image/png".into(), bytes: Arc::from(super::tests::png()) }],
        ),
        job_with(
            json!({"workflow": workflow()}),
            vec![JobInput::Media { role: "9.image".into(), mime: "image/png".into(), bytes: Arc::from(super::tests::png()) }],
        ),
        job_with(
            json!({"workflow": workflow()}),
            vec![JobInput::Media { role: "1.image".into(), mime: "image/png".into(), bytes: Arc::from(&b"not media"[..]) }],
        ),
        job_with(
            json!({"workflow": workflow()}),
            vec![JobInput::Media { role: "1.image".into(), mime: "image/x-exr".into(), bytes: Arc::from(&b"\x76\x2f\x31\x01\x02\x00\x00\x00"[..]) }],
        ),
        // A partner node bills credits no quote covers.
        job_with(json!({"workflow": {"1": {"class_type": "PartnerImage", "inputs": {"prompt": "a fox"}}}}), vec![]),
    ];
    for (n, j) in cases.iter().enumerate() {
        let http = Transport::new(vec![object_info()]);
        let scope = SubmitContext::new();
        let error = block_on(provider(http.clone()).submit_with_delivery(j, &scope)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "case {n}: {error:?}");
        assert!(scope.finish_error().is_some(), "case {n}: nothing paid was sent");
        assert_eq!(http.paths(), ["/api/object_info"], "case {n}");
    }
    let small = provider(Transport::new(vec![object_info()])).with_limits(Limits { input_bytes: 8, ..Limits::HARD }).unwrap();
    let j = job_with(json!({"workflow": workflow()}), vec![png()]);
    assert_eq!(block_on(small.submit(&j)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert!(provider(Transport::new(vec![])).with_limits(Limits { output_files: Limits::HARD.output_files + 1, ..Limits::HARD }).is_err());
}

#[test]
fn a_failed_upload_proves_no_job_but_a_lost_create_reply_may_hide_one() {
    let http = Transport::new(vec![object_info(), status(500, json!({"error": {"code": "internal", "message": "boom"}}))]);
    let scope = SubmitContext::new();
    assert_eq!(block_on(provider(http).submit_with_delivery(&job(), &scope)).unwrap_err().kind, ErrorKind::Server);
    assert!(scope.finish_error().is_some(), "an upload creates no job");

    let http = Transport::new(vec![object_info(), ok(json!({"id": ASSET})), Err(ProviderError::new(ErrorKind::Network, "reset"))]);
    let scope = SubmitContext::new();
    let error = block_on(provider(http).submit_with_delivery(&job(), &scope)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("may have been created"), "{}", error.message);
    assert!(scope.finish_error().is_none(), "the job may exist");
}

#[test]
fn a_documented_refusal_of_the_create_proves_no_job_but_other_failures_may_hide_one() {
    let create = |reply| {
        let http = Transport::new(vec![object_info(), ok(json!({"id": ASSET})), reply]);
        let scope = SubmitContext::new();
        let error = block_on(provider(http).submit_with_delivery(&job(), &scope)).unwrap_err();
        (error, scope.finish_error().is_some())
    };
    let refusal = |code: &str| json!({"error": {"code": code, "message": "no"}});
    for (code, at, kind) in [
        ("queue_full", 429, ErrorKind::Busy),
        ("rate_limited", 429, ErrorKind::RateLimited),
        ("insufficient_credits", 402, ErrorKind::Budget),
        ("invalid_workflow", 422, ErrorKind::UnsupportedInput),
    ] {
        let (error, refused) = create(status(at, refusal(code)));
        assert_eq!(error.kind, kind, "{code}");
        assert!(refused, "{code}: Comfy made no job");
    }
    // A 5xx, a lost reply, an unparsed body, a code at another status or a key Comfy already took
    // may each hide a job.
    for (n, reply) in [
        status(500, refusal("queue_full")),
        status(503, json!({})),
        Err(ProviderError::new(ErrorKind::Timeout, "lost")),
        status(429, json!("queue_full")),
        status(400, refusal("queue_full")),
        status(429, refusal("busy")),
        status(422, refusal("idempotency_key_reuse")),
    ]
    .into_iter()
    .enumerate()
    {
        let (_, refused) = create(reply);
        assert!(!refused, "case {n}: the job may exist");
    }
    // A refusal too large to read is unread. No upload, so the limit only meets the create.
    let big = json!({"error": {"code": "queue_full", "message": "x".repeat(64)}});
    let http = Transport::new(vec![object_info(), status(429, big)]);
    let small = provider(http).with_limits(Limits { response_bytes: 32, ..Limits::HARD }).unwrap();
    let scope = SubmitContext::new();
    assert!(block_on(small.submit_with_delivery(&job_with(json!({"workflow": workflow()}), vec![]), &scope)).is_err());
    assert!(scope.finish_error().is_none(), "an oversized refusal isn't read");
}

#[test]
fn a_retry_whose_key_comfy_already_took_is_reported_not_run_again() {
    let http = Transport::new(vec![
        object_info(),
        ok(json!({"id": ASSET})),
        status(422, json!({"error": {"code": "idempotency_key_reuse", "message": "key reused"}})),
    ]);
    let error = block_on(provider(http).submit(&job())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::JobFailed);
    assert!(!error.is_transient(), "the scheduler must not retry it");
    assert!(error.message.contains("won't submit it twice"), "{}", error.message);
}

#[test]
fn comfy_error_codes_map_to_shared_kinds_without_echoing_the_key() {
    for (http_status, code, kind) in [
        (402, "insufficient_credits", ErrorKind::Budget),
        (429, "queue_full", ErrorKind::Busy),
        (429, "rate_limited", ErrorKind::RateLimited),
        (401, "unauthorized", ErrorKind::Auth),
        (403, "sso_required", ErrorKind::Auth),
        (422, "invalid_workflow", ErrorKind::UnsupportedInput),
        (422, "missing_asset", ErrorKind::UnsupportedInput),
        (451, "content_blocked", ErrorKind::BlockedContent),
        (404, "not_found", ErrorKind::NotFound),
        (503, "something_new", ErrorKind::Busy),
    ] {
        let http = Transport::new(vec![status(http_status, json!({"error": {"code": code, "message": format!("bad key {KEY}")}}))]);
        let error = block_on(provider(http).catalog()).unwrap_err();
        assert_eq!(error.kind, kind, "{code}");
        assert_eq!(error.retry_after_ms, Some(7000), "{code}");
        assert!(!error.message.contains(KEY), "{code}: {}", error.message);
        assert!(error.message.contains("withheld"), "{code}");
    }
    // A bare `{message}` refusal still says why.
    let http = Transport::new(vec![status(400, json!({"message": "workflow must be an object"}))]);
    assert!(block_on(provider(http).catalog()).unwrap_err().message.ends_with("workflow must be an object"));
    let http = Transport::new(vec![Ok(HttpResponse { status: 500, headers: vec![], body: b"<html>".to_vec() })]);
    assert_eq!(block_on(provider(http).catalog()).unwrap_err().kind, ErrorKind::Server);
}

#[test]
fn the_catalog_is_cached_for_a_few_minutes() {
    let now = Arc::new(AtomicU64::new(0));
    let clock = Arc::clone(&now);
    let http = Transport::new(vec![object_info(), object_info()]);
    let cloud = ComfyCloud::new(key(), http.clone(), Arc::new(move || clock.load(Ordering::SeqCst)));
    block_on(cloud.catalog()).unwrap();
    now.store(CATALOG_TTL_MS - 1, Ordering::SeqCst);
    block_on(cloud.catalog()).unwrap();
    assert_eq!(http.requests().len(), 1);
    now.store(CATALOG_TTL_MS, Ordering::SeqCst);
    block_on(cloud.catalog()).unwrap();
    assert_eq!(http.requests().len(), 2);
    cloud.invalidate_catalog();
}

#[test]
fn polling_reports_the_queue_progress_and_terminal_states() {
    let poll = |reply| block_on(provider(Transport::new(vec![reply])).poll(&handle()));
    assert!(matches!(poll(job_reply("queued", json!({"queue_position": 3}))), Ok(JobProgress::Queued { position: Some(3) })));
    match poll(job_reply("running", json!({"progress": {"value": 7.5, "message": "KSampler\u{7}"}}))) {
        Ok(JobProgress::Running { fraction: Some(f), message: Some(m), .. }) => {
            assert_eq!(f, 1.0);
            assert_eq!(m, "KSampler");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(poll(job_reply("canceling", json!({"progress": {"value": null}}))), Ok(JobProgress::Running { fraction: None, .. })));
    let failed = json!({"error": {"code": "execution_error", "message": "out of memory", "node_id": "3", "class_type": "SaveImage"}});
    match poll(job_reply("failed", failed)) {
        Ok(JobProgress::Failed(e)) => {
            assert_eq!(e.kind, ErrorKind::JobFailed);
            assert!(e.message.contains("node 3 (SaveImage)") && e.message.contains("out of memory"), "{}", e.message);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(poll(job_reply("canceled", json!({}))), Ok(JobProgress::Failed(ProviderError { kind: ErrorKind::Cancelled, .. }))));
    assert!(matches!(poll(job_reply("expired", json!({}))), Ok(JobProgress::Failed(ProviderError { kind: ErrorKind::Expired, .. }))));
    assert_eq!(poll(job_reply("dancing", json!({}))).unwrap_err().kind, ErrorKind::Malformed);
    let other = ok(json!({"id": ASSET, "status": "queued"}));
    assert_eq!(poll(other).unwrap_err().kind, ErrorKind::Malformed);
}

#[test]
fn hostile_handles_and_ids_never_reach_a_url() {
    for id in ["", "../jobs", "a/b", "id?x=1", &"a".repeat(200)] {
        let http = Transport::new(vec![]);
        let h = JobHandle { remote_id: id.into(), data: json!({"v": 1}) };
        assert!(block_on(provider(http.clone()).poll(&h)).is_err(), "{id}");
        assert!(block_on(provider(http.clone()).cancel(&h)).is_err(), "{id}");
        assert!(http.requests().is_empty(), "{id}");
    }
    let wrong_version = JobHandle { remote_id: JOB.into(), data: json!({"v": 2}) };
    assert!(block_on(provider(Transport::new(vec![])).poll(&wrong_version)).is_err());
    // A created job whose id isn't one is refused too.
    let http = Transport::new(vec![object_info(), ok(json!({"id": ASSET})), ok(json!({"id": "../x", "status": "queued"}))]);
    assert_eq!(block_on(provider(http).submit(&job())).unwrap_err().kind, ErrorKind::Malformed);
}

#[test]
fn a_finished_job_downloads_through_the_signed_redirect_without_the_key() {
    let outputs = json!({"outputs": [
        {"node_id": "3", "name": "NodeCraft_00001_.png", "type": "image", "id": OUT},
        {"node_id": "4", "name": "latent.latent", "type": "latent", "id": ASSET}
    ]});
    let http = Transport::new(vec![job_reply("succeeded", outputs), redirect(SIGNED), file(&png())]);
    let progress = block_on(provider(http.clone()).poll(&handle())).unwrap();
    let JobProgress::Done(done) = progress else { panic!("{progress:?}") };
    assert!(matches!(done.outputs.as_slice(), [Output::Bytes { mime, .. }] if mime == "image/png"));
    let requests = http.requests();
    assert_eq!(requests[1].url, format!("{ORIGIN}/api/v2/assets/{OUT}/content"));
    assert!(bearer(&requests[1]));
    assert_eq!(requests[2].url, SIGNED);
    assert!(requests[2].headers.iter().all(|(_, v)| !v.contains(KEY)), "the signed URL is fetched without the key");
    assert_eq!(requests.len(), 3, "the latent is skipped");
}

#[test]
fn downloads_respect_redirect_rules_and_caps() {
    let one = |id: &str| json!({"outputs": [{"name": "a.png", "type": "image", "id": id}]});
    // http (not https) redirects, and ones carrying the key, are refused.
    for to in ["http://storage.example.com/a.png", &format!("https://x.example.com/?k={KEY}")] {
        let http = Transport::new(vec![job_reply("succeeded", one(OUT)), redirect(to)]);
        assert_eq!(block_on(provider(http.clone()).poll(&handle())).unwrap_err().kind, ErrorKind::Malformed, "{to}");
        assert_eq!(http.requests().len(), 2);
    }
    // A bare 200 with the bytes is accepted.
    let http = Transport::new(vec![job_reply("succeeded", one(OUT)), file(&png())]);
    assert!(matches!(block_on(provider(http).poll(&handle())), Ok(JobProgress::Done(_))));
    // An expired signed link is a retryable failure, so the next poll can mint a fresh one.
    let http = Transport::new(vec![job_reply("succeeded", one(OUT)), redirect(SIGNED), status(403, json!({}))]);
    let error = block_on(provider(http).poll(&handle())).unwrap_err();
    assert!(error.is_transient(), "{error:?}");
    // Comfy's own refusal of the download says what is wrong, and isn't retried.
    for (code, kind) in [(401, ErrorKind::Auth), (404, ErrorKind::NotFound), (451, ErrorKind::BlockedContent)] {
        let http = Transport::new(vec![job_reply("succeeded", one(OUT)), status(code, json!({}))]);
        let error = block_on(provider(http).poll(&handle())).unwrap_err();
        assert_eq!(error.kind, kind, "{code}");
        assert!(!error.is_transient(), "{code}: {error:?}");
    }
    // Too many files, an oversized file, text pages and latents-only jobs fail clearly.
    let many: Vec<Value> = (0..3).map(|_| json!({"name": "a.png", "type": "image", "id": OUT})).collect();
    let limited = |http| provider(http).with_limits(Limits { output_files: 2, output_bytes: 64, ..Limits::HARD }).unwrap();
    let http = Transport::new(vec![job_reply("succeeded", json!({"outputs": many}))]);
    assert_eq!(block_on(limited(http).poll(&handle())).unwrap_err().kind, ErrorKind::UnsupportedInput);
    let http = Transport::new(vec![job_reply("succeeded", one(OUT)), file(&[0u8; 65])]);
    assert_eq!(block_on(limited(http).poll(&handle())).unwrap_err().kind, ErrorKind::Malformed);
    // A file Comfy says is too large isn't downloaded at all.
    let http = Transport::new(vec![job_reply("succeeded", json!({"outputs": [{"name": "a.png", "type": "image", "id": OUT, "size_bytes": 65}]}))]);
    assert_eq!(block_on(limited(http.clone()).poll(&handle())).unwrap_err().kind, ErrorKind::Malformed);
    assert_eq!(http.requests().len(), 1);
    let http = Transport::new(vec![job_reply("succeeded", one(OUT)), file(b"<html>login</html>")]);
    assert_eq!(block_on(provider(http).poll(&handle())).unwrap_err().kind, ErrorKind::JobFailed);
    let http = Transport::new(vec![job_reply("succeeded", json!({"outputs": [{"name": "l", "type": "latent", "id": OUT}]}))]);
    assert_eq!(block_on(provider(http).poll(&handle())).unwrap_err().kind, ErrorKind::JobFailed);
    let http = Transport::new(vec![job_reply("succeeded", json!({}))]);
    assert_eq!(block_on(provider(http).poll(&handle())).unwrap_err().kind, ErrorKind::JobFailed);
    // A text output comes back as text.
    let http = Transport::new(vec![job_reply("succeeded", json!({"outputs": [{"name": "c.txt", "type": "text", "id": OUT}]})), file(b"a caption")]);
    match block_on(provider(http).poll(&handle())) {
        Ok(JobProgress::Done(done)) => assert!(matches!(done.outputs.as_slice(), [Output::Text(t)] if t == "a caption")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn cancel_posts_to_the_job_and_debug_hides_the_key() {
    let http = Transport::new(vec![job_reply("canceled", json!({}))]);
    let cloud = provider(http.clone());
    block_on(cloud.cancel(&handle())).unwrap();
    let r = &http.requests()[0];
    assert_eq!(r.method, HttpMethod::Post);
    assert_eq!(r.url, format!("{ORIGIN}/api/v2/jobs/{JOB}/cancel"));
    assert!(bearer(r));
    assert!(!format!("{cloud:?}").contains(KEY));
    assert!(!format!("{r:?}").contains(KEY));
    assert_eq!(cloud.id(), "comfy-cloud");
    assert!(!cloud.retry_ambiguous_submit(), "a retry under the same key could only be refused");
}

#[test]
fn uploads_are_named_with_the_extension_comfy_s_loaders_expect() {
    for (mime, ext) in [("image/png", "png"), ("image/jpeg", "jpg"), ("video/quicktime", "mov"), ("audio/mpeg", "mp3"), ("image/svg+xml", "svg"), ("x/", "bin")]
    {
        assert_eq!(request::extension(mime), ext, "{mime}");
    }
}

#[test]
fn mapping947_bad_saved_result_selection_is_refused_before_catalog_or_upload() {
    for outputs in ["[]".to_owned(), "null".into(), r#"["3","3"]"#.into(), " ".repeat(64 * 1024 + 1)] {
        let http = Transport::new(vec![]);
        let inputs = vec![JobInput::Text { role: "comfy.outputs".into(), text: outputs }];
        let error = block_on(provider(http.clone()).submit(&job_with(json!({}), inputs))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput);
        assert!(http.requests().is_empty(), "invalid result selection needs no server discovery");
    }
}

#[test]
fn hosted_workflow_card_rejects_conflicting_or_oversized_text_before_upload() {
    let text = |role: &str, text: String| JobInput::Text { role: role.into(), text };
    for inputs in [
        vec![text("comfy.workflow", workflow().to_string())],
        vec![text("comfy.values", r#"{"2.text":"fox"}"#.into())],
        vec![text("comfy.outputs", r#"["3"]"#.into())],
    ] {
        let http = Transport::new(vec![object_info()]);
        let error = block_on(provider(http.clone()).submit(&job_with(json!({"workflow":workflow()}), inputs))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput);
        assert_eq!(http.paths(), ["/api/object_info"]);
    }
    for (values, outputs) in [("[]", r#"["3"]"#), (r#"{"2.text":{}}"#, r#"["3"]"#), ("{}", r#"["missing"]"#)] {
        let http = Transport::new(vec![object_info()]);
        let inputs = vec![text("comfy.workflow", workflow().to_string()), text("comfy.values", values.into()), text("comfy.outputs", outputs.into())];
        assert!(block_on(provider(http.clone()).submit(&job_with(json!({}), inputs))).is_err());
        assert_eq!(http.paths(), ["/api/object_info"]);
    }
    let http = Transport::new(vec![object_info()]);
    let inputs = vec![text("comfy.workflow", " ".repeat(crate::comfyui::MAX_CARD_TEXT_BYTES + 1))];
    assert!(block_on(provider(http.clone()).submit(&job_with(json!({}), inputs))).is_err());
    assert_eq!(http.paths(), ["/api/object_info"]);
}

#[test]
fn hosted_outputs_follow_workflow_node_order_and_preserve_each_nodes_file_order() {
    let outputs = json!({"outputs":[
        {"node_id":"20","id":"last","name":"last.txt","type":"text"},
        {"node_id":"10","id":"first","name":"first.txt","type":"text"},
        {"node_id":"10","id":"second","name":"second.txt","type":"text"}
    ]});
    let http = Transport::new(vec![job_reply("succeeded", outputs), file(b"first"), file(b"second"), file(b"last")]);
    let JobProgress::Done(done) = block_on(provider(http.clone()).poll(&handle())).unwrap() else { panic!("not done") };
    assert_eq!(
        http.paths(),
        [format!("/api/v2/jobs/{JOB}"), "/api/v2/assets/first/content".into(), "/api/v2/assets/second/content".into(), "/api/v2/assets/last/content".into()]
    );
    assert!(matches!(done.outputs.as_slice(),[Output::Text(a),Output::Text(b),Output::Text(c)] if a=="first"&&b=="second"&&c=="last"));
}

#[test]
fn mapping947_cloud_honors_saved_result_order_when_the_api_reports_node_ids() {
    let outputs = json!({"outputs":[{"node_id":"2","name":"first.png","type":"image","id":OUT},{"node_id":"3","name":"second.png","type":"image","id":ASSET}]});
    let second = nodecraft_media::encode_png(2, 2, &[40; 16], &Default::default()).unwrap();
    let http = Transport::new(vec![job_reply("succeeded", outputs), file(&second), file(&png())]);
    let mut handle = handle();
    handle.data = crate::comfyui::output_selection::save(
        handle.data,
        &job_with(json!({}), vec![JobInput::Text { role: crate::comfyui::template::CARD_OUTPUTS.into(), text: r#"["3","2"]"#.into() }]),
    )
    .unwrap();
    let handle: JobHandle = serde_json::from_slice(&serde_json::to_vec(&handle).unwrap()).unwrap();
    let JobProgress::Done(done) = block_on(provider(http.clone()).poll(&handle)).unwrap() else { panic!("done") };
    assert!(matches!(&done.outputs[0],Output::Bytes { bytes,.. } if bytes.as_ref()==second));
    assert_eq!(http.requests()[1].url, format!("{ORIGIN}/api/v2/assets/{ASSET}/content"));
    assert_eq!(http.requests()[2].url, format!("{ORIGIN}/api/v2/assets/{OUT}/content"));
}

#[test]
fn recraft35_svg_reference_is_refused_before_hosted_upload_and_paid_job() {
    let http = Transport::new(vec![object_info()]);
    let mut j = job();
    j.inputs = vec![JobInput::Media {
        role: "1.image".into(),
        mime: nodecraft_media::svg::MIME.into(),
        bytes: Arc::from(br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="6"/>"#.as_slice()),
    }];
    let scope = SubmitContext::new();
    let error = block_on(provider(http.clone()).submit_with_delivery(&j, &scope)).unwrap_err();
    assert!(error.message.contains("Rasterize SVG"));
    assert_eq!(http.paths(), ["/api/object_info"], "no upload or paid job POST");
    assert!(scope.finish_error().is_some(), "the rejected reference cannot be billed");
}

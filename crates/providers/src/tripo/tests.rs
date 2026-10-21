use super::*;
use crate::{HttpMethod, JobInput};
use futures::executor::block_on;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};

const KEY: &str = "tripo-fixture-key-not-real";
const TASK: &str = "1ec04ced-4b87-44f6-a296-beee80777941";
const GLB_FIXTURE: &[u8] = include_bytes!("../fal/fixtures/triangle.glb");
const LINK: &str = "https://tripo-data.cdn.example.com/result/model.glb?sig=abc";

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
fn ok(v: Value) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&v).unwrap() })
}
fn status(code: u16, v: Value) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: code, headers: vec![("Retry-After".into(), "7".into())], body: serde_json::to_vec(&v).unwrap() })
}
fn file(bytes: &[u8]) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: bytes.to_vec() })
}
fn png() -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[120; 16], &nodecraft_media::Limits::default()).unwrap()
}
fn job(model: &str, inputs: Vec<JobInput>) -> ProviderJob {
    ProviderJob { model: model.into(), params: Default::default(), inputs, idempotency_key: "fixture-operation".into() }
}
fn prompt(text: &str) -> JobInput {
    JobInput::Text { role: "prompt".into(), text: text.into() }
}
fn image(role: &str) -> JobInput {
    JobInput::Media { role: role.into(), mime: "image/png".into(), bytes: Arc::from(png()) }
}
fn body(r: &HttpRequest) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}
fn authorised(r: &HttpRequest) -> bool {
    r.headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Bearer {KEY}")) && r.secret_headers.contains(&"authorization".to_owned())
}
fn created() -> Result<HttpResponse, ProviderError> {
    ok(json!({"code": 0, "data": {"task_id": TASK}}))
}
fn handle(task: &str) -> JobHandle {
    JobHandle { remote_id: TASK.into(), data: json!({"v": 1, "type": task}) }
}
fn queued(model: &str) -> JobHandle {
    match block_on(Tripo::new(key(), Transport::new(vec![created()])).submit(&job(model, vec![prompt("a red chair")]))) {
        Ok(Submission::Queued(handle)) => handle,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_text_task_sends_the_prompt_and_options_and_queues_a_resumable_handle() {
    let http = Transport::new(vec![created()]);
    let mut j = job(TEXT, vec![prompt("a red chair")]);
    j.params.insert("model_version".into(), json!("v2.5-20250123"));
    j.params.insert("model_seed".into(), json!(7));
    j.params.insert("negative_prompt".into(), json!("blurry"));
    j.params.insert("face_limit".into(), json!(20_000));
    let handle = match block_on(Tripo::new(key(), http.clone()).submit(&j)).unwrap() {
        Submission::Queued(handle) => handle,
        other => panic!("{other:?}"),
    };
    assert_eq!(handle, queued(TEXT));
    let sent = http.requests();
    let [create] = sent.as_slice() else { panic!("one request, got {}", sent.len()) };
    assert_eq!((create.method, create.url.as_str()), (HttpMethod::Post, "https://api.tripo3d.ai/v2/openapi/task"));
    assert!(authorised(create));
    assert_eq!(
        body(create),
        json!({"type": "text_to_model", "prompt": "a red chair", "model_version": "v2.5-20250123", "model_seed": 7, "negative_prompt": "blurry", "face_limit": 20000, "texture": true, "pbr": true})
    );
}

#[test]
fn image_and_multiview_tasks_upload_each_image_then_name_its_token_in_view_order() {
    let http = Transport::new(vec![ok(json!({"code": 0, "data": {"image_token": "tok-1"}})), created()]);
    block_on(Tripo::new(key(), http.clone()).submit(&job(IMAGE, vec![image("image")]))).unwrap();
    let sent = http.requests();
    let [upload, create] = sent.as_slice() else { panic!("two requests") };
    assert_eq!(upload.url, "https://api.tripo3d.ai/v2/openapi/upload");
    assert!(authorised(upload));
    let multipart = String::from_utf8_lossy(&upload.body);
    assert!(multipart.contains("name=\"file\"; filename=\"image.png\"") && multipart.contains("Content-Type: image/png"), "{multipart}");
    assert_eq!(body(create), json!({"type": "image_to_model", "file": {"type": "png", "file_token": "tok-1"}, "texture": true, "pbr": true}));

    // Back and front wired in that order: slots follow Tripo's front, left, back, right.
    let http = Transport::new(vec![
        ok(json!({"code": 0, "data": {"image_token": "tok-back"}})),
        ok(json!({"code": 0, "data": {"image_token": "tok-front"}})),
        created(),
    ]);
    block_on(Tripo::new(key(), http.clone()).submit(&job(MULTIVIEW, vec![image("back"), image("front")]))).unwrap();
    let sent = http.requests();
    assert_eq!(sent.len(), 3);
    assert_eq!(body(sent.last().unwrap())["files"], json!([{"type": "png", "file_token": "tok-front"}, {}, {"type": "png", "file_token": "tok-back"}, {}]));
}

#[test]
fn jobs_tripo_cant_take_are_refused_before_anything_is_sent() {
    let long = "x".repeat(1025);
    let jpeg_claim = JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes: Arc::from(png()) };
    let gif = JobInput::Media { role: "image".into(), mime: "image/gif".into(), bytes: Arc::from(png()) };
    let mut bad_version = job(TEXT, vec![prompt("chair")]);
    bad_version.params.insert("model_version".into(), json!("v9"));
    let mut huge_seed = job(TEXT, vec![prompt("chair")]);
    huge_seed.params.insert("model_seed".into(), json!(u64::MAX));
    let mut unknown = job(TEXT, vec![prompt("chair")]);
    unknown.params.insert("quad".into(), json!(true));
    let mut few_faces = job(TEXT, vec![prompt("chair")]);
    few_faces.params.insert("face_limit".into(), json!(10));
    let mut negative_on_image = job(IMAGE, vec![image("image")]);
    negative_on_image.params.insert("negative_prompt".into(), json!("blurry"));
    let cases = [
        job("tripo-v9", vec![prompt("chair")]),
        job(TEXT, vec![]),
        job(TEXT, vec![prompt("   ")]),
        job(TEXT, vec![prompt(&long)]),
        job(TEXT, vec![prompt("chair"), image("image")]),
        job(IMAGE, vec![]),
        job(IMAGE, vec![image("image"), image("image")]),
        job(IMAGE, vec![prompt("chair"), image("image")]),
        job(IMAGE, vec![jpeg_claim]),
        job(IMAGE, vec![gif]),
        job(MULTIVIEW, vec![image("front")]),
        job(MULTIVIEW, vec![image("left"), image("back")]),
        job(MULTIVIEW, vec![image("front"), image("front")]),
        job(MULTIVIEW, vec![image("front"), image("top")]),
        bad_version,
        huge_seed,
        unknown,
        few_faces,
        negative_on_image,
    ];
    for (n, j) in cases.iter().enumerate() {
        let http = Transport::new(vec![]);
        let scope = crate::SubmitContext::new();
        let error = block_on(Tripo::new(key(), http.clone()).submit_with_delivery(j, &scope)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "case {n}: {error:?}");
        assert!(scope.finish_error().is_some(), "case {n}: nothing was sent, so it was not accepted");
        assert!(http.requests().is_empty(), "case {n}");
    }
}

#[test]
fn a_failed_upload_proves_no_task_but_a_lost_create_reply_may_have_billed() {
    let http = Transport::new(vec![status(500, json!({"code": 1000, "message": "boom"}))]);
    let scope = crate::SubmitContext::new();
    let j = job(IMAGE, vec![image("image")]);
    assert_eq!(block_on(Tripo::new(key(), http).submit_with_delivery(&j, &scope)).unwrap_err().kind, ErrorKind::Server);
    assert!(scope.finish_error().is_some(), "an upload creates no task");

    let http = Transport::new(vec![ok(json!({"code": 0, "data": {"image_token": "tok"}})), Err(ProviderError::new(ErrorKind::Network, "reset"))]);
    let scope = crate::SubmitContext::new();
    let error = block_on(Tripo::new(key(), http).submit_with_delivery(&j, &scope)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("billed"), "{}", error.message);
    assert!(scope.finish_error().is_none(), "the task may exist");
    assert!(!Tripo::new(key(), Transport::new(vec![])).retry_ambiguous_submit());
}

#[test]
fn tripo_error_codes_map_to_shared_kinds_without_echoing_the_key() {
    for (http_status, code, kind) in [
        (401, 1002, ErrorKind::Auth),
        (400, 1004, ErrorKind::UnsupportedInput),
        (429, 1007, ErrorKind::RateLimited),
        (429, 2000, ErrorKind::RateLimited),
        (400, 2008, ErrorKind::BlockedContent),
        (403, 2010, ErrorKind::Budget),
        (400, 2015, ErrorKind::UnsupportedInput),
        (500, 1001, ErrorKind::Server),
        (502, 9999, ErrorKind::Server),
        (200, 2010, ErrorKind::Budget),
    ] {
        let reply = status(http_status, json!({"code": code, "message": format!("nope {KEY}"), "suggestion": "x"}));
        let error = block_on(Tripo::new(key(), Transport::new(vec![reply])).submit(&job(TEXT, vec![prompt("chair")]))).unwrap_err();
        assert_eq!(error.kind, kind, "code {code}");
        assert!(!error.message.contains(KEY), "{}", error.message);
        assert_eq!(error.retry_after_ms, Some(7_000));
    }
    // A reply that isn't JSON still fails by its status.
    let html = Ok(HttpResponse { status: 503, headers: vec![], body: b"<html>down</html>".to_vec() });
    let error = block_on(Tripo::new(key(), Transport::new(vec![html])).submit(&job(TEXT, vec![prompt("chair")]))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Busy);
}

#[test]
fn polling_reports_queue_progress_and_downloads_the_pbr_model_without_the_key() {
    let http = Transport::new(vec![
        ok(json!({"code": 0, "data": {"task_id": TASK, "status": "queued", "queuing_num": 3, "progress": 0}})),
        ok(json!({"code": 0, "data": {"task_id": TASK, "status": "running", "progress": 40, "queuing_num": -1}})),
        ok(
            json!({"code": 0, "data": {"task_id": TASK, "status": "success", "progress": 100, "output": {"model": "https://other.example.com/m.glb", "pbr_model": LINK}}}),
        ),
        file(GLB_FIXTURE),
    ]);
    let tripo = Tripo::new(key(), http.clone());
    let h = handle(TEXT);
    assert_eq!(block_on(tripo.poll(&h)).unwrap(), JobProgress::Queued { position: Some(3) });
    assert_eq!(block_on(tripo.poll(&h)).unwrap(), JobProgress::Running { fraction: Some(0.4), message: None, partial: None });
    let JobProgress::Done(done) = block_on(tripo.poll(&h)).unwrap() else { panic!("done") };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: GLB.into(), bytes: Arc::from(GLB_FIXTURE) }]);
    let sent = http.requests();
    assert_eq!(sent.first().unwrap().url, format!("https://api.tripo3d.ai/v2/openapi/task/{TASK}"));
    assert!(authorised(sent.first().unwrap()));
    let download = sent.last().unwrap();
    assert_eq!(download.url, LINK);
    assert!(download.headers.iter().all(|(_, v)| !v.contains(KEY)), "the key never goes to the storage host");
}

#[test]
fn terminal_failures_and_hostile_status_replies_are_errors_not_results() {
    let tripo = |reply| Tripo::new(key(), Transport::new(vec![reply]));
    for (state, kind) in [
        ("banned", ErrorKind::BlockedContent),
        ("failed", ErrorKind::JobFailed),
        ("expired", ErrorKind::JobFailed),
        ("cancelled", ErrorKind::Cancelled),
        ("unknown", ErrorKind::JobFailed),
    ] {
        let reply = ok(json!({"code": 0, "data": {"task_id": TASK, "status": state}}));
        match block_on(tripo(reply).poll(&handle(IMAGE))).unwrap() {
            JobProgress::Failed(error) => assert_eq!(error.kind, kind, "{state}"),
            other => panic!("{state}: {other:?}"),
        }
    }
    let success = |output: Value| ok(json!({"code": 0, "data": {"task_id": TASK, "status": "success", "output": output}}));
    for (n, reply) in [
        ok(json!({"code": 0, "data": {"task_id": "someone-else", "status": "queued"}})),
        ok(json!({"code": 0, "data": {"task_id": TASK, "status": "dancing"}})),
        ok(json!({"code": 0, "data": {"task_id": TASK}})),
        ok(json!({"code": 0})),
        success(json!({})),
        success(json!({"pbr_model": "http://cdn.example.com/m.glb"})),
        success(json!({"pbr_model": "https://127.0.0.1/m.glb"})),
        success(json!({"pbr_model": "https://localhost/m.glb"})),
        success(json!({"pbr_model": "https://localhost.localdomain/m.glb"})),
        success(json!({"pbr_model": "https://metadata.google.internal/m.glb"})),
        success(json!({"pbr_model": "https://printer.local/m.glb"})),
        success(json!({"pbr_model": "https://app.localhost./m.glb"})),
        success(json!({"pbr_model": "https://[::1]/m.glb"})),
        success(json!({"pbr_model": "https://user:pw@cdn.example.com/m.glb"})),
        success(json!({"pbr_model": "https://cdn.example.com:8443/m.glb"})),
        success(json!({"pbr_model": format!("https://cdn.example.com/{KEY}.glb")})),
        Ok(HttpResponse { status: 200, headers: vec![], body: b"not json".to_vec() }),
    ]
    .into_iter()
    .enumerate()
    {
        let error = block_on(tripo(reply).poll(&handle(IMAGE))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Malformed, "case {n}: {error:?}");
    }
    // A forged handle never reaches a URL.
    for h in [
        JobHandle { remote_id: "../../upload".into(), data: json!({"v": 1, "type": TEXT}) },
        JobHandle { remote_id: TASK.into(), data: json!({"v": 2, "type": TEXT}) },
        JobHandle { remote_id: TASK.into(), data: json!({"v": 1, "type": "rig"}) },
    ] {
        let http = Transport::new(vec![]);
        assert!(block_on(Tripo::new(key(), http.clone()).poll(&h)).is_err());
        assert!(http.requests().is_empty());
    }
}

#[test]
fn only_a_whole_glb_within_the_limit_is_accepted_as_the_model() {
    let finished = || ok(json!({"code": 0, "data": {"task_id": TASK, "status": "success", "output": {"pbr_model": LINK}}}));
    let mut truncated = GLB_FIXTURE.to_vec();
    truncated.pop();
    let mut version_one = GLB_FIXTURE.to_vec();
    version_one[4] = 1;
    for (n, bytes) in [truncated, version_one, b"glTF".to_vec(), b"<html>expired</html>".to_vec(), vec![]].into_iter().enumerate() {
        let error = block_on(Tripo::new(key(), Transport::new(vec![finished(), file(&bytes)])).poll(&handle(TEXT))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Malformed, "case {n}");
    }
    let expired = Ok(HttpResponse { status: 403, headers: vec![], body: b"AccessDenied".to_vec() });
    let error = block_on(Tripo::new(key(), Transport::new(vec![finished(), expired])).poll(&handle(TEXT))).unwrap_err();
    assert!(error.message.contains("billed"), "{}", error.message);
    // An expired link is retried (the next poll hands out a fresh one), never given up as permanent.
    assert!(error.is_transient(), "{error:?}");
    for status in [404, 500] {
        let failed = Ok(HttpResponse { status, headers: vec![], body: vec![] });
        let error = block_on(Tripo::new(key(), Transport::new(vec![finished(), failed])).poll(&handle(TEXT))).unwrap_err();
        assert!(error.is_transient(), "{status}: {error:?}");
    }
    let small = Limits { output_bytes: GLB_FIXTURE.len() - 1, ..Limits::HARD };
    let tripo = Tripo::with_limits(key(), Transport::new(vec![finished(), file(GLB_FIXTURE)]), small).unwrap();
    assert_eq!(block_on(tripo.poll(&handle(TEXT))).unwrap_err().kind, ErrorKind::Malformed);
    assert!(Tripo::with_limits(key(), Transport::new(vec![]), Limits { output_bytes: usize::MAX, ..Limits::HARD }).is_err());
}

#[test]
fn tripo_cannot_cancel_and_polls_patiently() {
    let tripo = Tripo::new(key(), Transport::new(vec![]));
    assert_eq!(block_on(tripo.cancel(&handle(TEXT))).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert_eq!(tripo.poll_interval_ms(&JobProgress::Queued { position: None }), 3_000);
    assert_eq!(tripo.id(), "tripo");
}

use super::*;
use crate::{HttpMethod, JobInput};
use futures::executor::block_on;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};

const KEY: &str = "msy-fixture-key-not-real";
const TASK: &str = "0193bfc5-ee4f-73f8-8525-44b398884ce9";
const GLB_FIXTURE: &[u8] = include_bytes!("../fal/fixtures/triangle.glb");
const LINK: &str = "https://assets.meshy.ai/tasks/0193bfc5/output/model.glb?Expires=1&Signature=abc";

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
fn mesh() -> JobInput {
    JobInput::Media { role: "mesh".into(), mime: GLB.into(), bytes: Arc::from(GLB_FIXTURE) }
}
fn png_uri() -> String {
    use base64::Engine as _;
    format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png()))
}
fn glb_uri() -> String {
    use base64::Engine as _;
    format!("data:application/octet-stream;base64,{}", base64::engine::general_purpose::STANDARD.encode(GLB_FIXTURE))
}
fn body(r: &HttpRequest) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}
fn authorised(r: &HttpRequest) -> bool {
    r.headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Bearer {KEY}")) && r.secret_headers.contains(&"authorization".to_owned())
}
fn created() -> Result<HttpResponse, ProviderError> {
    ok(json!({"result": TASK}))
}
fn handle(task: &str) -> JobHandle {
    JobHandle { remote_id: TASK.into(), data: json!({"v": 1, "type": task}) }
}
/// The one request a submit sends, and the handle it returned.
fn submitted(j: &ProviderJob) -> (HttpRequest, JobHandle) {
    let http = Transport::new(vec![created()]);
    let Submission::Queued(handle) = block_on(Meshy::new(key(), http.clone()).submit(j)).unwrap() else { panic!("queued") };
    let sent = http.requests();
    let [create] = sent.as_slice() else { panic!("one request, got {}", sent.len()) };
    (create.clone(), handle)
}

#[test]
fn a_text_task_asks_for_a_preview_mesh_and_queues_a_resumable_handle() {
    let mut j = job(TEXT, vec![prompt("a red chair")]);
    j.params.insert("ai_model".into(), json!("meshy-7.1"));
    j.params.insert("pose_mode".into(), json!("t-pose"));
    let (create, handle) = submitted(&j);
    assert_eq!(handle, self::handle(TEXT));
    assert_eq!((create.method, create.url.as_str()), (HttpMethod::Post, "https://api.meshy.ai/openapi/v2/text-to-3d"));
    assert!(authorised(&create));
    assert_eq!(body(&create), json!({"mode": "preview", "prompt": "a red chair", "ai_model": "meshy-7.1", "pose_mode": "t-pose", "target_formats": ["glb"]}));
    // No pose is the default, so it isn't sent.
    let mut j = job(TEXT, vec![prompt("a red chair")]);
    j.params.insert("pose_mode".into(), json!("none"));
    assert_eq!(body(&submitted(&j).0), json!({"mode": "preview", "prompt": "a red chair", "target_formats": ["glb"]}));
}

#[test]
fn image_tasks_send_each_image_inline_as_textured_pbr_with_views_in_meshys_order() {
    let (create, handle) = submitted(&job(IMAGE, vec![image("image")]));
    assert_eq!(handle.data, json!({"v": 1, "type": IMAGE}));
    assert_eq!(create.url, "https://api.meshy.ai/openapi/v1/image-to-3d");
    assert_eq!(
        body(&create),
        json!({"image_url": png_uri(), "should_texture": true, "enable_pbr": true, "texture_resolution": "2k", "target_formats": ["glb"]})
    );
    // Back then front wired: Meshy takes the front first, then the others in view order.
    let (create, _) = submitted(&job(MULTI_IMAGE, vec![image("back"), image("front")]));
    assert_eq!(create.url, "https://api.meshy.ai/openapi/v1/multi-image-to-3d");
    assert_eq!(body(&create)["image_urls"], json!([png_uri(), png_uri()]));
    let (create, _) = submitted(&job(MULTI_IMAGE, vec![image("right"), image("front"), image("left"), image("back")]));
    assert_eq!(body(&create)["image_urls"].as_array().map(Vec::len), Some(4));
}

#[test]
fn retexture_and_remesh_send_the_wired_glb_inline() {
    let mut j = job(RETEXTURE, vec![mesh(), prompt("weathered bronze")]);
    j.params.insert("uv".into(), json!("original"));
    j.params.insert("ai_model".into(), json!("latest"));
    let (create, _) = submitted(&j);
    assert_eq!(create.url, "https://api.meshy.ai/openapi/v1/retexture");
    assert_eq!(
        body(&create),
        json!({"model_url": glb_uri(), "text_style_prompt": "weathered bronze", "enable_original_uv": true, "ai_model": "latest", "enable_pbr": true, "texture_resolution": "2k", "target_formats": ["glb"]})
    );
    let (create, _) = submitted(&job(RETEXTURE, vec![image("style"), mesh()]));
    assert_eq!(body(&create)["image_style_url"], json!(png_uri()));
    assert!(body(&create).get("text_style_prompt").is_none());

    let mut j = job(REMESH, vec![mesh()]);
    j.params.insert("topology".into(), json!("quad"));
    j.params.insert("target_polycount".into(), json!(5000));
    let (create, _) = submitted(&j);
    assert_eq!(create.url, "https://api.meshy.ai/openapi/v1/remesh");
    assert_eq!(body(&create), json!({"model_url": glb_uri(), "topology": "quad", "target_polycount": 5000, "target_formats": ["glb"]}));
}

#[test]
fn jobs_meshy_cant_take_are_refused_before_anything_is_sent() {
    let long = "x".repeat(801);
    let jpeg_claim = JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes: Arc::from(png()) };
    let gif = JobInput::Media { role: "image".into(), mime: "image/gif".into(), bytes: Arc::from(png()) };
    let obj = JobInput::Media { role: "mesh".into(), mime: "model/obj".into(), bytes: Arc::from(&b"v 0 0 0\n"[..]) };
    let mut truncated = GLB_FIXTURE.to_vec();
    truncated.pop();
    let broken = JobInput::Media { role: "mesh".into(), mime: GLB.into(), bytes: Arc::from(truncated) };
    let with = |mut j: ProviderJob, name: &str, value: Value| {
        j.params.insert(name.into(), value);
        j
    };
    let cases = [
        job("meshy-9", vec![prompt("chair")]),
        job(TEXT, vec![]),
        job(TEXT, vec![prompt("   ")]),
        job(TEXT, vec![prompt(&long)]),
        job(TEXT, vec![prompt("chair"), prompt("table")]),
        job(TEXT, vec![prompt("chair"), image("image")]),
        job(IMAGE, vec![]),
        job(IMAGE, vec![image("image"), image("image")]),
        job(IMAGE, vec![prompt("chair"), image("image")]),
        job(IMAGE, vec![jpeg_claim]),
        job(IMAGE, vec![gif]),
        job(MULTI_IMAGE, vec![image("left"), image("back")]),
        job(MULTI_IMAGE, vec![image("front"), image("front")]),
        job(MULTI_IMAGE, vec![image("front"), image("top")]),
        job(RETEXTURE, vec![mesh()]),
        job(RETEXTURE, vec![prompt("bronze")]),
        job(RETEXTURE, vec![mesh(), prompt("bronze"), image("style")]),
        job(RETEXTURE, vec![mesh(), mesh(), prompt("bronze")]),
        job(RETEXTURE, vec![obj, prompt("bronze")]),
        job(REMESH, vec![]),
        job(REMESH, vec![broken]),
        job(REMESH, vec![mesh(), prompt("tidy")]),
        with(job(TEXT, vec![prompt("chair")]), "ai_model", json!("meshy-5")),
        with(job(TEXT, vec![prompt("chair")]), "pose_mode", json!("x-pose")),
        with(job(TEXT, vec![prompt("chair")]), "should_remesh", json!(true)),
        with(job(IMAGE, vec![image("image")]), "texture_resolution", json!("4k")),
        with(job(RETEXTURE, vec![mesh(), prompt("b")]), "ai_model", json!("meshy-7.1")),
        with(job(RETEXTURE, vec![mesh(), prompt("b")]), "uv", json!(true)),
        with(job(REMESH, vec![mesh()]), "target_polycount", json!(10)),
        with(job(REMESH, vec![mesh()]), "target_polycount", json!(300_001)),
        with(job(REMESH, vec![mesh()]), "topology", json!("hex")),
        with(job(REMESH, vec![mesh()]), "pose_mode", json!("a-pose")),
    ];
    for (n, j) in cases.iter().enumerate() {
        let http = Transport::new(vec![]);
        let scope = crate::SubmitContext::new();
        let error = block_on(Meshy::new(key(), http.clone()).submit_with_delivery(j, &scope)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "case {n}: {error:?}");
        assert!(scope.finish_error().is_some(), "case {n}: nothing was sent, so it was not accepted");
        assert!(http.requests().is_empty(), "case {n}");
    }
    // Lowered limits refuse what the hard ones accept.
    let small = Limits { image_bytes: 8, ..Limits::HARD };
    let meshy = Meshy::with_limits(key(), Transport::new(vec![]), small).unwrap();
    assert_eq!(block_on(meshy.submit(&job(IMAGE, vec![image("image")]))).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert!(Meshy::with_limits(key(), Transport::new(vec![]), Limits { model_bytes: usize::MAX, ..Limits::HARD }).is_err());
}

#[test]
fn a_lost_create_reply_may_have_billed_and_is_never_replayed() {
    let http = Transport::new(vec![Err(ProviderError::new(ErrorKind::Network, "reset"))]);
    let scope = crate::SubmitContext::new();
    let error = block_on(Meshy::new(key(), http).submit_with_delivery(&job(TEXT, vec![prompt("chair")]), &scope)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("billed"), "{}", error.message);
    assert!(scope.finish_error().is_none(), "the task may exist");
    assert!(!Meshy::new(key(), Transport::new(vec![])).retry_ambiguous_submit());
}

#[test]
fn http_errors_map_to_shared_kinds_without_echoing_the_key() {
    for (code, kind) in [
        (400, ErrorKind::UnsupportedInput),
        (401, ErrorKind::Auth),
        (402, ErrorKind::Budget),
        (403, ErrorKind::Auth),
        (404, ErrorKind::NotFound),
        (422, ErrorKind::UnsupportedInput),
        (429, ErrorKind::RateLimited),
        (500, ErrorKind::Server),
        (503, ErrorKind::Busy),
    ] {
        let reply = status(code, json!({"message": format!("nope {KEY}")}));
        let error = block_on(Meshy::new(key(), Transport::new(vec![reply])).submit(&job(TEXT, vec![prompt("chair")]))).unwrap_err();
        assert_eq!(error.kind, kind, "HTTP {code}");
        assert!(!error.message.contains(KEY), "{}", error.message);
        assert_eq!(error.retry_after_ms, Some(7_000));
    }
    let reply = status(402, json!({"message": "Insufficient funds\u{7}"}));
    let error = block_on(Meshy::new(key(), Transport::new(vec![reply])).submit(&job(TEXT, vec![prompt("chair")]))).unwrap_err();
    assert!(error.message.ends_with("Insufficient funds"), "{}", error.message);
    // A reply that isn't JSON still fails by its status.
    let html = Ok(HttpResponse { status: 503, headers: vec![("Retry-After".into(), "99999999".into())], body: b"<html>down</html>".to_vec() });
    let error = block_on(Meshy::new(key(), Transport::new(vec![html])).submit(&job(TEXT, vec![prompt("chair")]))).unwrap_err();
    assert_eq!((error.kind, error.retry_after_ms), (ErrorKind::Busy, None));
    // A receipt without a usable id is malformed.
    for receipt in [json!({}), json!({"result": ""}), json!({"result": "../v1/balance"}), json!({"result": KEY})] {
        let error = block_on(Meshy::new(key(), Transport::new(vec![ok(receipt.clone())])).submit(&job(TEXT, vec![prompt("chair")]))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Malformed, "{receipt}");
    }
}

#[test]
fn polling_reports_queue_progress_and_downloads_the_glb_without_the_key() {
    let http = Transport::new(vec![
        ok(json!({"id": TASK, "status": "PENDING", "progress": 0, "preceding_tasks": 3})),
        ok(json!({"id": TASK, "status": "IN_PROGRESS", "progress": 40})),
        ok(json!({"id": TASK, "status": "SUCCEEDED", "progress": 100, "model_urls": {"glb": LINK, "fbx": "https://assets.meshy.ai/m.fbx"}})),
        file(GLB_FIXTURE),
    ]);
    let meshy = Meshy::new(key(), http.clone());
    let h = handle(TEXT);
    assert_eq!(block_on(meshy.poll(&h)).unwrap(), JobProgress::Queued { position: Some(3) });
    assert_eq!(block_on(meshy.poll(&h)).unwrap(), JobProgress::Running { fraction: Some(0.4), message: None, partial: None });
    let JobProgress::Done(done) = block_on(meshy.poll(&h)).unwrap() else { panic!("done") };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: GLB.into(), bytes: Arc::from(GLB_FIXTURE) }]);
    let sent = http.requests();
    assert_eq!(sent.first().unwrap().url, format!("https://api.meshy.ai/openapi/v2/text-to-3d/{TASK}"));
    assert!(authorised(sent.first().unwrap()));
    let download = sent.last().unwrap();
    assert_eq!(download.url, LINK);
    assert!(download.headers.iter().all(|(_, v)| !v.contains(KEY)), "the key never goes to the storage host");
    // Each task type polls its own endpoint.
    let http = Transport::new(vec![ok(json!({"id": TASK, "status": "PENDING"}))]);
    block_on(Meshy::new(key(), http.clone()).poll(&handle(REMESH))).unwrap();
    assert_eq!(http.requests().first().unwrap().url, format!("https://api.meshy.ai/openapi/v1/remesh/{TASK}"));
}

#[test]
fn terminal_failures_and_hostile_status_replies_are_errors_not_results() {
    let meshy = |reply| Meshy::new(key(), Transport::new(vec![reply]));
    for (task, kind, said) in [
        (json!({"id": TASK, "status": "FAILED", "task_error": {"message": "The image has no clear subject"}}), ErrorKind::JobFailed, "no clear subject"),
        (json!({"id": TASK, "status": "FAILED", "task_error": {"message": format!("bad {KEY}")}}), ErrorKind::JobFailed, "couldn't make the model"),
        (json!({"id": TASK, "status": "FAILED"}), ErrorKind::JobFailed, "couldn't make the model"),
        (json!({"id": TASK, "status": "CANCELED"}), ErrorKind::Cancelled, "cancelled"),
    ] {
        match block_on(meshy(ok(task.clone())).poll(&handle(IMAGE))).unwrap() {
            JobProgress::Failed(error) => {
                assert_eq!(error.kind, kind, "{task}");
                assert!(error.message.contains(said) && !error.message.contains(KEY), "{}", error.message);
            }
            other => panic!("{task}: {other:?}"),
        }
    }
    let success = |urls: Value| ok(json!({"id": TASK, "status": "SUCCEEDED", "model_urls": urls}));
    for (n, reply) in [
        ok(json!({"id": "someone-else", "status": "PENDING"})),
        ok(json!({"status": "PENDING"})),
        ok(json!({"id": TASK, "status": "DANCING"})),
        ok(json!({"id": TASK})),
        success(json!({})),
        success(json!({"glb": ""})),
        success(json!({"glb": "http://assets.meshy.ai/m.glb"})),
        success(json!({"glb": "https://127.0.0.1/m.glb"})),
        success(json!({"glb": "https://localhost/m.glb"})),
        success(json!({"glb": "https://metadata.google.internal/m.glb"})),
        success(json!({"glb": "https://[::1]/m.glb"})),
        success(json!({"glb": "https://user:pw@assets.meshy.ai/m.glb"})),
        success(json!({"glb": "https://assets.meshy.ai:8443/m.glb"})),
        success(json!({"glb": format!("https://assets.meshy.ai/{KEY}.glb")})),
        Ok(HttpResponse { status: 200, headers: vec![], body: b"not json".to_vec() }),
    ]
    .into_iter()
    .enumerate()
    {
        let error = block_on(meshy(reply).poll(&handle(IMAGE))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Malformed, "case {n}: {error:?}");
    }
    // Hostile progress numbers stay a fraction.
    let reply = ok(json!({"id": TASK, "status": "IN_PROGRESS", "progress": 1e300}));
    assert_eq!(block_on(meshy(reply).poll(&handle(IMAGE))).unwrap(), JobProgress::Running { fraction: Some(1.0), message: None, partial: None });
    let reply = ok(json!({"id": TASK, "status": "PENDING", "preceding_tasks": u64::MAX}));
    assert_eq!(block_on(meshy(reply).poll(&handle(IMAGE))).unwrap(), JobProgress::Queued { position: None });
    // A forged handle never reaches a URL.
    for h in [
        JobHandle { remote_id: "../../balance".into(), data: json!({"v": 1, "type": TEXT}) },
        JobHandle { remote_id: TASK.into(), data: json!({"v": 2, "type": TEXT}) },
        JobHandle { remote_id: TASK.into(), data: json!({"v": 1, "type": "rig"}) },
    ] {
        let http = Transport::new(vec![]);
        assert!(block_on(Meshy::new(key(), http.clone()).poll(&h)).is_err());
        assert!(block_on(Meshy::new(key(), http.clone()).cancel(&h)).is_err());
        assert!(http.requests().is_empty());
    }
}

#[test]
fn only_a_whole_glb_within_the_limit_is_accepted_as_the_model() {
    let finished = || ok(json!({"id": TASK, "status": "SUCCEEDED", "model_urls": {"glb": LINK}}));
    let mut truncated = GLB_FIXTURE.to_vec();
    truncated.pop();
    let mut version_one = GLB_FIXTURE.to_vec();
    version_one[4] = 1;
    for (n, bytes) in [truncated, version_one, b"glTF".to_vec(), b"<html>expired</html>".to_vec(), vec![]].into_iter().enumerate() {
        let error = block_on(Meshy::new(key(), Transport::new(vec![finished(), file(&bytes)])).poll(&handle(TEXT))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Malformed, "case {n}");
    }
    for status in [403, 404, 500] {
        let failed = Ok(HttpResponse { status, headers: vec![], body: b"AccessDenied".to_vec() });
        let error = block_on(Meshy::new(key(), Transport::new(vec![finished(), failed])).poll(&handle(TEXT))).unwrap_err();
        assert!(error.message.contains("billed"), "{}", error.message);
        // The next poll hands out a fresh link, so a failed download is retried.
        assert!(error.is_transient(), "{status}: {error:?}");
    }
    let small = Limits { output_bytes: GLB_FIXTURE.len() - 1, ..Limits::HARD };
    let meshy = Meshy::with_limits(key(), Transport::new(vec![finished(), file(GLB_FIXTURE)]), small).unwrap();
    assert_eq!(block_on(meshy.poll(&handle(TEXT))).unwrap_err().kind, ErrorKind::Malformed);
}

#[test]
fn cancelling_is_refused_without_a_request() {
    // Meshy's DELETE erases the task and its models; nothing confirms a refund.
    let http = Transport::new(vec![]);
    let error = block_on(Meshy::new(key(), http.clone()).cancel(&handle(IMAGE))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnsupportedInput);
    assert!(error.message.contains("billed"), "{}", error.message);
    assert!(http.requests().is_empty());
    let meshy = Meshy::new(key(), Transport::new(vec![]));
    assert_eq!(meshy.poll_interval_ms(&JobProgress::Queued { position: None }), 3_000);
    assert_eq!(meshy.id(), "meshy");
}

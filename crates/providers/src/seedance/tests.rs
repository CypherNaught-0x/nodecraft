use super::*;
use crate::{HttpMethod, JobInput};
use futures::executor::block_on;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};

const KEY: &str = "seedance-fixture-key-not-real";
const TASK: &str = "cgt-20261023101530-7xk2p";
const LINK: &str = "https://ark-content-generation-ap-southeast-1.tos-ap-southeast-1.bytepluses.com/video.mp4?X-Tos-Signature=abc";

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
fn refusal(code: u16, error: &str) -> Result<HttpResponse, ProviderError> {
    status(code, json!({"error": {"code": error, "message": "no", "type": "BadRequest"}}))
}
fn file(bytes: &[u8]) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: bytes.to_vec() })
}
fn png(w: u32, h: u32) -> Vec<u8> {
    let pixels = vec![120u8; (w * h * 4) as usize];
    nodecraft_media::encode_png(w, h, &pixels, &nodecraft_media::Limits::default()).unwrap()
}
fn mp4() -> Vec<u8> {
    let mut bytes = b"\0\0\0\x20ftypisom\0\0\x02\0isomiso2avc1mp41".to_vec();
    bytes.resize(64, 0);
    bytes
}
fn job(model: &str, inputs: Vec<JobInput>) -> ProviderJob {
    ProviderJob { model: model.into(), params: Default::default(), inputs, idempotency_key: "fixture-operation".into() }
}
fn prompt(text: &str) -> JobInput {
    JobInput::Text { role: "prompt".into(), text: text.into() }
}
fn image(role: &str) -> JobInput {
    JobInput::Media { role: role.into(), mime: "image/png".into(), bytes: Arc::from(png(320, 320)) }
}
fn body(r: &HttpRequest) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}
fn authorised(r: &HttpRequest) -> bool {
    r.headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Bearer {KEY}")) && r.secret_headers.contains(&"authorization".to_owned())
}
fn created() -> Result<HttpResponse, ProviderError> {
    ok(json!({"id": TASK}))
}
fn handle() -> JobHandle {
    JobHandle { remote_id: TASK.into(), data: json!({"v": 1, "model": V2_0}) }
}
fn task(state: &str) -> Result<HttpResponse, ProviderError> {
    ok(json!({"id": TASK, "model": V2_0, "status": state, "error": null}))
}
fn succeeded() -> Result<HttpResponse, ProviderError> {
    ok(
        json!({"id": TASK, "model": V2_0, "status": "succeeded", "content": {"video_url": LINK}, "usage": {"completion_tokens": 108_680, "total_tokens": 108_680}, "duration": 5}),
    )
}
fn submit(p: &Seedance, j: &ProviderJob) -> JobHandle {
    match block_on(p.submit(j)).unwrap() {
        Submission::Queued(handle) => handle,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_text_task_sends_every_option_explicitly_and_queues_a_resumable_handle() {
    let http = Transport::new(vec![created()]);
    let mut j = job(V2_0, vec![prompt("a paper boat in the rain")]);
    j.params.insert("resolution".into(), json!("1080p"));
    j.params.insert("ratio".into(), json!("21:9"));
    j.params.insert("duration_seconds".into(), json!("12"));
    j.params.insert("generate_audio".into(), json!(false));
    j.params.insert("seed".into(), json!("42"));
    let handle = submit(&Seedance::new(key(), http.clone()), &j);
    assert_eq!(handle, JobHandle { remote_id: TASK.into(), data: json!({"v": 1, "model": V2_0}) });
    let sent = http.requests();
    let [create] = sent.as_slice() else { panic!("one request, got {}", sent.len()) };
    assert_eq!((create.method, create.url.as_str()), (HttpMethod::Post, "https://ark.ap-southeast.bytepluses.com/api/v3/contents/generations/tasks"));
    assert!(authorised(create));
    assert!(create.headers.iter().any(|(n, v)| n == "User-Agent" && v == crate::USER_AGENT));
    assert_eq!(
        body(create),
        json!({"model": V2_0, "content": [{"type": "text", "text": "a paper boat in the rain"}], "resolution": "1080p", "ratio": "21:9",
            "duration": 12, "generate_audio": false, "watermark": false, "seed": 42})
    );
    // Nothing set: 720p, the model's choice of shape, five seconds with sound. Never -1.
    let http = Transport::new(vec![created()]);
    submit(&Seedance::new(key(), http.clone()), &job(V2_5, vec![prompt("a boat")]));
    let sent = body(http.requests().first().unwrap());
    assert_eq!(
        (&sent["resolution"], &sent["ratio"], &sent["duration"], &sent["generate_audio"]),
        (&json!("720p"), &json!("adaptive"), &json!(5), &json!(true))
    );
    assert!(sent.get("seed").is_none());
}

#[test]
fn frames_and_references_go_inline_as_data_urls_with_their_roles() {
    let http = Transport::new(vec![created()]);
    submit(&Seedance::new(key(), http.clone()), &job(V2_0_FAST, vec![image("last_frame"), prompt("a boat"), image("first_frame")]));
    let content = body(http.requests().first().unwrap())["content"].clone();
    let roles: Vec<&str> = content.as_array().unwrap().iter().skip(1).map(|c| c["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["first_frame", "last_frame"], "the first frame goes first whatever the wiring order");
    let url = content[1]["image_url"]["url"].as_str().unwrap();
    assert!(url.starts_with("data:image/png;base64,iVBOR"), "{}", url.get(..40).unwrap_or(url));
    assert_eq!(content[1]["type"], "image_url");

    let http = Transport::new(vec![created()]);
    let mut inputs = vec![prompt("the boat and the duck")];
    inputs.extend((0..9).map(|_| image("reference")));
    submit(&Seedance::new(key(), http.clone()), &job(V2_0, inputs));
    let content = body(http.requests().first().unwrap())["content"].clone();
    assert_eq!(content.as_array().unwrap().len(), 10);
    assert!(content.as_array().unwrap().iter().skip(1).all(|c| c["role"] == "reference_image"));
}

#[test]
fn jobs_seedance_cant_take_are_refused_before_anything_is_sent() {
    let with = |model: &str, inputs: Vec<JobInput>, name: &str, value: Value| {
        let mut j = job(model, inputs);
        j.params.insert(name.into(), value);
        j
    };
    let p = || vec![prompt("a boat")];
    let media = |mime: &str, bytes: Vec<u8>| JobInput::Media { role: "first_frame".into(), mime: mime.into(), bytes: Arc::from(bytes) };
    let mut ten_refs = p();
    ten_refs.extend((0..10).map(|_| image("reference")));
    let mut thirty_one = p();
    thirty_one.extend((0..31).map(|_| image("reference")));
    let cases = [
        job("seedance-1-0-pro-250528", p()),
        job(V2_0, vec![]),
        job(V2_0, vec![prompt("  ")]),
        job(V2_0, vec![prompt(&"x".repeat(8_001))]),
        job(V2_0, vec![prompt("a"), prompt("b")]),
        job(V2_0, vec![image("first_frame")]),
        job(V2_0, vec![prompt("a"), image("last_frame")]),
        job(V2_0, vec![prompt("a"), image("first_frame"), image("reference")]),
        job(V2_0, vec![prompt("a"), image("first_frame"), image("first_frame")]),
        job(V2_0, vec![prompt("a"), image("video")]),
        job(V2_0, ten_refs),
        job(V2_5, thirty_one),
        job(V2_0, vec![prompt("a"), media("image/jpeg", png(320, 320))]),
        job(V2_0, vec![prompt("a"), media("image/gif", png(320, 320))]),
        job(V2_0, vec![prompt("a"), media("image/png", png(200, 320))]),
        job(V2_0, vec![prompt("a"), media("image/png", png(900, 320))]),
        job(V2_0, vec![prompt("a"), media("image/png", vec![])]),
        with(V2_5, vec![prompt("a"), image("first_frame")], "ratio", json!("16:9")),
        with(V2_0, p(), "resolution", json!("1440p")),
        with(V2_0_MINI, p(), "resolution", json!("1080p")),
        with(V2_5, p(), "resolution", json!("4k")),
        with(V2_0, p(), "ratio", json!("2:1")),
        with(V2_0, p(), "duration_seconds", json!("16")),
        with(V2_5, p(), "duration_seconds", json!("31")),
        with(V2_0, p(), "duration_seconds", json!("3")),
        with(V2_0, p(), "duration_seconds", json!(-1)),
        with(V2_0, p(), "generate_audio", json!("loud")),
        with(V2_0, p(), "seed", json!(1u64 << 31)),
        with(V2_0, p(), "camera_fixed", json!(true)),
        with(V2_0, p(), "callback_url", json!("https://example.com/hook")),
    ];
    for (n, j) in cases.iter().enumerate() {
        let http = Transport::new(vec![]);
        let scope = crate::SubmitContext::new();
        let error = block_on(Seedance::new(key(), http.clone()).submit_with_delivery(j, &scope)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "case {n}: {error:?}");
        assert!(scope.finish_error().is_some(), "case {n}: nothing was sent, so it was not accepted");
        assert!(http.requests().is_empty(), "case {n}");
    }
    // The longest clips and widest shapes each model takes are accepted.
    for j in [
        with(V2_5, p(), "duration_seconds", json!("30")),
        with(V2_0, p(), "resolution", json!("4k")),
        with(V2_0, p(), "seed", json!(i32::MAX)),
        job(V2_0, vec![prompt("a"), media("image/png", png(800, 320))]),
        with(V2_5, vec![prompt("a"), image("first_frame")], "ratio", json!("adaptive")),
    ] {
        submit(&Seedance::new(key(), Transport::new(vec![created()])), &j);
    }
}

#[test]
fn a_documented_refusal_of_the_create_proves_no_task_but_other_failures_may_have_billed() {
    let create = |reply| {
        let scope = crate::SubmitContext::new();
        let error = block_on(Seedance::new(key(), Transport::new(vec![reply])).submit_with_delivery(&job(V2_0, vec![prompt("a boat")]), &scope)).unwrap_err();
        (error, scope.finish_error().is_some())
    };
    for (at, code, kind) in [
        (400, "InvalidParameter", ErrorKind::UnsupportedInput),
        (400, "InputImageSensitiveContentDetected.PrivacyInformation", ErrorKind::BlockedContent),
        (400, "InputTextSensitiveContentDetected", ErrorKind::BlockedContent),
        (401, "AuthenticationError", ErrorKind::Auth),
        (403, "AccountOverdueError", ErrorKind::Budget),
        (404, "ModelNotOpen", ErrorKind::Auth),
        (429, "SetLimitExceeded", ErrorKind::Budget),
        (429, "RateLimitExceeded.EndpointRPMExceeded", ErrorKind::RateLimited),
        (429, "ServerOverloaded", ErrorKind::Busy),
    ] {
        let (error, refused) = create(refusal(at, code));
        assert_eq!(error.kind, kind, "{code}");
        assert!(refused, "{code}: ModelArk created no task");
        assert!(error.message.contains(code) && !error.message.contains("billed"), "{}", error.message);
        assert_eq!(error.retry_after_ms, Some(7_000));
    }
    for (n, reply) in [
        refusal(500, "InternalServiceError"),
        refusal(502, "InvalidParameter"),
        refusal(400, "SomethingNewThatMayHaveQueued"),
        refusal(403, "MCPNetworkDenied"),
        refusal(429, "BillingPending"),
        Err(ProviderError::new(ErrorKind::Timeout, "lost")),
        Ok(HttpResponse { status: 429, headers: vec![], body: b"<html>slow down</html>".to_vec() }),
        status(400, json!({"error": {"message": "no code"}})),
        ok(json!({"no": "id"})),
    ]
    .into_iter()
    .enumerate()
    {
        let (error, refused) = create(reply);
        assert!(!refused, "case {n}: the task may exist");
        assert!(error.message.contains("billed") || error.kind == ErrorKind::Malformed, "case {n}: {}", error.message);
    }
    let small = Seedance::with_limits(key(), Transport::new(vec![refusal(429, &"x".repeat(64))]), Limits { response_bytes: 32, ..Limits::HARD }).unwrap();
    let scope = crate::SubmitContext::new();
    assert!(block_on(small.submit_with_delivery(&job(V2_0, vec![prompt("a boat")]), &scope)).is_err());
    assert!(scope.finish_error().is_none(), "an oversized refusal isn't read");
    assert!(!Seedance::new(key(), Transport::new(vec![])).retry_ambiguous_submit());
}

#[test]
fn errors_never_echo_the_key() {
    let echo = status(401, json!({"error": {"code": "AuthenticationError", "message": format!("bad key {KEY}")}}));
    let error = block_on(Seedance::new(key(), Transport::new(vec![echo])).submit(&job(V2_0, vec![prompt("a boat")]))).unwrap_err();
    assert!(!error.message.contains(KEY) && error.message.contains("withheld"), "{}", error.message);
    let echo = ok(json!({"id": TASK, "status": "failed", "error": {"code": "X", "message": KEY}}));
    let JobProgress::Failed(error) = block_on(Seedance::new(key(), Transport::new(vec![echo])).poll(&handle())).unwrap() else { panic!("failed") };
    assert!(!error.message.contains(KEY), "{}", error.message);
    let echo = ok(json!({"id": format!("cgt-{KEY}")}));
    assert_eq!(block_on(Seedance::new(key(), Transport::new(vec![echo])).submit(&job(V2_0, vec![prompt("a")]))).unwrap_err().kind, ErrorKind::Malformed);
}

#[test]
fn polling_reports_progress_and_downloads_the_video_without_the_key() {
    let http = Transport::new(vec![task("queued"), task("running"), succeeded(), file(&mp4())]);
    let p = Seedance::new(key(), http.clone());
    assert_eq!(block_on(p.poll(&handle())).unwrap(), JobProgress::Queued { position: None });
    assert!(matches!(block_on(p.poll(&handle())).unwrap(), JobProgress::Running { fraction: None, .. }));
    let JobProgress::Done(done) = block_on(p.poll(&handle())).unwrap() else { panic!("done") };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: MP4.into(), bytes: Arc::from(mp4()) }]);
    assert_eq!(done.usage, Usage { output_tokens: Some(108_680), ..Usage::default() }, "tokens are evidence, the cost is the quote");
    let sent = http.requests();
    let poll = sent.first().unwrap();
    assert_eq!(
        (poll.method, poll.url.as_str()),
        (HttpMethod::Get, format!("https://ark.ap-southeast.bytepluses.com/api/v3/contents/generations/tasks/{TASK}").as_str())
    );
    assert!(authorised(poll));
    let download = sent.last().unwrap();
    assert_eq!(download.url, LINK);
    assert!(download.headers.iter().all(|(_, v)| !v.contains(KEY)) && download.secret_headers.is_empty(), "the key never goes to the storage host");
}

#[test]
fn terminal_failures_and_hostile_status_replies_are_errors_not_results() {
    let seedance = |reply| Seedance::new(key(), Transport::new(vec![reply]));
    for (state, error, kind) in [
        ("failed", json!({"code": "OutputVideoSensitiveContentDetected", "message": "no"}), ErrorKind::BlockedContent),
        ("failed", json!({"code": "InvalidParameter.TaskTypeConstraint", "message": "no"}), ErrorKind::JobFailed),
        ("expired", json!(null), ErrorKind::JobFailed),
        ("cancelled", json!(null), ErrorKind::Cancelled),
    ] {
        let reply = ok(json!({"id": TASK, "status": state, "error": error}));
        match block_on(seedance(reply).poll(&handle())).unwrap() {
            JobProgress::Failed(error) => assert_eq!(error.kind, kind, "{state}"),
            other => panic!("{state}: {other:?}"),
        }
    }
    let done = |content: Value| ok(json!({"id": TASK, "status": "succeeded", "content": content}));
    for (n, reply) in [
        ok(json!({"id": "cgt-someone-else", "status": "queued"})),
        ok(json!({"id": TASK, "status": "dancing"})),
        ok(json!({"id": TASK})),
        done(json!({})),
        done(json!({"video_url": ""})),
        done(json!({"video_url": "http://cdn.example.com/v.mp4"})),
        done(json!({"video_url": "https://169.254.169.254/v.mp4"})),
        done(json!({"video_url": "https://metadata.google.internal/v.mp4"})),
        done(json!({"video_url": "https://cdn.example.com:8443/v.mp4"})),
        done(json!({"video_url": format!("https://cdn.example.com/{KEY}.mp4")})),
        Ok(HttpResponse { status: 200, headers: vec![], body: b"not json".to_vec() }),
    ]
    .into_iter()
    .enumerate()
    {
        let error = block_on(seedance(reply).poll(&handle())).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Malformed, "case {n}: {error:?}");
    }
    let gone = refusal(404, "NotFound.");
    assert_eq!(block_on(seedance(gone).poll(&handle())).unwrap_err().kind, ErrorKind::NotFound);
    for h in [
        JobHandle { remote_id: "../../images/generations".into(), data: json!({"v": 1, "model": V2_0}) },
        JobHandle { remote_id: TASK.into(), data: json!({"v": 2, "model": V2_0}) },
        JobHandle { remote_id: TASK.into(), data: json!({"v": 1, "model": "seedream-4-5-251128"}) },
    ] {
        let http = Transport::new(vec![]);
        assert!(block_on(Seedance::new(key(), http.clone()).poll(&h)).is_err());
        assert!(block_on(Seedance::new(key(), http.clone()).cancel(&h)).is_err());
        assert!(http.requests().is_empty());
    }
}

#[test]
fn only_an_mp4_within_the_limit_is_accepted_and_a_failed_download_is_retried() {
    for (n, bytes) in [b"<html>expired</html>".to_vec(), vec![]].into_iter().enumerate() {
        let error = block_on(Seedance::new(key(), Transport::new(vec![succeeded(), file(&bytes)])).poll(&handle())).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Malformed, "case {n}");
    }
    for code in [403, 404, 500] {
        let failed = Ok(HttpResponse { status: code, headers: vec![], body: vec![] });
        let error = block_on(Seedance::new(key(), Transport::new(vec![succeeded(), failed])).poll(&handle())).unwrap_err();
        assert!(error.is_transient() && error.message.contains("billed"), "{code}: {error:?}");
    }
    let small = Limits { output_bytes: mp4().len() - 1, ..Limits::HARD };
    let p = Seedance::with_limits(key(), Transport::new(vec![succeeded(), file(&mp4())]), small).unwrap();
    assert_eq!(block_on(p.poll(&handle())).unwrap_err().kind, ErrorKind::Malformed);
    assert!(Seedance::with_limits(key(), Transport::new(vec![]), Limits { output_bytes: usize::MAX, ..Limits::HARD }).is_err());
}

#[test]
fn only_a_queued_task_is_deleted_so_a_finished_video_is_never_erased() {
    let http = Transport::new(vec![task("queued"), ok(json!({}))]);
    block_on(Seedance::new(key(), http.clone()).cancel(&handle())).unwrap();
    let sent = http.requests();
    let [read, delete] = sent.as_slice() else { panic!("two requests") };
    assert_eq!(read.method, HttpMethod::Get);
    assert_eq!((delete.method, delete.url.as_str()), (HttpMethod::Delete, format!("{API}/{TASK}").as_str()));
    assert!(authorised(delete));

    for reply in [task("running"), succeeded()] {
        let http = Transport::new(vec![reply]);
        assert_eq!(block_on(Seedance::new(key(), http.clone()).cancel(&handle())).unwrap_err().kind, ErrorKind::UnsupportedInput);
        assert_eq!(http.requests().len(), 1, "nothing is deleted");
    }
    for state in ["failed", "cancelled", "expired"] {
        let http = Transport::new(vec![task(state)]);
        block_on(Seedance::new(key(), http.clone()).cancel(&handle())).unwrap();
        assert_eq!(http.requests().len(), 1, "{state}: already stopped");
    }
    let http = Transport::new(vec![task("queued"), refusal(400, "OperationDenied.UnsupportedPhase")]);
    assert!(block_on(Seedance::new(key(), http).cancel(&handle())).is_err(), "a task that started meanwhile stays");
    let p = Seedance::new(key(), Transport::new(vec![]));
    assert_eq!((p.id(), p.poll_interval_ms(&JobProgress::Queued { position: None })), ("seedance", 5_000));
}

use super::*;
use crate::{HttpMethod, JobInput, SubmitContext};
use futures::executor::block_on;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex;

const KEY: &str = "public-bfl-fixture";
const ID: &str = "public_task_33";
const POLL: &str = "https://api.eu1.bfl.ai/v1/get_result?id=public_task_33";
const IMAGE: &str = "https://delivery.eu1.bfl.ai/image.png?signature=public";
struct Wire {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
}
impl Wire {
    fn new(replies: impl IntoIterator<Item = HttpResponse>) -> Arc<Self> {
        Arc::new(Self { requests: Mutex::default(), replies: Mutex::new(replies.into_iter().map(Ok).collect()) })
    }
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable transport")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front().expect("unexpected request");
        Box::pin(async move { reply })
    }
}
fn provider(wire: Arc<Wire>) -> Bfl {
    Bfl::new(Secret::new(KEY).unwrap(), wire)
}
fn reply(value: serde_json::Value) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&value).unwrap() }
}
fn receipt() -> HttpResponse {
    reply(json!({"id":ID,"polling_url":POLL,"cost":1.4}))
}
fn status(value: &str) -> HttpResponse {
    reply(json!({"id":ID,"status":value,"result":{},"progress":0.5}))
}
fn ready(url: &str) -> HttpResponse {
    reply(json!({"id":ID,"status":"Ready","result":{"sample":url},"cost":1.4}))
}
fn png() -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: vec![],
        body: nodecraft_media::encode_png(64, 64, &vec![128; 64 * 64 * 4], &nodecraft_media::Limits::default()).unwrap(),
    }
}
fn job() -> ProviderJob {
    ProviderJob {
        model: MODEL.into(),
        params: json!({"width":64,"height":64,"seed":42}).as_object().unwrap().clone(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A public harbour".into() }],
        idempotency_key: "public-key".into(),
    }
}
fn handle() -> JobHandle {
    JobHandle { remote_id: ID.into(), data: json!({"v":1,"polling_url":POLL,"width":64,"height":64}) }
}

#[test]
fn bfl33_underscore_task_queue_resume_download_and_credential_boundary() {
    let wire = Wire::new([receipt(), status("Pending"), status("Reasoning"), status("Generating"), ready(IMAGE), png()]);
    let p = provider(wire.clone());
    let Submission::Queued(handle) = block_on(p.submit(&job())).unwrap() else { panic!("queue") };
    let saved = serde_json::to_string(&handle).unwrap();
    assert!(!saved.contains(KEY));
    let restored: JobHandle = serde_json::from_str(&saved).unwrap();
    assert_eq!(restored.remote_id, ID);
    assert!(matches!(block_on(p.poll(&restored)).unwrap(), JobProgress::Queued { .. }));
    for _ in 0..2 {
        assert!(matches!(block_on(p.poll(&restored)).unwrap(), JobProgress::Running { fraction: None, .. }));
    }
    let JobProgress::Done(done) = block_on(p.poll(&restored)).unwrap() else { panic!("done") };
    assert_eq!(done.usage, crate::Usage::default(), "credits are not silently converted to dollars");
    assert_eq!(done.outputs, [Output::Bytes { mime: "image/png".into(), bytes: Arc::from(png().body) }]);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    let create = requests.first().unwrap();
    assert_eq!((create.method, create.url.as_str()), (HttpMethod::Post, API));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&create.body).unwrap(),
        json!({"prompt":"A public harbour","width":64,"height":64,"seed":42,"output_format":"png"})
    );
    for request in requests.iter().take(5) {
        assert_eq!(request.secret_headers, ["x-key"]);
        assert!(request.headers.iter().any(|(name, value)| name == "x-key" && value == KEY));
        assert_eq!(request.max_response_bytes, METADATA_BYTES);
        assert!(!format!("{request:?}").contains(KEY));
    }
    for request in requests.iter().skip(1).take(4) {
        assert_eq!(request.url, POLL);
    }
    let download = requests.last().unwrap();
    assert_eq!(download.url, IMAGE);
    assert!(download.headers.is_empty());
    assert_eq!(download.max_response_bytes, OUTPUT_BYTES);
    assert!(!p.retry_ambiguous_submit());
}

#[test]
fn bfl33_invalid_input_stays_unsent() {
    let mut cases = vec![];
    for (name, value) in [
        ("width", json!(65)),
        ("height", json!(0)),
        ("width", json!(u64::MAX)),
        ("seed", json!(4294967296u64)),
        ("seed", json!(-1)),
        ("webhook_url", json!("https://example.test")),
    ] {
        let mut j = job();
        j.params.insert(name.into(), value);
        cases.push(j);
    }
    let mut j = job();
    j.params = json!({"width":2048,"height":2048}).as_object().unwrap().clone();
    cases.push(j);
    let mut j = job();
    j.model = "unknown".into();
    cases.push(j);
    let mut j = job();
    j.idempotency_key = "x".repeat(257);
    cases.push(j);
    for text in ["".into(), "x".repeat(16385), "bad\0text".into()] {
        let mut j = job();
        j.inputs = vec![JobInput::Text { role: "prompt".into(), text }];
        cases.push(j);
    }
    let mut j = job();
    j.inputs.clear();
    cases.push(j);
    let mut j = job();
    j.inputs.push(JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(png().body) });
    cases.push(j);
    let wire = Wire::new([]);
    let p = provider(wire.clone());
    for j in cases {
        let scope = SubmitContext::new();
        assert!(block_on(p.submit_with_delivery(&j, &scope)).is_err());
        assert!(scope.finish_error().is_some());
    }
    let scope = SubmitContext::new();
    let p = Bfl::new(Secret::new(&"k".repeat(257)).unwrap(), wire.clone());
    assert!(block_on(p.submit_with_delivery(&job(), &scope)).is_err());
    assert!(scope.finish_error().is_some());
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[test]
fn bfl33_forged_receipts_and_saved_handles_cannot_send_keys_elsewhere() {
    for url in [
        "https://evil.test/v1/get_result?id=public_task_33",
        "http://api.bfl.ai/v1/get_result?id=public_task_33",
        "https://api.bfl.ai.evil.test/v1/get_result?id=public_task_33",
        "https://api.foo.bar.bfl.ai/v1/get_result?id=public_task_33",
        "https://api.eu1.bfl.ai:8443/v1/get_result?id=public_task_33",
        "https://u@api.bfl.ai/v1/get_result?id=public_task_33",
        "https://api.bfl.ai/v1/get_result?id=other",
        "https://api.bfl.ai/v1/get_result?id=public_task_33&extra=1",
        "https://api.bfl.ai/v1/get_result?id=public_task_33#fragment",
        "https://api.bfl.ai/v1/get_result?id=public-bfl-fixture",
        "https://api.bfl.ai/v1/other?id=public_task_33",
        "https://127.0.0.1/v1/get_result?id=public_task_33",
    ] {
        let wire = Wire::new([reply(json!({"id":ID,"polling_url":url}))]);
        let p = provider(wire.clone());
        assert!(block_on(p.submit(&job())).is_err(), "{url}");
        let mut h = handle();
        h.data["polling_url"] = json!(url);
        assert!(block_on(p.poll(&h)).is_err(), "{url}");
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
    for id in ["", "../private", KEY] {
        let wire = Wire::new([]);
        let mut h = handle();
        h.remote_id = id.into();
        assert!(block_on(provider(wire).poll(&h)).is_err());
    }
}

#[test]
fn bfl33_output_origins_dimensions_and_corruption_are_checked() {
    for url in [
        "https://evil.test/image.png",
        "https://delivery.eu1.bfl.ai.evil.test/image.png",
        "https://delivery.a.b.bfl.ai/image.png",
        "https://delivery.eu1.bfl.ai:8443/image.png",
        "http://delivery.eu1.bfl.ai/image.png",
        "https://127.0.0.1/image.png",
        "https://u:password@delivery.eu1.bfl.ai/image.png",
        "https://delivery.eu1.bfl.ai/image.png?key=public-bfl-fixture",
    ] {
        let wire = Wire::new([ready(url)]);
        assert!(block_on(provider(wire.clone()).poll(&handle())).is_err());
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
    let mut wrong = png();
    wrong.body = nodecraft_media::encode_png(32, 32, &vec![128; 32 * 32 * 4], &nodecraft_media::Limits::default()).unwrap();
    let mut corrupt = png();
    corrupt.body.truncate(32);
    let oversized = HttpResponse { body: vec![0; OUTPUT_BYTES + 1], ..png() };
    for bad in [wrong, corrupt, oversized] {
        assert!(block_on(provider(Wire::new([ready(IMAGE), bad])).poll(&handle())).is_err());
    }
    let wire = Wire::new([ready(IMAGE), HttpResponse { status: 403, body: KEY.as_bytes().to_vec(), headers: vec![] }]);
    assert_eq!(block_on(provider(wire).poll(&handle())).unwrap_err().kind, ErrorKind::Server);
}

#[test]
fn bfl33_terminal_states_and_malformed_responses() {
    for (state, kind) in [
        ("Request Moderated", ErrorKind::BlockedContent),
        ("Content Moderated", ErrorKind::BlockedContent),
        ("Task not found", ErrorKind::NotFound),
        ("No Card Generated", ErrorKind::JobFailed),
        ("Error", ErrorKind::JobFailed),
    ] {
        let JobProgress::Failed(error) = block_on(provider(Wire::new([status(state)])).poll(&handle())).unwrap() else { panic!("failed") };
        assert_eq!(error.kind, kind);
    }
    for response in [
        status("unknown"),
        reply(json!({"id":"other","status":"Pending"})),
        reply(json!({"id":ID,"status":"Ready","result":{}})),
        HttpResponse { body: vec![b' '; METADATA_BYTES + 1], ..status("Pending") },
    ] {
        assert!(block_on(provider(Wire::new([response])).poll(&handle())).is_err());
    }
    let duplicate = HttpResponse { body: format!("{{\"id\":\"{ID}\",\"id\":\"other\",\"polling_url\":\"{POLL}\"}}").into_bytes(), ..receipt() };
    assert!(block_on(provider(Wire::new([duplicate])).submit(&job())).is_err());
}

#[test]
fn bfl33_lost_reply_error_redaction_and_unconfirmed_cancel() {
    for code in [202, 302, 400, 401, 402, 403, 404, 408, 422, 429, 500, 503] {
        let wire = Wire::new([HttpResponse { status: code, headers: vec![("retry-after".into(), "2".into())], body: KEY.as_bytes().to_vec() }]);
        let scope = SubmitContext::new();
        let error = block_on(provider(wire).submit_with_delivery(&job(), &scope)).unwrap_err();
        assert!(!error.message.contains(KEY));
        assert_eq!(error.retry_after_ms, Some(2000));
        assert!(scope.finish_error().is_none());
    }
    let wire = Wire::new([]);
    wire.replies.lock().unwrap().push_back(Err(ProviderError::new(ErrorKind::Network, KEY)));
    let p = provider(wire.clone());
    let scope = SubmitContext::new();
    let error = block_on(p.submit_with_delivery(&job(), &scope)).unwrap_err();
    assert!(!error.message.contains(KEY));
    assert!(scope.finish_error().is_none());
    assert!(!p.retry_ambiguous_submit());
    assert_eq!(block_on(p.cancel(&handle())).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[test]
fn bfl33_catalog_selects_direct_text_to_image_with_unknown_price() {
    use nodecraft_catalog::{Catalog, Pricing, Wiring};
    use nodecraft_types::Modality;
    let catalog = Catalog::builtin().unwrap();
    let model = catalog.get("bfl.flux-2-klein-4b").unwrap();
    assert_eq!(model.provider_model, MODEL);
    assert_eq!(model.provider, "bfl");
    assert!(matches!(model.pricing, Pricing::ByUsage { .. }));
    assert!(Wiring::new(Modality::Image).with_inline_prompt().fit(model).is_ok());
    assert!(Wiring::new(Modality::Image).with_inline_prompt().with(Modality::Image, 1).fit(model).is_err());
}

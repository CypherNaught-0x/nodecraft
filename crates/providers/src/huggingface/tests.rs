use super::*;
use crate::llm::tests::Canned;
use crate::{HttpRequest, HttpResponse, Output};
use futures::executor::block_on;
use serde_json::json;

const REPLY: &str = include_str!("fixtures/reply.json");
fn job() -> ProviderJob {
    ProviderJob {
        model: MODEL.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A harbour".into() }],
        idempotency_key: "public-fixture".into(),
    }
}
fn provider(http: Arc<dyn Http>) -> HuggingFace {
    HuggingFace::new(&Secret::new("public-fixture-key").unwrap(), http)
}

#[test]
fn huggingface48_catalog_defaults_use_fixed_huggingface_origin_and_secret_auth() {
    let model = nodecraft_catalog::Catalog::builtin().unwrap().get("huggingface.gpt-oss-120b-groq").unwrap().clone();
    assert!(matches!(model.pricing, nodecraft_catalog::Pricing::ByUsage { .. }));
    assert!(model.usage.is_none());
    assert_eq!(model.provider_model, MODEL);
    assert_eq!(model.modes.len(), 1);
    let values = model.effective_params(&Default::default(), true).unwrap();
    assert_eq!(values.get("max_tokens"), Some(&nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(DEFAULT_TOKENS))));
    let http = Canned::new(200, REPLY);
    let p = provider(http.clone());
    let Submission::Finished(done) = block_on(p.submit(&job())).unwrap() else { panic!("sync") };
    assert_eq!(done.outputs, [Output::Text("A quiet harbour.".into())]);
    assert_eq!((done.usage.input_tokens, done.usage.output_tokens, done.usage.cost), (Some(12), Some(7), None));
    let request = http.request();
    assert_eq!(request.url, "https://router.huggingface.co/v1/chat/completions");
    assert_eq!(request.secret_headers, ["authorization"]);
    assert!(request.headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer public-fixture-key"));
    assert!(!format!("{request:?}").contains("public-fixture-key"));
    assert_eq!(request.max_response_bytes, 8 * 1024 * 1024);
    let body = http.body();
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["max_tokens"], 2048);
    assert!(body.get("temperature").is_none());
    assert!(!p.retry_ambiguous_submit());
}

#[test]
fn huggingface48_streaming_delivery_keeps_usage_and_completion() {
    let http = Canned::new(200, include_str!("fixtures/stream.txt"));
    let p = provider(http.clone());
    let context = SubmitContext::streaming_text();
    let Submission::Finished(done) = block_on(p.submit_with_delivery(&job(), &context)).unwrap() else { panic!("sync") };
    assert_eq!(done.outputs, [Output::Text("A quiet harbour.".into())]);
    assert_eq!(done.usage.output_tokens, Some(7));
    assert_eq!(context.take_text().concat(), "A quiet harbour.");
    assert_eq!(http.body()["stream"], true);
    assert_eq!(http.body()["stream_options"]["include_usage"], true);
}

#[test]
fn huggingface48_invalid_inputs_are_refused_before_http() {
    let http = Canned::new(200, REPLY);
    let p = provider(http.clone());
    let mut cases = vec![];
    let mut j = job();
    j.idempotency_key = "k".repeat(257);
    cases.push(j);
    let mut j = job();
    j.model = "../models/private".into();
    cases.push(j);
    let mut j = job();
    j.model = "openai/gpt-oss-120b:fastest".into();
    cases.push(j);
    for cap in [json!(0), json!(8193), json!(-1), json!(1.5), json!("2048")] {
        let mut j = job();
        j.params.insert("max_tokens".into(), cap);
        cases.push(j);
    }
    let mut j = job();
    j.params.insert("temperature".into(), json!(0.7));
    cases.push(j);
    let mut j = job();
    j.inputs = vec![];
    cases.push(j);
    let mut j = job();
    j.inputs = vec![JobInput::Text { role: "prompt".into(), text: "x".repeat(MAX_TEXT_BYTES + 1) }];
    cases.push(j);
    let mut j = job();
    j.inputs = vec![JobInput::Text { role: "private".into(), text: "x".into() }];
    cases.push(j);
    let mut j = job();
    j.inputs = vec![j.inputs[0].clone(); 65];
    cases.push(j);
    let mut j = job();
    j.inputs = vec![JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(&b"PNG"[..]) }];
    cases.push(j);
    for j in cases {
        assert_eq!(block_on(p.submit(&j)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    }
    assert!(http.seen.lock().unwrap().is_empty());
}

struct Failure(ErrorKind);
impl Http for Failure {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        Box::pin(async move { Err(ProviderError::new(self.0, "public transport failure")) })
    }
}
#[test]
fn huggingface48_errors_lifecycle_and_no_replay_are_honest() {
    for (status, kind) in [
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
        let p = provider(Canned::new(status, r#"{"error":{"message":"public fixture"}}"#));
        assert_eq!(block_on(p.submit(&job())).unwrap_err().kind, kind, "{status}");
    }
    for kind in [ErrorKind::Network, ErrorKind::Timeout, ErrorKind::Busy, ErrorKind::Budget] {
        let p = provider(Arc::new(Failure(kind)));
        assert_eq!(block_on(p.submit(&job())).unwrap_err().kind, kind);
        assert!(!p.retry_ambiguous_submit());
    }
    for (body, kind) in
        [("not JSON", ErrorKind::Malformed), (r#"{"choices":[{"message":{"content":"partial"},"finish_reason":"content_filter"}]}"#, ErrorKind::BlockedContent)]
    {
        assert_eq!(block_on(provider(Canned::new(200, body)).submit(&job())).unwrap_err().kind, kind);
    }
    let p = provider(Canned::new(200, REPLY));
    let handle = JobHandle { remote_id: "no-handle".into(), data: serde_json::Value::Null };
    assert_eq!(block_on(p.poll(&handle)).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(block_on(p.cancel(&handle)).unwrap_err().kind, ErrorKind::UnsupportedInput);
}

#[test]
fn huggingface48_rate_and_quota_errors_keep_the_existing_retry_contract() {
    let http = Canned::with_headers(429, r#"{"error":"rate limited"}"#, vec![("Retry-After".into(), "2".into())]);
    let error = block_on(provider(http.clone()).submit(&job())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::RateLimited);
    assert_eq!(error.retry_after_ms, Some(2000));
    assert_eq!(http.seen.lock().unwrap().len(), 1);
    let http = Canned::new(429, r#"{"error":{"code":"insufficient_quota","message":"public fixture quota"}}"#);
    let error = block_on(provider(http).submit(&job())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Budget);
}

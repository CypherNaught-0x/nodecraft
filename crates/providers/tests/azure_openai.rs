//! Azure's v1 preset exercises the shared adapter against public-safe fixtures, never Azure.
#![cfg(feature = "llm")]

use futures::executor::block_on;
use nodecraft_providers::llm::{ApiKey, Endpoint, LlmClient, chat_request};
use nodecraft_providers::{
    BoxFuture, ErrorKind, Http, HttpRequest, HttpResponse, JobHandle, JobInput, Output, Provider, ProviderError, ProviderJob, Submission,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const KEY: &str = "azure-public-fixture-key";
const REPLY: &[u8] = include_bytes!("fixtures/llm/openai_chat_reply.json");
const STREAM: &[u8] = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n";

struct Fixture {
    status: u16,
    body: Vec<u8>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Http for Fixture {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.seen.lock().unwrap().push(request);
        Box::pin(async { Ok(HttpResponse { status: self.status, headers: vec![("Retry-After".into(), "2".into())], body: self.body.clone() }) })
    }
}

fn client(base: &str, status: u16, body: &[u8]) -> (LlmClient, Arc<Fixture>) {
    let http = Arc::new(Fixture { status, body: body.to_vec(), seen: Mutex::new(vec![]) });
    (LlmClient::new(Endpoint::azure_openai(base, ApiKey::new(KEY)).unwrap(), http.clone()), http)
}

fn job() -> ProviderJob {
    ProviderJob {
        model: "team-production-deployment".into(),
        params: [("max_tokens".into(), json!(128))].into_iter().collect(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A harbour".into() }],
        idempotency_key: "public-fixture".into(),
    }
}

#[test]
fn azure_deployment_and_completion_cap_survive_the_shared_provider_path() {
    for root in ["https://demo.openai.azure.com", "https://demo.services.ai.azure.com"] {
        for base in [root.to_owned(), format!("{root}/openai/v1/")] {
            let (client, http) = client(&base, 200, REPLY);
            assert_eq!(client.id(), "azure-openai");
            let Submission::Finished(done) = block_on(client.submit(&job())).unwrap() else { panic!("synchronous reply") };
            assert_eq!(done.outputs, vec![Output::Text("A quiet harbour at dawn.".into())]);
            assert_eq!((done.usage.input_tokens, done.usage.output_tokens), (Some(12), Some(7)));
            let requests = http.seen.lock().unwrap();
            assert_eq!(requests.len(), 1);
            let request = &requests[0];
            assert_eq!(request.url, format!("{root}/openai/v1/chat/completions"));
            assert!(request.headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Bearer {KEY}")));
            assert!(request.secret_headers.iter().any(|n| n == "authorization"));
            assert!(!format!("{request:?} {client:?}").contains(KEY));
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["model"], job().model);
            assert_eq!(body["max_completion_tokens"], 128);
            assert!(body.get("max_tokens").is_none());
            assert!(body.get("temperature").is_none());
            assert!(request.max_response_bytes <= 8 * 1024 * 1024);
        }
    }
}

#[test]
fn azure_stream_and_finished_job_lifecycle_use_shared_semantics() {
    let (client, http) = client("https://demo.openai.azure.com", 200, STREAM);
    let mut pieces = String::new();
    let reply = block_on(client.chat_stream(&chat_request(&job()).unwrap(), &mut |piece| pieces.push_str(piece))).unwrap();
    assert_eq!(pieces, "Hello");
    assert_eq!(reply.text, pieces);
    assert_eq!((reply.usage.input_tokens, reply.usage.output_tokens), (Some(12), Some(1)));
    let request = http.seen.lock().unwrap()[0].clone();
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    let handle = JobHandle { remote_id: "completed".into(), data: Value::Null };
    assert_eq!(block_on(client.poll(&handle)).unwrap_err().kind, ErrorKind::NotFound);
    block_on(client.cancel(&handle)).unwrap();
    assert_eq!(http.seen.lock().unwrap().len(), 1, "there is no remote async job to poll or cancel");
}

#[test]
fn azure_statuses_content_filter_and_malformed_replies_are_actionable() {
    for (status, body, expected) in [
        (401, r#"{"error":{"code":"401","message":"invalid key"}}"#, ErrorKind::Auth),
        (403, r#"{"error":{"message":"access denied"}}"#, ErrorKind::Auth),
        (404, r#"{"error":{"code":"DeploymentNotFound","message":"deployment missing"}}"#, ErrorKind::NotFound),
        (429, r#"{"error":{"code":"429","message":"rate limit"}}"#, ErrorKind::RateLimited),
        (429, r#"{"error":{"code":"insufficient_quota","message":"quota"}}"#, ErrorKind::Budget),
        (400, r#"{"error":{"code":"content_filter","message":"filtered"}}"#, ErrorKind::BlockedContent),
        (400, r#"{"error":{"code":"BadRequest","message":"unsupported input"}}"#, ErrorKind::UnsupportedInput),
        (503, "busy", ErrorKind::Busy),
        (504, "timed out", ErrorKind::Timeout),
        (500, "server error", ErrorKind::Server),
        (200, "<html>", ErrorKind::Malformed),
        (200, r#"{"choices":[{"message":{"content":null},"finish_reason":"content_filter"}]}"#, ErrorKind::BlockedContent),
    ] {
        let (client, http) = client("https://demo.openai.azure.com", status, body.as_bytes());
        let error = block_on(client.submit(&job())).unwrap_err();
        assert_eq!(error.kind, expected, "{status}: {body}");
        if status == 429 {
            assert_eq!(error.retry_after_ms, Some(2000));
        }
        assert_eq!(http.seen.lock().unwrap().len(), 1, "adapter does not retry a potentially paid request");
    }
}

#[test]
fn azure_preset_rejects_unsafe_or_old_endpoints_without_echoing_them() {
    for base in [
        "http://demo.openai.azure.com",
        "https://user:secret@demo.openai.azure.com",
        "https://demo.openai.azure.com?api-key=secret",
        "https://demo.openai.azure.com#secret",
        "https://demo.openai.azure.com/openai/deployments/old",
        "https://demo.openai.azure.com/v1",
        "https://demo.openai.azure.com\n/secret",
        "not an endpoint",
    ] {
        let error = Endpoint::azure_openai(base, ApiKey::new(KEY)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput);
        assert!(!error.message.contains("secret"));
    }
}

#[test]
fn builtin_azure_card_maps_only_its_validated_deployment_parameter() {
    let catalog = nodecraft_catalog::Catalog::builtin().unwrap();
    let model = catalog.get("azure-openai.deployment").unwrap();
    assert!(model.cards);
    assert!(matches!(model.pricing, nodecraft_catalog::Pricing::ByUsage { .. }));
    assert!(model.usage.is_none(), "no invented token prices");
    assert_eq!(model.modes.len(), 1);
    assert_eq!(model.modes[0].output, nodecraft_types::Modality::Text);
    let (azure, http) = client("https://demo.openai.azure.com", 200, REPLY);
    let mut job = job();
    job.model = model.provider_model.clone();
    for deployment in [json!(null), json!(""), json!("bad/name"), json!("line\nbreak"), json!(" x "), json!("x".repeat(65))] {
        job.params.insert("deployment".into(), deployment);
        assert_eq!(block_on(azure.submit(&job)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    }
    assert!(http.seen.lock().unwrap().is_empty(), "invalid deployments refuse before a paid request");
    let values = [("deployment".into(), nodecraft_types::Value::Text("my-chat.1".into()))].into();
    let params = model.effective_params(&values, true).unwrap();
    assert_eq!(params["max_tokens"], nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(1024)));
    job.params.insert("deployment".into(), json!("my-chat.1"));
    job.params.insert("max_tokens".into(), json!(1024));
    block_on(azure.submit(&job)).unwrap();
    let request = &http.seen.lock().unwrap()[0];
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], "my-chat.1");
    assert!(body.get("deployment").is_none());
    assert_eq!(body["max_completion_tokens"], 1024);
}

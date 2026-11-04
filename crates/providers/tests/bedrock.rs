//! Bedrock's OpenAI-compatible preset (#46) exercises the shared adapter against public-safe
//! fixtures, never AWS. Reply bodies are synthetic Chat Completions shapes, not captured traffic.
#![cfg(feature = "llm")]

use futures::executor::block_on;
use nodecraft_providers::llm::{ApiKey, Endpoint, LlmClient, chat_request};
use nodecraft_providers::{
    BoxFuture, ErrorKind, Http, HttpRequest, HttpResponse, JobHandle, JobInput, Output, Provider, ProviderError, ProviderJob, Submission,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const KEY: &str = "bedrock-public-fixture-key";
const REPLY: &[u8] = include_bytes!("fixtures/llm/openai_chat_reply.json");
const STREAM: &[u8] = b"data: {\"choices\":[{\"delta\":{\"content\":\"po\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"ng\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n";

struct Fixture {
    status: u16,
    body: Vec<u8>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Http for Fixture {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.seen.lock().unwrap().push(request);
        Box::pin(async { Ok(HttpResponse { status: self.status, headers: vec![("Retry-After".into(), "3".into())], body: self.body.clone() }) })
    }
}

fn client(region: &str, status: u16, body: &[u8]) -> (LlmClient, Arc<Fixture>) {
    let http = Arc::new(Fixture { status, body: body.to_vec(), seen: Mutex::new(vec![]) });
    (LlmClient::new(Endpoint::bedrock(region, ApiKey::new(KEY)).unwrap(), http.clone()), http)
}

fn job(model: &str) -> ProviderJob {
    ProviderJob {
        model: model.into(),
        params: [("max_tokens".into(), json!(256))].into_iter().collect(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A harbour".into() }],
        idempotency_key: "public-fixture-46".into(),
    }
}

#[test]
fn bedrock_routes_to_the_regional_runtime_with_a_secret_bearer_key() {
    for region in ["us-east-1", "eu-central-1", "ap-southeast-4", "us-gov-west-1", " us-west-2 "] {
        let (client, http) = client(region, 200, REPLY);
        assert_eq!(client.id(), "bedrock");
        assert!(!client.retry_ambiguous_submit(), "Bedrock's chat route documents no idempotency");
        assert!(client.custom_base_url().is_none());
        let Submission::Finished(done) = block_on(client.submit(&job("openai.gpt-oss-120b-1:0"))).unwrap() else { panic!("synchronous reply") };
        assert_eq!(done.outputs, vec![Output::Text("A quiet harbour at dawn.".into())]);
        assert_eq!((done.usage.input_tokens, done.usage.output_tokens), (Some(12), Some(7)));
        assert!(done.usage.cost.is_none(), "Bedrock reports tokens, not a charge");
        let requests = http.seen.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.url, format!("https://bedrock-runtime.{}.amazonaws.com/openai/v1/chat/completions", region.trim()));
        assert!(request.headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Bearer {KEY}")));
        assert!(request.secret_headers.iter().any(|n| n == "authorization"), "the key never follows a redirect");
        assert!(!request.headers.iter().any(|(n, _)| n.to_ascii_lowercase().starts_with("x-amz")), "no SigV4 headers");
        assert!(!format!("{request:?} {client:?}").contains(KEY));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], "openai.gpt-oss-120b-1:0");
        assert_eq!(body["max_completion_tokens"], 256, "Bedrock's OpenAI models document max_completion_tokens");
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("temperature").is_none());
        assert!(request.max_response_bytes <= 8 * 1024 * 1024);
    }
}

#[test]
fn bedrock_stream_and_finished_job_lifecycle_use_shared_semantics() {
    let (client, http) = client("us-east-1", 200, STREAM);
    let mut pieces = String::new();
    let reply = block_on(client.chat_stream(&chat_request(&job("openai.gpt-oss-20b-1:0")).unwrap(), &mut |piece| pieces.push_str(piece))).unwrap();
    assert_eq!(pieces, "pong");
    assert_eq!(reply.text, pieces);
    assert_eq!((reply.usage.input_tokens, reply.usage.output_tokens), (Some(9), Some(2)));
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
fn bedrock_statuses_and_malformed_replies_are_actionable_and_never_retried() {
    for (status, body, expected) in [
        (401, r#"{"message":"Invalid API Key format"}"#, ErrorKind::Auth),
        (403, r#"{"message":"You don't have access to the model with the specified model ID."}"#, ErrorKind::Auth),
        (404, r#"{"error":{"message":"model not found"}}"#, ErrorKind::NotFound),
        (429, r#"{"message":"Too many requests, please wait before trying again."}"#, ErrorKind::RateLimited),
        (400, r#"{"error":{"message":"unsupported input"}}"#, ErrorKind::UnsupportedInput),
        (503, "busy", ErrorKind::Busy),
        (504, "timed out", ErrorKind::Timeout),
        (500, "server error", ErrorKind::Server),
        (200, "<html>", ErrorKind::Malformed),
        (200, r#"{"choices":[{"message":{"content":null},"finish_reason":"content_filter"}]}"#, ErrorKind::BlockedContent),
    ] {
        let (client, http) = client("us-east-1", status, body.as_bytes());
        let error = block_on(client.submit(&job("openai.gpt-oss-120b-1:0"))).unwrap_err();
        assert_eq!(error.kind, expected, "{status}: {body}");
        assert!(!error.message.contains(KEY));
        if status == 429 {
            assert_eq!(error.retry_after_ms, Some(3000));
        }
        assert_eq!(http.seen.lock().unwrap().len(), 1, "adapter does not retry a potentially paid request");
    }
}

#[test]
fn bedrock_preset_rejects_anything_but_a_region_code_without_echoing_it() {
    for region in [
        "",
        "   ",
        "us",
        "us-east",
        "US-EAST-1",
        "us-east-123",
        "us--east-1",
        "-us-east-1",
        "us-east-1-",
        "us_east_1",
        "us-east-1.secret.example",
        "secret.example/us-east-1",
        "https://bedrock-runtime.us-east-1.amazonaws.com",
        "us-east-1\nsecret",
        "us-éast-1",
        "cn-north-1",
        "cn-northwest-1",
        "eusc-de-east-1",
        "us-iso-east-1",
        "us-isob-east-1",
        &"a-".repeat(20),
    ] {
        let error = Endpoint::bedrock(region, ApiKey::new(KEY)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "{region:?}");
        assert!(!error.message.contains("secret"), "{region:?}");
        assert!(!error.message.contains(KEY));
    }
}

#[test]
fn bedrock_preset_refuses_partitions_not_on_amazonaws_com() {
    for region in ["cn-north-1", "eusc-de-east-1", "us-iso-east-1", "us-isob-east-1", "us-iso-west-1"] {
        let error = Endpoint::bedrock(region, ApiKey::new(KEY)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "{region}");
        assert!(error.message.contains("amazonaws.com only"), "{region}: {}", error.message);
    }
    // Ordinary and GovCloud Regions stay on amazonaws.com.
    for region in ["us-east-1", "us-gov-west-1", "eu-south-2", "ap-southeast-4"] {
        assert!(Endpoint::bedrock(region, ApiKey::new(KEY)).is_ok(), "{region}");
    }
}

#[test]
fn builtin_bedrock_cards_send_their_exact_documented_model_ids() {
    let catalog = nodecraft_catalog::Catalog::builtin().unwrap();
    for (id, provider_model) in [("bedrock.gpt-oss-120b", "openai.gpt-oss-120b-1:0"), ("bedrock.gpt-oss-20b", "openai.gpt-oss-20b-1:0")] {
        let model = catalog.get(id).unwrap();
        assert_eq!(model.provider, "bedrock");
        assert_eq!(model.provider_model, provider_model);
        assert!(model.cards);
        assert!(matches!(model.pricing, nodecraft_catalog::Pricing::ByUsage { .. }), "no invented per-Region rate");
        assert!(model.usage.is_none(), "no invented token prices");
        assert_eq!(model.modes.len(), 1);
        assert_eq!(model.modes[0].output, nodecraft_types::Modality::Text);
        let params = model.effective_params(&Default::default(), true).unwrap();
        assert_eq!(params["max_tokens"], nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(2048)));
        let (client, http) = client("us-east-1", 200, REPLY);
        let mut job = job(&model.provider_model);
        job.params.insert("max_tokens".into(), json!(2048));
        block_on(client.submit(&job)).unwrap();
        let body: Value = serde_json::from_slice(&http.seen.lock().unwrap()[0].body).unwrap();
        assert_eq!(body["model"], provider_model);
        assert_eq!(body["max_completion_tokens"], 2048);
    }
}

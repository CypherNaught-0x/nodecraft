//! Contract test for the builtin chat models (#503): every `openai` and `anthropic` catalog entry,
//! with its schema defaults, becomes a request the LLM adapter accepts and sends to the right
//! endpoint, carrying only the parameters the model takes. Replies are recorded fixtures; nothing
//! touches the network.
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use nodecraft_catalog::{Catalog, ModelDescriptor};
use nodecraft_providers::llm::{ApiKey, Endpoint, LlmClient, MAX_IMAGES};
use nodecraft_providers::{BoxFuture, Http, HttpRequest, HttpResponse, JobInput, Output, Provider, ProviderError, ProviderJob, Submission};
use nodecraft_types::{Modality, Number, ParamValues, Unsigned, Value};
use serde_json::{Value as Json, json};

const OPENAI_REPLY: &[u8] = include_bytes!("fixtures/llm/openai_chat_reply.json");
const ANTHROPIC_REPLY: &[u8] = include_bytes!("fixtures/llm/anthropic_message_reply.json");
const KEY: &str = "test-key-not-real";

/// Answers every request with one recorded reply and keeps what was sent.
struct Recorded {
    reply: &'static [u8],
    seen: Mutex<Vec<HttpRequest>>,
}

impl Http for Recorded {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.seen.lock().unwrap().push(request);
        let body = self.reply.to_vec();
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
    }
}

fn chat_models(catalog: &Catalog) -> Vec<&ModelDescriptor> {
    catalog.models().filter(|m| m.provider == "openai" || m.provider == "anthropic").collect()
}

/// The job the engine builds for a generation.text card: the model's effective parameters as
/// JSON, and the card's prompt on the inline `prompt` role.
fn job(model: &ModelDescriptor, node_params: &ParamValues) -> ProviderJob {
    let params = model.effective_params(node_params, true).unwrap();
    let params = params
        .iter()
        .map(|(name, value)| {
            let v = match value {
                Value::Unsigned(n) => json!(n.0),
                Value::Number(n) => json!(n.get()),
                other => panic!("{}: unexpected parameter {name} = {other:?}", model.id),
            };
            (name.clone(), v)
        })
        .collect();
    ProviderJob {
        model: model.provider_model.clone(),
        params,
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "Describe a harbour at dawn in one line.".into() }],
        idempotency_key: "fixture-key".into(),
    }
}

/// Submits `job` through the adapter `model.provider` names; the request sent and the reply.
fn send(model: &ModelDescriptor, job: &ProviderJob) -> (HttpRequest, Submission) {
    let (endpoint, reply) = match model.provider.as_str() {
        "openai" => (Endpoint::openai(ApiKey::new(KEY)), OPENAI_REPLY),
        "anthropic" => (Endpoint::anthropic(ApiKey::new(KEY)), ANTHROPIC_REPLY),
        other => panic!("{other} is not a chat provider"),
    };
    let http = Arc::new(Recorded { reply, seen: Mutex::new(vec![]) });
    let client = LlmClient::new(endpoint, http.clone());
    assert_eq!(client.id(), model.provider, "the catalog provider id routes to this endpoint");
    let done = block_on(client.submit(job)).unwrap_or_else(|e| panic!("{}: {e}", model.id));
    let mut seen = http.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "{}: one request", model.id);
    (seen.remove(0), done)
}

fn header<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
    request.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

#[test]
fn every_builtin_chat_model_builds_a_request_the_adapter_sends() {
    let catalog = Catalog::builtin().unwrap();
    let models = chat_models(&catalog);
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "anthropic.claude-haiku-5-5",
            "anthropic.claude-opus-5-5",
            "anthropic.claude-sonnet-5-5",
            "openai.gpt-4.1-mini",
            "openai.gpt-6-luna",
            "openai.gpt-6.1-sol"
        ]
    );
    for model in models {
        // A text-to-text mode with an inline prompt, so a generation.text card runs it unwired, and
        // an image-to-text mode reading up to the adapter's image cap beside the prompt (#592).
        let [mode, vision] = model.modes.as_slice() else { panic!("{}: two modes", model.id) };
        assert_eq!((mode.id.as_str(), mode.output), ("text-to-text", Modality::Text), "{}", model.id);
        let [prompt] = mode.inputs.as_slice() else { panic!("{}: one input role", model.id) };
        assert_eq!((prompt.role.as_str(), prompt.modality, prompt.min, prompt.max, prompt.inline), ("prompt", Modality::Text, 1, 1, true));
        assert_eq!((vision.id.as_str(), vision.output), ("image-to-text", Modality::Text), "{}", model.id);
        let [prompt, image] = vision.inputs.as_slice() else { panic!("{}: a prompt and an image role", model.id) };
        assert_eq!((image.role.as_str(), image.modality, image.min, image.max), ("image", Modality::Image, 1, MAX_IMAGES as u8));
        assert_eq!((prompt.role.as_str(), prompt.inline), ("prompt", true));

        let job = job(model, &ParamValues::new());
        assert_eq!(job.params.get("max_tokens"), Some(&json!(16_000)), "{}: the quoted default is what is sent", model.id);
        let (request, done) = send(model, &job);
        let body: Json = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], model.provider_model.as_str(), "{}", model.id);
        assert!(body.get("temperature").is_none(), "{}: no temperature unless set", model.id);
        match model.provider.as_str() {
            "openai" => {
                assert_eq!(request.url, "https://api.openai.com/v1/chat/completions");
                assert_eq!(header(&request, "authorization"), Some(format!("Bearer {KEY}").as_str()));
                // OpenAI's own API takes the cap as max_completion_tokens (reasoning included).
                assert_eq!(body["max_completion_tokens"], 16_000, "{}", model.id);
                assert!(body.get("max_tokens").is_none());
                assert_eq!(body["messages"][0]["role"], "user");
            }
            _ => {
                assert_eq!(request.url, "https://api.anthropic.com/v1/messages");
                assert_eq!(header(&request, "x-api-key"), Some(KEY));
                assert_eq!(body["max_tokens"], 16_000, "{}", model.id);
                assert_eq!(body["messages"][0]["content"][0]["text"], "Describe a harbour at dawn in one line.");
            }
        }
        let Submission::Finished(done) = done else { panic!("{}: chat replies finish at submit", model.id) };
        assert_eq!(done.outputs, vec![Output::Text("A quiet harbour at dawn.".into())]);
        assert_eq!((done.usage.input_tokens, done.usage.output_tokens), (Some(12), Some(7)));
    }
}

#[test]
fn temperature_is_offered_and_sent_only_where_the_model_accepts_it() {
    let catalog = Catalog::builtin().unwrap();
    let mut node = ParamValues::new();
    node.insert("max_tokens".into(), Value::Unsigned(Unsigned(256)));
    node.insert("temperature".into(), Value::Number(Number::new(0.7).unwrap()));
    for model in chat_models(&catalog) {
        let offered = model.params.params.iter().any(|p| p.name == "temperature");
        // Current Claude models and OpenAI's reasoning models (at their default effort) reject
        // sampling parameters; only the non-reasoning GPT-4.1 mini takes one.
        assert_eq!(offered, model.id == "openai.gpt-4.1-mini", "{}", model.id);
        let (request, _) = send(model, &job(model, &node));
        let body: Json = serde_json::from_slice(&request.body).unwrap();
        let cap = if model.provider == "openai" { "max_completion_tokens" } else { "max_tokens" };
        assert_eq!(body[cap], 256, "{}", model.id);
        if offered {
            let sent = body["temperature"].as_f64().unwrap();
            assert!((sent - 0.7).abs() < 1e-6, "{}: {sent}", model.id);
        } else {
            // A temperature left on the card from another model is never sent to one that rejects it.
            assert!(body.get("temperature").is_none(), "{}", model.id);
        }
    }
    // Out-of-range values are refused by the schema before any request is built.
    let mini = catalog.get("openai.gpt-4.1-mini").unwrap();
    let mut hot = ParamValues::new();
    hot.insert("temperature".into(), Value::Number(Number::new(2.5).unwrap()));
    assert!(mini.effective_params(&hot, true).is_err());
    let mut long = ParamValues::new();
    long.insert("max_tokens".into(), Value::Unsigned(Unsigned(32_769)));
    assert!(mini.effective_params(&long, true).is_err(), "GPT-4.1 mini writes at most 32,768 tokens");
}

#[test]
fn every_builtin_chat_model_sends_the_prompt_and_its_images_in_one_message() {
    let catalog = Catalog::builtin().unwrap();
    for model in chat_models(&catalog) {
        let mut job = job(model, &ParamValues::new());
        // The engine sends the prompt first, then the wired images, all in one user message.
        job.inputs.push(JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(&b"PNG"[..]) });
        let (request, _) = send(model, &job);
        let body: Json = serde_json::from_slice(&request.body).unwrap();
        let content = body["messages"][0]["content"].as_array().unwrap_or_else(|| panic!("{}: parts", model.id));
        let kinds: Vec<&str> = content.iter().map(|p| p["type"].as_str().unwrap()).collect();
        let expected = if model.provider == "openai" { ["text", "image_url"] } else { ["text", "image"] };
        assert_eq!(kinds, expected, "{}", model.id);
    }
}

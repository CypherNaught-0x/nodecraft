//! Catalog chat endpoints served from a base URL the person set (#960).

use super::tests::Canned;
use super::*;
use crate::base_url::BaseUrl;
use futures::executor::block_on;

const REPLY: &str = r#"{"choices":[{"message":{"content":"Hello!"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#;
const ANTHROPIC_REPLY: &str = r#"{"content":[{"type":"text","text":"Hello!"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}"#;

fn job() -> ProviderJob {
    ProviderJob {
        model: "gpt-4.1-mini".into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "hi".into() }],
        idempotency_key: "k".into(),
    }
}

fn cost_header() -> Vec<(String, String)> {
    vec![("x-litellm-response-cost".into(), "0.0012".into())]
}

#[test]
fn the_default_endpoint_is_unchanged_when_no_base_url_is_set() {
    let http = Canned::with_headers(200, REPLY, cost_header());
    let client = LlmClient::new(Endpoint::openai(ApiKey::new("sk-fixture")), http.clone());
    assert_eq!(client.custom_base_url(), None);
    assert!(client.retry_ambiguous_submit(), "the default endpoint keeps its retry policy");
    let Submission::Finished(done) = block_on(client.submit(&job())).unwrap() else { panic!("queued") };
    assert_eq!(done.usage.cost, None, "only a LiteLLM proxy profile's spend header is read");
    let request = http.request();
    assert_eq!(request.url, "https://api.openai.com/v1/chat/completions");
    assert!(request.headers.contains(&("Authorization".into(), "Bearer sk-fixture".into())));
}

#[test]
fn a_custom_base_url_takes_the_request_and_the_key_and_keeps_the_model() {
    let base = BaseUrl::parse("http://127.0.0.1:4000/v1/").unwrap();
    let http = Canned::with_headers(200, REPLY, cost_header());
    let client = LlmClient::with_base_url(Endpoint::openai(ApiKey::new("sk-fixture")), &base, http.clone());
    assert_eq!(client.id(), "openai", "the catalog provider id (lane and model identity) is kept");
    assert_eq!(client.custom_base_url(), Some("http://127.0.0.1:4000/v1"));
    assert!(!client.retry_ambiguous_submit(), "a custom server is assumed to ignore idempotency");
    let Submission::Finished(done) = block_on(client.submit(&job())).unwrap() else { panic!("queued") };
    assert_eq!(done.outputs, vec![Output::Text("Hello!".into())]);
    assert_eq!((done.usage.input_tokens, done.usage.output_tokens), (Some(3), Some(2)));
    assert_eq!(done.usage.cost, None, "a custom server's spend header never replaces the catalog price");
    let request = http.request();
    assert_eq!(request.url, "http://127.0.0.1:4000/v1/chat/completions");
    assert!(request.headers.contains(&("Authorization".into(), "Bearer sk-fixture".into())));
    assert!(request.secret_headers.contains(&"authorization".to_owned()));
    assert_eq!(http.body()["model"], "gpt-4.1-mini");
    // Debug output never shows the key.
    assert!(!format!("{client:?}").contains("sk-fixture"));
}

#[test]
fn anthropic_can_be_served_from_a_custom_base_url_too() {
    let base = BaseUrl::parse("https://gateway.example/anthropic/v1").unwrap();
    let http = Canned::new(200, ANTHROPIC_REPLY);
    let client = LlmClient::with_base_url(Endpoint::anthropic(ApiKey::new("ak-fixture")), &base, http.clone());
    assert_eq!(client.id(), "anthropic");
    let mut request = job();
    request.model = "claude-x".into();
    let Submission::Finished(done) = block_on(client.submit(&request)).unwrap() else { panic!("queued") };
    assert_eq!(done.outputs, vec![Output::Text("Hello!".into())]);
    let sent = http.request();
    assert_eq!(sent.url, "https://gateway.example/anthropic/v1/messages");
    assert!(sent.headers.contains(&("x-api-key".into(), "ak-fixture".into())));
}

#[test]
fn a_litellm_proxy_is_terminal_on_an_uncertain_submit_and_a_zero_cost_is_no_report() {
    let proxy = LlmClient::new(Endpoint::litellm("http://127.0.0.1:4000", None), Canned::new(200, REPLY));
    assert!(!proxy.retry_ambiguous_submit());
    assert_eq!(proxy.custom_base_url(), None, "the LiteLLM profile is its own provider, not a catalog provider moved elsewhere");
    let zero = LlmClient::new(
        Endpoint::litellm("http://127.0.0.1:4000", None),
        Canned::with_headers(200, REPLY, vec![("x-litellm-response-cost".into(), "0".into())]),
    );
    let Submission::Finished(done) = block_on(zero.submit(&job())).unwrap() else { panic!("queued") };
    assert_eq!(done.usage.cost, None);
}

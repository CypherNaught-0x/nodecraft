//! Opt-in live smoke tests against real APIs. They cost a few tokens and never run in CI:
//! `NODECRAFT_LIVE_OPENAI=1 OPENAI_API_KEY=… cargo test -p nodecraft-providers --test live_llm`
//! (likewise `NODECRAFT_LIVE_ANTHROPIC=1 ANTHROPIC_API_KEY=…`, `NODECRAFT_LIVE_OLLAMA=1`).

#![cfg(all(feature = "http", feature = "llm", not(target_arch = "wasm32")))]

use std::sync::Arc;

use nodecraft_providers::ReqwestHttp;
use nodecraft_providers::llm::{ANTHROPIC_DEFAULT_MODEL, ApiKey, ChatRequest, Endpoint, LlmClient, Message};

fn enabled(provider: &str) -> bool {
    std::env::var(format!("NODECRAFT_LIVE_{provider}")).is_ok_and(|v| v == "1")
}

fn ask(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        system: None,
        messages: vec![Message::user("Reply with the single word: pong")],
        max_tokens: Some(64),
        temperature: None,
        tools: vec![],
    }
}

#[tokio::test(flavor = "current_thread")]
async fn live_openai() {
    if !enabled("OPENAI") {
        return;
    }
    let key = ApiKey::new(std::env::var("OPENAI_API_KEY").unwrap());
    let client = LlmClient::new(Endpoint::openai(key), Arc::new(ReqwestHttp::new().unwrap()));
    let model = std::env::var("NODECRAFT_LIVE_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
    let reply = client.chat_stream(&ask(&model), &mut |_| {}).await.unwrap();
    assert!(reply.text.to_lowercase().contains("pong"), "{reply:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn live_anthropic() {
    if !enabled("ANTHROPIC") {
        return;
    }
    let key = ApiKey::new(std::env::var("ANTHROPIC_API_KEY").unwrap());
    let client = LlmClient::new(Endpoint::anthropic(key), Arc::new(ReqwestHttp::new().unwrap()));
    let reply = client.chat_stream(&ask(ANTHROPIC_DEFAULT_MODEL), &mut |_| {}).await.unwrap();
    assert!(reply.text.to_lowercase().contains("pong"), "{reply:?}");
    assert!(!client.list_models().await.unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn live_ollama() {
    if !enabled("OLLAMA") {
        return;
    }
    let client = LlmClient::new(Endpoint::ollama(None), Arc::new(ReqwestHttp::new().unwrap()));
    let models = client.list_models().await.unwrap();
    let model = models.first().expect("pull a model into Ollama first");
    let reply = client.chat(&ask(model)).await.unwrap();
    assert!(!reply.text.is_empty());
}

#[cfg(feature = "huggingface")]
#[tokio::test(flavor = "current_thread")]
async fn live_huggingface48() {
    use nodecraft_providers::{JobInput, Output, Provider, ProviderJob, Submission};
    if !enabled("HUGGINGFACE") {
        return;
    }
    let key = nodecraft_providers::secrets::Secret::new(&std::env::var("HF_TOKEN").unwrap()).unwrap();
    let provider = nodecraft_providers::huggingface::HuggingFace::new(&key, Arc::new(ReqwestHttp::new().unwrap()));
    let job = ProviderJob {
        model: nodecraft_providers::huggingface::MODEL.into(),
        params: serde_json::Map::from_iter([("max_tokens".into(), serde_json::json!(1024))]),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "Reply with one word: pong".into() }],
        idempotency_key: "nodecraft-live-huggingface48".into(),
    };
    let Submission::Finished(reply) = provider.submit(&job).await.unwrap() else { panic!("sync") };
    assert!(reply.outputs.iter().any(|output| matches!(output,Output::Text(text) if text.to_lowercase().contains("pong"))));
}

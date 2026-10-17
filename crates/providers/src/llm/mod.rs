//! Chat (LLM) endpoints for Text nodes, prompt helpers, auto-select and the agent.
//!
//! Two wire dialects cover the endpoints in #10:
//! - [`Dialect::OpenAi`]: Chat Completions, spoken by OpenAI, Ollama, LM Studio, vLLM, OpenRouter
//!   and LiteLLM (which also reports per-request spend);
//! - [`Dialect::Anthropic`]: the Anthropic Messages API.
//!
//! [`LlmClient`] sends a [`ChatRequest`] (text and images) and returns a [`ChatResponse`], either
//! whole or streamed token by token, and implements [`Provider`] so text jobs go through the same
//! queue as everything else.

mod anthropic;
mod openai;
pub mod sse;

pub use anthropic::DEFAULT_MODEL as ANTHROPIC_DEFAULT_MODEL;

use std::sync::Arc;

use serde_json::Value;

use crate::http::{Http, HttpRequest};
use crate::provider::{BoxFuture, JobHandle, JobInput, JobProgress, Output, Provider, ProviderJob, Submission};
use crate::{ErrorKind, ProviderError};

/// Cap on images per request and on their total size (before base64).
pub const MAX_IMAGES: usize = 16;
pub const MAX_IMAGE_BYTES_TOTAL: usize = 20 * 1024 * 1024;
/// Default reply length when the job doesn't say.
pub const DEFAULT_MAX_TOKENS: u32 = 16_000;
/// Cap on a whole (non-streamed) reply.
const MAX_REPLY_BYTES: usize = 8 * 1024 * 1024;
/// Chat requests can take minutes on large models.
const CHAT_TIMEOUT_MS: u64 = 10 * 60 * 1000;
/// Streams run as long as the model writes; the size cap bounds them instead.
const STREAM_TIMEOUT_MS: u64 = 60 * 60 * 1000;
/// Upper bound on a streamed reply's bytes, whatever `max_tokens` says.
const MAX_STREAM_BYTES: usize = 128 * 1024 * 1024;
/// Pages of a model list followed before stopping.
const MAX_MODEL_PAGES: usize = 20;

/// An API key. Never printed: `Debug` shows `ApiKey(…)`.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl From<&crate::secrets::Secret> for ApiKey {
    fn from(secret: &crate::secrets::Secret) -> Self {
        Self(secret.expose().to_owned())
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(…)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    OpenAi,
    Anthropic,
}

/// Where to send chat requests.
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Provider id (`openai`, `ollama`, `anthropic`, …): the scheduler lane and catalog prefix.
    pub id: String,
    pub dialect: Dialect,
    /// Base URL including the version path, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    pub key: Option<ApiKey>,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A gateway URL can carry a key in its query.
        f.debug_struct("Endpoint")
            .field("id", &self.id)
            .field("dialect", &self.dialect)
            .field("base_url", &crate::http::redact_url(&self.base_url))
            .field("key", &self.key)
            .finish()
    }
}

impl Endpoint {
    pub fn openai(key: ApiKey) -> Self {
        Self { id: "openai".into(), dialect: Dialect::OpenAi, base_url: "https://api.openai.com/v1".into(), key: Some(key) }
    }

    /// A local Ollama server (OpenAI-compatible API, no key).
    pub fn ollama(base: Option<&str>) -> Self {
        let base = base.unwrap_or("http://127.0.0.1:11434").trim_end_matches('/');
        Self { id: "ollama".into(), dialect: Dialect::OpenAi, base_url: format!("{base}/v1"), key: None }
    }

    /// A local LM Studio server.
    pub fn lm_studio(base: Option<&str>) -> Self {
        let base = base.unwrap_or("http://127.0.0.1:1234").trim_end_matches('/');
        Self { id: "lmstudio".into(), dialect: Dialect::OpenAi, base_url: format!("{base}/v1"), key: None }
    }

    pub fn openrouter(key: ApiKey) -> Self {
        Self { id: "openrouter".into(), dialect: Dialect::OpenAi, base_url: "https://openrouter.ai/api/v1".into(), key: Some(key) }
    }

    /// A LiteLLM proxy (virtual key); replies report their cost.
    pub fn litellm(base: &str, key: Option<ApiKey>) -> Self {
        Self { id: "litellm".into(), dialect: Dialect::OpenAi, base_url: format!("{}/v1", base.trim_end_matches('/')), key }
    }

    /// Any other OpenAI-compatible server (vLLM, a gateway).
    pub fn openai_compatible(id: &str, base_url: &str, key: Option<ApiKey>) -> Self {
        Self { id: id.into(), dialect: Dialect::OpenAi, base_url: base_url.trim_end_matches('/').into(), key }
    }

    pub fn anthropic(key: ApiKey) -> Self {
        Self { id: "anthropic".into(), dialect: Dialect::Anthropic, base_url: "https://api.anthropic.com/v1".into(), key: Some(key) }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base_url.trim_end_matches('/'))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Text(String),
    /// PNG, JPEG, GIF or WebP.
    Image {
        mime: String,
        bytes: Arc<[u8]>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, parts: vec![Part::Text(text.into())] }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    /// Reply length cap. `None` uses the endpoint's own default where it has one (OpenAI-style
    /// servers), or [`DEFAULT_MAX_TOKENS`] where the API requires a value (Anthropic).
    pub max_tokens: Option<u32>,
    /// `None` leaves the model's default. Current Claude models reject sampling parameters.
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The model finished.
    EndTurn,
    /// The reply hit `max_tokens` and is cut off.
    MaxTokens,
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub text: String,
    pub stop: StopReason,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Spend reported by the endpoint (LiteLLM's `x-litellm-response-cost`), in USD.
    pub reported_cost_usd: Option<f64>,
}

const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

impl ChatRequest {
    /// Drops empty text parts and checks what both APIs would reject, so the user gets a clear
    /// error before anything is sent (or billed).
    fn validate(&mut self, dialect: Dialect) -> Result<(), ProviderError> {
        let bad = |m: String| Err(ProviderError::new(ErrorKind::UnsupportedInput, m));
        if self.model.trim().is_empty() {
            return bad("no model chosen".into());
        }
        for m in &mut self.messages {
            m.parts.retain(|p| !matches!(p, Part::Text(t) if t.trim().is_empty()));
        }
        if self.messages.is_empty() || self.messages.iter().any(|m| m.parts.is_empty()) {
            return bad("nothing to send: a message has no text or image".into());
        }
        if self.messages.iter().any(|m| m.role == Role::Assistant && m.parts.iter().any(|p| matches!(p, Part::Image { .. }))) {
            return bad("only user messages can carry images".into());
        }
        if dialect == Dialect::Anthropic && self.messages.last().is_some_and(|m| m.role == Role::Assistant) {
            return bad("the conversation must end with a user message".into());
        }
        if let Some(t) = self.temperature {
            let max = if dialect == Dialect::Anthropic { 1.0 } else { 2.0 };
            if !t.is_finite() || !(0.0..=max).contains(&t) {
                return bad(format!("temperature must be between 0 and {max}"));
            }
            if dialect == Dialect::Anthropic && anthropic::rejects_sampling(&self.model) {
                return bad(format!("{} doesn't accept a temperature; leave it unset", self.model));
            }
        }
        let images: Vec<(&String, usize)> = self
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .filter_map(|p| match p {
                Part::Image { mime, bytes } => Some((mime, bytes.len())),
                Part::Text(_) => None,
            })
            .collect();
        if images.len() > MAX_IMAGES {
            return bad(format!("{} images is more than the {MAX_IMAGES} a request may carry", images.len()));
        }
        if let Some((mime, _)) = images.iter().find(|(m, _)| !IMAGE_TYPES.contains(&m.as_str())) {
            return bad(format!("{mime} images can't be sent to chat models; use PNG, JPEG, GIF or WebP"));
        }
        let total: usize = images.iter().map(|(_, n)| *n).fold(0, usize::saturating_add);
        if total > MAX_IMAGE_BYTES_TOTAL {
            return bad(format!("the images add up to {total} bytes, more than {MAX_IMAGE_BYTES_TOTAL}"));
        }
        Ok(())
    }
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let b = [chunk.first().copied().unwrap_or(0), chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(T.get(((n >> shift) & 63) as usize).copied().unwrap_or(b'A')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Talks to one chat endpoint.
pub struct LlmClient {
    endpoint: Endpoint,
    http: Arc<dyn Http>,
}

impl std::fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmClient").field("endpoint", &self.endpoint).finish_non_exhaustive()
    }
}

impl LlmClient {
    pub fn new(endpoint: Endpoint, http: Arc<dyn Http>) -> Self {
        Self { endpoint, http }
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    fn request(&self, req: &ChatRequest, stream: bool) -> Result<HttpRequest, ProviderError> {
        let mut req = req.clone();
        req.validate(self.endpoint.dialect)?;
        let req = &req;
        let (path, body) = match self.endpoint.dialect {
            Dialect::OpenAi => ("chat/completions", openai::body(req, stream, self.endpoint.id == "openai")),
            Dialect::Anthropic => ("messages", anthropic::body(req, stream)),
        };
        // A stream carries ~200–250 bytes of framing per token: size the cap from the reply length,
        // so a long, already-billed reply isn't cut off by a cap meant for whole replies.
        // With no limit sent (OpenAI-style default), the server may write up to the model's maximum.
        let sent_limit = match self.endpoint.dialect {
            Dialect::Anthropic => Some(req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
            Dialect::OpenAi => req.max_tokens,
        };
        let (cap, timeout) = if stream {
            let cap = match sent_limit {
                Some(tokens) => usize::try_from(u64::from(tokens.max(1)).saturating_mul(400)).unwrap_or(usize::MAX).clamp(MAX_REPLY_BYTES, MAX_STREAM_BYTES),
                None => MAX_STREAM_BYTES,
            };
            (cap, STREAM_TIMEOUT_MS)
        } else {
            (MAX_REPLY_BYTES, CHAT_TIMEOUT_MS)
        };
        let http = HttpRequest::post_json(self.endpoint.url(path), &body)?.timeout_ms(timeout).max_response_bytes(cap);
        Ok(self.auth(http))
    }

    fn auth(&self, mut http: HttpRequest) -> HttpRequest {
        match (self.endpoint.dialect, &self.endpoint.key) {
            (Dialect::OpenAi, Some(key)) => http = http.header("Authorization", format!("Bearer {}", key.expose())),
            (Dialect::Anthropic, Some(key)) => http = http.header("x-api-key", key.expose()),
            (_, None) => {}
        }
        if self.endpoint.dialect == Dialect::Anthropic {
            http = http.header("anthropic-version", anthropic::VERSION);
        }
        http
    }

    /// Sends the request and waits for the whole reply.
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let response = self.http.send(self.request(req, false)?).await?;
        let cost = reported_cost(&response.headers);
        let response = self.error_for(response)?;
        let mut reply = match self.endpoint.dialect {
            Dialect::OpenAi => openai::parse(&response.body)?,
            Dialect::Anthropic => anthropic::parse(&response.body)?,
        };
        reply.reported_cost_usd = cost;
        Ok(reply)
    }

    /// Sends the request and streams the reply: `on_text` gets each piece as it arrives.
    pub async fn chat_stream(&self, req: &ChatRequest, on_text: &mut (dyn FnMut(&str) + Send)) -> Result<ChatResponse, ProviderError> {
        let request = self.request(req, true)?;
        let mut parser = sse::SseParser::new();
        let mut state = StreamState::default();
        let dialect = self.endpoint.dialect;
        let mut failure: Option<ProviderError> = None;
        let mut sink = |chunk: &[u8]| -> Result<(), ProviderError> {
            let events = parser.feed(chunk).map_err(|m| ProviderError::new(ErrorKind::Malformed, m))?;
            for e in events {
                let delta = match dialect {
                    Dialect::OpenAi => openai::stream_event(&e, &mut state),
                    Dialect::Anthropic => anthropic::stream_event(&e, &mut state),
                };
                match delta {
                    Ok(Some(text)) => on_text(&text),
                    Ok(None) => {}
                    Err(err) => {
                        failure = Some(err.clone());
                        return Err(err);
                    }
                }
            }
            Ok(())
        };
        let head = self.http.send_streaming(request, &mut sink).await;
        drop(sink);
        // A server may close right after the last event without a blank line.
        if failure.is_none() && head.as_ref().is_ok_and(|h| h.is_success()) {
            for last in parser.finish() {
                let flushed = match dialect {
                    Dialect::OpenAi => openai::stream_event(&last, &mut state),
                    Dialect::Anthropic => anthropic::stream_event(&last, &mut state),
                };
                match flushed {
                    Ok(Some(text)) => on_text(&text),
                    Ok(None) => {}
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
        }
        if let Some(e) = failure {
            return Err(e);
        }
        let head = head?;
        let cost = reported_cost(&head.headers);
        self.error_for(head)?;
        if !state.finished {
            return Err(ProviderError::new(ErrorKind::Network, "the reply stream ended early"));
        }
        Ok(ChatResponse {
            text: state.text,
            stop: state.stop.unwrap_or(StopReason::EndTurn),
            input_tokens: state.input_tokens,
            output_tokens: state.output_tokens,
            reported_cost_usd: cost,
        })
    }

    /// The model ids the endpoint offers (`GET /models`; Anthropic's list is paged).
    pub async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let mut ids = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..MAX_MODEL_PAGES {
            let mut url = self.endpoint.url("models");
            if self.endpoint.dialect == Dialect::Anthropic {
                url.push_str("?limit=1000");
                if let Some(a) = &after {
                    url.push_str(&format!("&after_id={}", a.chars().filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c)).collect::<String>()));
                }
            }
            let response = self.http.send(self.auth(HttpRequest::get(url))).await?;
            let response = self.error_for(response)?;
            let v: Value = response.json()?;
            // OpenAI and Anthropic: {"data":[{"id":…}]}; Ollama and LM Studio serve the same shape on /v1.
            ids.extend(v.get("data").and_then(Value::as_array).into_iter().flatten().filter_map(|m| m.get("id")?.as_str().map(str::to_owned)));
            let more = v.get("has_more").and_then(Value::as_bool).unwrap_or(false);
            after = v.get("last_id").and_then(Value::as_str).map(str::to_owned);
            if !more || after.is_none() {
                break;
            }
        }
        Ok(ids)
    }

    fn error_for(&self, response: crate::http::HttpResponse) -> Result<crate::http::HttpResponse, ProviderError> {
        if response.is_success() {
            return Ok(response);
        }
        let mut e = match self.endpoint.dialect {
            Dialect::OpenAi => openai::error(response.status, &response.body),
            Dialect::Anthropic => anthropic::error(response.status, &response.body),
        };
        e.retry_after_ms = response.header("retry-after").and_then(|v| v.trim().parse::<u64>().ok()).map(|s| s.saturating_mul(1000));
        Err(e)
    }
}

/// What a stream has accumulated.
#[derive(Debug, Default)]
pub(crate) struct StreamState {
    text: String,
    stop: Option<StopReason>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    finished: bool,
}

fn reported_cost(headers: &[(String, String)]) -> Option<f64> {
    headers.iter().find(|(n, _)| n == "x-litellm-response-cost").and_then(|(_, v)| v.trim().parse::<f64>().ok()).filter(|c| c.is_finite() && *c >= 0.0)
}

/// Turns a provider job into a chat request: `system` and `prompt` text inputs, image inputs,
/// and the `max_tokens` / `temperature` params.
pub fn chat_request(job: &ProviderJob) -> Result<ChatRequest, ProviderError> {
    let mut system = None;
    let mut parts = Vec::new();
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "system" => {
                // Several system inputs are joined, not silently replaced.
                system = Some(match system.take() {
                    Some(prev) => format!("{prev}\n\n{text}"),
                    None => text.clone(),
                });
            }
            JobInput::Text { text, .. } => parts.push(Part::Text(text.clone())),
            JobInput::Media { mime, bytes, .. } => parts.push(Part::Image { mime: mime.clone(), bytes: bytes.clone() }),
        }
    }
    let max_tokens = job.params.get("max_tokens").and_then(Value::as_u64).map(|n| u32::try_from(n).unwrap_or(u32::MAX).clamp(1, 128_000));
    // Range checks happen at send time, per dialect, so a bad value is reported, not altered.
    let temperature = job.params.get("temperature").and_then(Value::as_f64).filter(|t| t.is_finite()).map(|t| t as f32);
    Ok(ChatRequest { model: job.model.clone(), system, messages: vec![Message { role: Role::User, parts }], max_tokens, temperature })
}

/// Text generations finish within one request, so `submit` returns the result directly
/// ([`Submission::Finished`]). Chat APIs offer no idempotency keys; a retry may run (and bill) the
/// prompt again, which is cheap for text.
impl Provider for LlmClient {
    fn id(&self) -> &str {
        &self.endpoint.id
    }

    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move {
            let reply = self.chat(&chat_request(job)?).await?;
            Ok(Submission::Finished(vec![Output::Text(reply.text)]))
        })
    }

    fn poll<'a>(&'a self, _handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "chat replies finish at submit; there is nothing to poll")) })
    }

    fn cancel<'a>(&'a self, _handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }

    fn poll_interval_ms(&self, _progress: &JobProgress) -> u64 {
        1
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::http::HttpResponse;
    use std::sync::Mutex;

    /// Replies with canned responses and records requests.
    pub(crate) struct Canned {
        pub replies: Mutex<Vec<HttpResponse>>,
        pub seen: Mutex<Vec<HttpRequest>>,
    }

    impl Canned {
        pub(crate) fn new(status: u16, body: &str) -> Arc<Self> {
            Self::with_headers(status, body, vec![])
        }
        pub(crate) fn with_headers(status: u16, body: &str, headers: Vec<(String, String)>) -> Arc<Self> {
            Arc::new(Self { replies: Mutex::new(vec![HttpResponse { status, headers, body: body.as_bytes().to_vec() }]), seen: Mutex::new(vec![]) })
        }
        pub(crate) fn request(&self) -> HttpRequest {
            self.seen.lock().unwrap()[0].clone()
        }
        pub(crate) fn body(&self) -> Value {
            serde_json::from_slice(&self.request().body).unwrap()
        }
    }

    impl Http for Canned {
        fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
            self.seen.lock().unwrap().push(request);
            let r = self.replies.lock().unwrap().pop().unwrap();
            Box::pin(async move { Ok(r) })
        }
    }

    pub(crate) fn request() -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            system: Some("be brief".into()),
            messages: vec![Message {
                role: Role::User,
                parts: vec![Part::Text("hi".into()), Part::Image { mime: "image/png".into(), bytes: Arc::from(&b"PNG"[..]) }],
            }],
            max_tokens: Some(100),
            temperature: Some(0.5),
        }
    }

    #[test]
    fn anthropic_model_lists_are_paged() {
        let pages = Arc::new(Canned {
            replies: Mutex::new(vec![
                HttpResponse { status: 200, headers: vec![], body: br#"{"data":[{"id":"claude-b"}],"has_more":false,"last_id":"claude-b"}"#.to_vec() },
                HttpResponse { status: 200, headers: vec![], body: br#"{"data":[{"id":"claude-a"}],"has_more":true,"last_id":"claude-a"}"#.to_vec() },
            ]),
            seen: Mutex::new(vec![]),
        });
        let client = LlmClient::new(Endpoint::anthropic(ApiKey::new("k")), pages.clone());
        assert_eq!(futures::executor::block_on(client.list_models()).unwrap(), ["claude-a", "claude-b"]);
        let seen = pages.seen.lock().unwrap();
        assert_eq!(seen[1].url, "https://api.anthropic.com/v1/models?limit=1000&after_id=claude-a");
    }

    #[test]
    fn base64_matches_rfc4648() {
        for (raw, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(raw.as_bytes()), enc);
        }
    }

    #[test]
    fn requests_are_validated() {
        let oa = Dialect::OpenAi;
        let mut r = request();
        assert!(r.validate(oa).is_ok());
        r.messages[0].parts.push(Part::Image { mime: "image/tiff".into(), bytes: Arc::from(&b"x"[..]) });
        assert_eq!(r.validate(oa).unwrap_err().kind, ErrorKind::UnsupportedInput);
        let mut r = request();
        r.messages.clear();
        assert!(r.validate(oa).is_err());
        let mut r = request();
        r.messages[0].parts = (0..=MAX_IMAGES).map(|_| Part::Image { mime: "image/png".into(), bytes: Arc::from(&b"x"[..]) }).collect();
        assert!(r.validate(oa).is_err());
        // Empty text is dropped; a message left empty is refused.
        let mut r = request();
        r.messages[0].parts = vec![Part::Text("  ".into())];
        assert!(r.validate(oa).is_err());
        let mut r = request();
        r.messages[0].parts.push(Part::Text(String::new()));
        r.validate(oa).unwrap();
        assert_eq!(r.messages[0].parts.len(), 2);
        // Images only from the user; Claude needs the last turn to be the user's.
        let mut r = request();
        r.messages[0].role = Role::Assistant;
        assert!(r.validate(oa).is_err());
        let mut r = request();
        r.messages.push(Message { role: Role::Assistant, parts: vec![Part::Text("prefill".into())] });
        assert!(r.validate(oa).is_ok());
        assert!(r.validate(Dialect::Anthropic).is_err());
        // Temperature ranges and models that reject sampling.
        let mut r = request();
        r.temperature = Some(1.5);
        assert!(r.validate(oa).is_ok());
        r.model = "claude-3-5-haiku".into();
        assert!(r.validate(Dialect::Anthropic).is_err(), "Anthropic's range is 0–1");
        r.temperature = Some(0.7);
        assert!(r.validate(Dialect::Anthropic).is_ok());
        r.model = "claude-opus-5-5".into();
        assert!(r.validate(Dialect::Anthropic).unwrap_err().message.contains("doesn't accept a temperature"));
        assert_eq!(format!("{:?}", ApiKey::new("sk-secret")), "ApiKey(…)");
    }

    #[test]
    fn jobs_become_chat_requests() {
        let job = ProviderJob {
            model: "gpt".into(),
            params: serde_json::json!({"max_tokens": 1e12, "temperature": 9.0}).as_object().cloned().unwrap(),
            inputs: vec![
                JobInput::Text { role: "system".into(), text: "sys".into() },
                JobInput::Text { role: "system".into(), text: "more".into() },
                JobInput::Text { role: "prompt".into(), text: "hello".into() },
                JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(&b"P"[..]) },
            ],
            idempotency_key: "k".into(),
        };
        let r = chat_request(&job).unwrap();
        assert_eq!(r.system.as_deref(), Some("sys\n\nmore"), "system inputs are joined");
        assert_eq!(r.messages[0].parts.len(), 2);
        assert_eq!(r.max_tokens, None, "a non-integer max_tokens is left to the endpoint");
        assert_eq!(r.temperature, Some(9.0), "range errors are reported at send time, per dialect");
    }

    #[test]
    fn provider_submit_finishes_at_once() {
        let http = Canned::with_headers(
            200,
            r#"{"choices":[{"message":{"content":"Hello!"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#,
            vec![("x-litellm-response-cost".into(), "0.0012".into())],
        );
        let client = LlmClient::new(Endpoint::litellm("http://proxy:4000", Some(ApiKey::new("vk"))), http.clone());
        let job = ProviderJob {
            model: "gpt-x".into(),
            params: Default::default(),
            inputs: vec![JobInput::Text { role: "prompt".into(), text: "hi".into() }],
            idempotency_key: "k".into(),
        };
        let done = futures::executor::block_on(client.submit(&job)).unwrap();
        assert_eq!(done, Submission::Finished(vec![Output::Text("Hello!".into())]));
        let costed = Canned::with_headers(200, r#"{"choices":[{"message":{"content":"x"}}]}"#, vec![("x-litellm-response-cost".into(), "0.0012".into())]);
        let proxy = LlmClient::new(Endpoint::litellm("http://proxy:4000", None), costed);
        let reply = futures::executor::block_on(proxy.chat(&chat_request(&job).unwrap())).unwrap();
        assert_eq!(reply.reported_cost_usd, Some(0.0012), "LiteLLM spend is reported");
        assert_eq!(http.request().url, "http://proxy:4000/v1/chat/completions");
        assert_eq!(client.id(), "litellm");
    }
}

//! Chat (LLM) endpoints for Text nodes, prompt helpers, auto-select and the agent.
//!
//! Two wire dialects cover the endpoints in #10:
//! - [`Dialect::OpenAi`]: Chat Completions, spoken by OpenAI, Ollama, LM Studio, vLLM, OpenRouter
//!   and LiteLLM (which also reports per-request spend);
//! - [`Dialect::Anthropic`]: the Anthropic Messages API.
//!
//! Both are spoken through `rig-core`'s wires over our own transport (#1122): `rig_chat` sends,
//! `openai` and `anthropic` turn requests, replies and errors to and from the crate's terms.
//!
//! [`LlmClient`] sends a [`ChatRequest`] (a conversation of text, images, tool calls and tool
//! results, plus the tools the model may call) and returns a [`ChatResponse`], either whole or
//! streamed token by token. The reply comes back as a [`Message`] to append to the conversation
//! as it is, so the next turn carries everything the model produced (including Claude's thinking
//! blocks, which must be sent back unchanged). [`LlmClient`] also implements [`Provider`], so text
//! jobs go through the same queue as everything else.

mod anthropic;
mod azure;
mod bedrock;
#[cfg(test)]
mod custom_base_tests;
mod openai;
mod rig_chat;
#[cfg(test)]
mod stream_tests;
#[cfg(test)]
mod turns_tests;
#[cfg(test)]
mod vendors_tests;

pub use anthropic::DEFAULT_MODEL as ANTHROPIC_DEFAULT_MODEL;
pub use azure::validate_azure_deployment;
pub use bedrock::{BEDROCK, validate_bedrock_region};

use std::collections::HashSet;
use std::sync::Arc;

use nodecraft_types::{Currency, Money};
use serde_json::Value;

use crate::http::{Http, HttpRequest};
use crate::provider::{BoxFuture, Completed, JobHandle, JobInput, JobProgress, Output, Provider, ProviderJob, Submission, Usage};
use crate::{ErrorKind, ProviderError};

/// Cap on images per request and on their total size (before base64).
pub const MAX_IMAGES: usize = 16;
pub const MAX_IMAGE_BYTES_TOTAL: usize = 20 * 1024 * 1024;
/// Anthropic refuses an image over 10 MB once base64-encoded: about 7.5 MB of raw bytes (#592).
pub const ANTHROPIC_MAX_IMAGE_BYTES: usize = 7_500_000;
/// Most PDFs one request carries (#591).
pub const MAX_DOCUMENTS: usize = 5;
/// Most PDF bytes one request carries: both APIs take requests up to 32 MB, and base64 adds a third.
pub const MAX_DOCUMENT_BYTES_TOTAL: usize = 22 * 1024 * 1024;
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
/// Most tools a request may offer (OpenAI's limit).
pub const MAX_TOOLS: usize = 128;
/// Most content blocks (or tool calls) a reply may carry.
const MAX_REPLY_BLOCKS: usize = 1024;
/// Longest tool name both APIs accept.
const MAX_TOOL_NAME: usize = 64;
/// Longest tool call id accepted in a conversation.
const MAX_TOOL_ID: usize = 256;

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

    /// DeepSeek's OpenAI-format API, in its own dialect (#1122).
    pub fn deepseek(key: ApiKey) -> Self {
        Self { id: openai::DEEPSEEK_ID.into(), dialect: Dialect::OpenAi, base_url: "https://api.deepseek.com".into(), key: Some(key) }
    }

    /// Mistral's API, in its own dialect (#1122).
    pub fn mistral(key: ApiKey) -> Self {
        Self { id: openai::MISTRAL_ID.into(), dialect: Dialect::OpenAi, base_url: "https://api.mistral.ai/v1".into(), key: Some(key) }
    }

    /// GroqCloud's OpenAI-compatible API, in its own dialect (#1122).
    pub fn groq(key: ApiKey) -> Self {
        Self { id: openai::GROQ_ID.into(), dialect: Dialect::OpenAi, base_url: "https://api.groq.com/openai/v1".into(), key: Some(key) }
    }

    /// xAI's Chat Completions API (Grok), in its own dialect (#1122).
    pub fn xai(key: ApiKey) -> Self {
        Self { id: openai::XAI_ID.into(), dialect: Dialect::OpenAi, base_url: "https://api.x.ai/v1".into(), key: Some(key) }
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
    /// PNG, JPEG, GIF or WebP (user messages only).
    Image {
        mime: String,
        bytes: Arc<[u8]>,
    },
    /// A PDF the model reads as a document, its text and its page pictures (user messages only).
    Document {
        mime: String,
        bytes: Arc<[u8]>,
    },
    /// A call the model made to one of the request's tools (assistant messages only). `input` is
    /// the parsed arguments; when a server sends arguments that aren't JSON, it is the raw text as
    /// a string, which fails any object schema: validate it before running the tool.
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// The answer to a [`Part::ToolUse`] of the previous assistant message (user messages only).
    ToolResult {
        id: String,
        content: String,
        is_error: bool,
    },
    /// A block only the dialect that produced it understands, such as Claude's thinking blocks.
    /// Sent back unchanged in later turns (the model keeps its reasoning only if they are), and
    /// left out when the conversation goes to an endpoint of the other dialect.
    Opaque {
        dialect: Dialect,
        block: Value,
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

    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, parts: vec![Part::Text(text.into())] }
    }

    /// The text parts, joined.
    pub fn text(&self) -> String {
        self.parts.iter().filter_map(|p| if let Part::Text(t) = p { Some(t.as_str()) } else { None }).collect()
    }
}

/// A tool the model may call. The model picks whether to call one (`auto`): current Claude models
/// reject forcing a call, so say in the prompt when a tool should be used, and check the reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    /// ASCII letters, digits, `_` and `-`, at most 64 characters.
    pub name: String,
    pub description: String,
    /// A JSON Schema for the arguments; must be an object schema.
    pub input_schema: Value,
    /// Asks the endpoint to guarantee arguments that match the schema (OpenAI's and Anthropic's
    /// strict mode). OpenAI then requires every property to be listed in `required` and
    /// `additionalProperties: false`. Not sent unless set, since some local servers reject it.
    pub strict: bool,
}

/// A tool call in a reply.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToolCall<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub input: &'a Value,
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
    /// Tools the model may call; empty for plain chat.
    pub tools: Vec<Tool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The model finished.
    EndTurn,
    /// The reply hit `max_tokens` and is cut off.
    MaxTokens,
    /// The model is waiting for the results of its tool calls.
    ToolUse,
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    /// The reply's text parts, joined.
    pub text: String,
    /// The whole reply as an assistant message: append it to the conversation as it is.
    pub message: Message,
    pub stop: StopReason,
    /// Token counts, and the spend the endpoint reports (LiteLLM's `x-litellm-response-cost`).
    pub usage: Usage,
}

impl ChatResponse {
    fn new(parts: Vec<Part>, stop: StopReason, usage: Usage) -> Self {
        let message = Message { role: Role::Assistant, parts };
        Self { text: message.text(), message, stop, usage }
    }

    /// The tool calls the model made, in order.
    pub fn tool_calls(&self) -> impl Iterator<Item = ToolCall<'_>> {
        self.message.parts.iter().filter_map(|p| match p {
            Part::ToolUse { id, name, input } => Some(ToolCall { id, name, input }),
            _ => None,
        })
    }
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
            let foreign = m.parts.iter().any(|p| matches!(p, Part::Opaque { dialect: d, .. } if *d != dialect));
            m.parts.retain(|p| match p {
                Part::Text(t) => !t.trim().is_empty(),
                Part::Opaque { dialect: d, .. } => *d == dialect,
                _ => true,
            });
            if foreign && m.parts.is_empty() {
                return bad("an earlier reply holds only reasoning this endpoint can't read; remove that turn or continue with the model that wrote it".into());
            }
        }
        if self.messages.is_empty() || self.messages.iter().any(|m| m.parts.is_empty()) {
            return bad("nothing to send: a message has no text or image".into());
        }
        for m in &self.messages {
            for p in &m.parts {
                let ok = match p {
                    Part::Image { .. } | Part::Document { .. } | Part::ToolResult { .. } => m.role == Role::User,
                    Part::ToolUse { .. } => m.role == Role::Assistant,
                    Part::Text(_) | Part::Opaque { .. } => true,
                };
                if !ok {
                    let what = match p {
                        Part::Image { .. } => "only user messages can carry images",
                        Part::Document { .. } => "only user messages can carry PDFs",
                        Part::ToolResult { .. } => "only user messages can carry tool results",
                        _ => "only assistant messages can carry tool calls",
                    };
                    return bad(what.into());
                }
            }
        }
        check_tool_pairs(&self.messages)?;
        check_tools(&self.tools)?;
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
                _ => None,
            })
            .collect();
        if images.len() > MAX_IMAGES {
            return bad(format!("{} images is more than the {MAX_IMAGES} a request may carry", images.len()));
        }
        if let Some((mime, _)) = images.iter().find(|(m, _)| !IMAGE_TYPES.contains(&m.as_str())) {
            return bad(format!("{mime} images can't be sent to chat models; use PNG, JPEG, GIF or WebP"));
        }
        if dialect == Dialect::Anthropic
            && let Some((_, n)) = images.iter().find(|(_, n)| *n > ANTHROPIC_MAX_IMAGE_BYTES)
        {
            return bad(format!("an image of {n} bytes is more than the {ANTHROPIC_MAX_IMAGE_BYTES} Anthropic accepts; send it smaller"));
        }
        let total: usize = images.iter().map(|(_, n)| *n).fold(0, usize::saturating_add);
        if total > MAX_IMAGE_BYTES_TOTAL {
            return bad(format!("the images add up to {total} bytes, more than {MAX_IMAGE_BYTES_TOTAL}"));
        }
        let documents: Vec<(&String, usize)> = self
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .filter_map(|p| match p {
                Part::Document { mime, bytes } => Some((mime, bytes.len())),
                _ => None,
            })
            .collect();
        if documents.len() > MAX_DOCUMENTS {
            return bad(format!("{} PDFs is more than the {MAX_DOCUMENTS} a request may carry", documents.len()));
        }
        if let Some((mime, _)) = documents.iter().find(|(m, _)| m.as_str() != "application/pdf") {
            return bad(format!("{mime} documents can't be sent to chat models; only PDFs"));
        }
        let total: usize = documents.iter().map(|(_, n)| *n).fold(0, usize::saturating_add);
        if total > MAX_DOCUMENT_BYTES_TOTAL {
            return bad(format!(
                "the PDFs add up to {total} bytes, more than the {MAX_DOCUMENT_BYTES_TOTAL} a request may carry; use PDF to text, or send fewer pages"
            ));
        }
        Ok(())
    }
}

/// Every tool call is answered in the next message, and every result answers a call in the
/// previous one: both APIs refuse a conversation that breaks this.
fn check_tool_pairs(messages: &[Message]) -> Result<(), ProviderError> {
    let bad = |m: String| Err(ProviderError::new(ErrorKind::UnsupportedInput, m));
    let calls =
        |m: &Message| -> Vec<String> { m.parts.iter().filter_map(|p| if let Part::ToolUse { id, .. } = p { Some(id.clone()) } else { None }).collect() };
    let results =
        |m: &Message| -> Vec<String> { m.parts.iter().filter_map(|p| if let Part::ToolResult { id, .. } = p { Some(id.clone()) } else { None }).collect() };
    let mut open: HashSet<String> = HashSet::new();
    for m in messages {
        for p in &m.parts {
            let (Part::ToolUse { id, .. } | Part::ToolResult { id, .. }) = p else { continue };
            if id.is_empty() || id.len() > MAX_TOOL_ID {
                return bad("a tool call id is empty or too long".into());
            }
        }
        let answered = results(m);
        let unique = |ids: &[String]| ids.iter().collect::<HashSet<_>>().len() == ids.len();
        if !unique(&answered) || !unique(&calls(m)) {
            return bad("a tool call id appears twice in one message".into());
        }
        if let Some(stray) = answered.iter().find(|id| !open.contains(*id)) {
            return bad(format!("the tool result for {:?} doesn't answer a call in the previous message", stray.chars().take(64).collect::<String>()));
        }
        if open.iter().any(|id| !answered.contains(id)) {
            return bad("every tool call needs its result in the next message".into());
        }
        open = calls(m).into_iter().collect();
    }
    if !open.is_empty() {
        return bad("every tool call needs its result in the next message".into());
    }
    Ok(())
}

fn check_tools(tools: &[Tool]) -> Result<(), ProviderError> {
    let bad = |m: String| Err(ProviderError::new(ErrorKind::UnsupportedInput, m));
    if tools.len() > MAX_TOOLS {
        return bad(format!("{} tools is more than the {MAX_TOOLS} a request may offer", tools.len()));
    }
    let mut seen = HashSet::new();
    for t in tools {
        let ok = !t.name.is_empty() && t.name.len() <= MAX_TOOL_NAME && t.name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !ok {
            return bad(format!(
                "{:?} isn't a valid tool name (letters, digits, _ and -, at most {MAX_TOOL_NAME})",
                t.name.chars().take(80).collect::<String>()
            ));
        }
        if !seen.insert(t.name.as_str()) {
            return bad(format!("two tools are named {}", t.name));
        }
        if !t.input_schema.is_object() || t.input_schema.get("type").and_then(Value::as_str) != Some("object") {
            return bad(format!("the {} tool's input schema must be an object schema", t.name));
        }
    }
    Ok(())
}

/// A reported USD spend as money. Fractions of a micro-dollar round up, so a charge is never
/// free, but float noise doesn't: 0.000123 is 123 micro-dollars, not 124.
fn usd(cost: f64) -> Option<Money> {
    // Bounded before the cast: NaN, negative and absurd amounts are ignored.
    if !(cost.is_finite() && (0.0..1e12).contains(&cost)) {
        return None;
    }
    let micros = cost * 1e6;
    let nearest = micros.round();
    let micros = if (micros - nearest).abs() <= 8.0 * f64::EPSILON * nearest.max(1.0) { nearest } else { micros.ceil() };
    Some(Money::new(Currency::USD, micros as u64))
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
    /// The endpoint's base URL was set by the person (#960), not the provider's own API.
    custom: bool,
}

impl std::fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmClient").field("endpoint", &self.endpoint).finish_non_exhaustive()
    }
}

impl LlmClient {
    pub fn new(endpoint: Endpoint, http: Arc<dyn Http>) -> Self {
        Self { endpoint, http, custom: false }
    }

    /// `endpoint` (with its id, dialect and key) served from `base` instead of its own API (#960),
    /// such as a LiteLLM proxy or vLLM in front of OpenAI. The key goes to `base`'s host only.
    /// Such a server is assumed to ignore idempotency, so an uncertain submit is never sent
    /// again, and a spend header it sends is not trusted.
    pub fn with_base_url(mut endpoint: Endpoint, base: &crate::base_url::BaseUrl, http: Arc<dyn Http>) -> Self {
        endpoint.base_url = base.as_str().to_owned();
        Self { endpoint, http, custom: true }
    }

    /// The spend a reply's headers report. Never read from a custom base URL: any server can
    /// send the header, and a catalog model there keeps the catalog's price (#960, #961).
    /// Endpoints at their own address read it as before.
    fn reported_cost(&self, headers: &[(String, String)]) -> Option<Money> {
        if self.custom {
            return None;
        }
        reported_cost(headers)
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// `req` checked for what the endpoint would refuse, before anything is sent (or billed).
    fn validated(&self, req: &ChatRequest) -> Result<ChatRequest, ProviderError> {
        let mut req = req.clone();
        req.validate(self.endpoint.dialect)?;
        Ok(req)
    }

    /// The reply's size cap and timeout. A stream carries ~200–250 bytes of framing per token:
    /// the cap is sized from the reply length, so a long, already-billed reply isn't cut off by a
    /// cap meant for whole replies. With no limit sent (OpenAI-style default), the server may
    /// write up to the model's maximum.
    fn limits(&self, req: &ChatRequest, stream: bool) -> (usize, u64) {
        let sent_limit = match self.endpoint.dialect {
            Dialect::Anthropic => Some(req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
            Dialect::OpenAi => req.max_tokens,
        };
        if stream {
            let cap = match sent_limit {
                Some(tokens) => usize::try_from(u64::from(tokens.max(1)).saturating_mul(400)).unwrap_or(usize::MAX).clamp(MAX_REPLY_BYTES, MAX_STREAM_BYTES),
                None => MAX_STREAM_BYTES,
            };
            (cap, STREAM_TIMEOUT_MS)
        } else {
            (MAX_REPLY_BYTES, CHAT_TIMEOUT_MS)
        }
    }

    fn auth(&self, mut http: HttpRequest) -> HttpRequest {
        match (self.endpoint.dialect, &self.endpoint.key) {
            (Dialect::OpenAi, Some(key)) => http = http.secret_header("Authorization", format!("Bearer {}", key.expose())),
            (Dialect::Anthropic, Some(key)) => http = http.secret_header("x-api-key", key.expose()),
            (_, None) => {}
        }
        if self.endpoint.dialect == Dialect::Anthropic {
            http = http.header("anthropic-version", anthropic::VERSION);
            // Anthropic answers a page's request only when it says it comes from a browser with the
            // person's own key (#118); the key is theirs, kept in their browser.
            #[cfg(target_arch = "wasm32")]
            {
                http = http.header("anthropic-dangerous-direct-browser-access", "true");
            }
        }
        http
    }

    /// Sends the request and waits for the whole reply.
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.sent(req, None, None).await
    }

    /// Sends the request and streams the reply: `on_text` gets each piece as it arrives.
    pub async fn chat_stream(&self, req: &ChatRequest, on_text: &mut (dyn FnMut(&str) + Send)) -> Result<ChatResponse, ProviderError> {
        self.sent(req, Some(on_text), None).await
    }

    /// [`Self::chat_stream`], sent through `context` when there is one, so its delivery is tracked.
    async fn streamed(
        &self,
        req: &ChatRequest,
        on_text: &mut (dyn FnMut(&str) + Send),
        context: Option<&crate::SubmitContext>,
    ) -> Result<ChatResponse, ProviderError> {
        self.sent(req, Some(on_text), context).await
    }

    /// Sends the request in the endpoint's dialect: streamed when `on_text` takes the pieces,
    /// through `context` when there is one.
    async fn sent(
        &self,
        req: &ChatRequest,
        on_text: Option<&mut (dyn FnMut(&str) + Send)>,
        context: Option<&crate::SubmitContext>,
    ) -> Result<ChatResponse, ProviderError> {
        match self.endpoint.dialect {
            Dialect::OpenAi => self.openai_chat(req, on_text, context).await,
            Dialect::Anthropic => self.anthropic_chat(req, on_text, context).await,
        }
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

    /// What a LiteLLM proxy's key has spent, and its budget (`GET /key/info`), for the budgets
    /// panel. Other endpoints don't report spend: they answer with an error.
    pub async fn litellm_key_info(&self) -> Result<KeySpend, ProviderError> {
        if self.endpoint.dialect != Dialect::OpenAi {
            return Err(ProviderError::new(ErrorKind::UnsupportedInput, "only LiteLLM proxies report key spend"));
        }
        let root = self.endpoint.base_url.trim_end_matches('/');
        let root = root.strip_suffix("/v1").unwrap_or(root);
        let response = self.http.send(self.auth(HttpRequest::get(format!("{root}/key/info")).max_response_bytes(1024 * 1024))).await?;
        let response = self.error_for(response)?;
        let v: Value = response.json()?;
        let info = v.get("info").ok_or_else(|| ProviderError::new(ErrorKind::Malformed, "key info without `info`"))?;
        let amount = |k: &str| info.get(k).and_then(Value::as_f64).and_then(usd);
        Ok(KeySpend { spent: amount("spend"), max_budget: amount("max_budget") })
    }

    fn error_for(&self, response: crate::http::HttpResponse) -> Result<crate::http::HttpResponse, ProviderError> {
        if response.is_success() {
            return Ok(response);
        }
        let mut e = match self.endpoint.dialect {
            Dialect::OpenAi => openai::error(response.status, &response.body),
            Dialect::Anthropic => anthropic::error(response.status, &response.body),
        };
        e.retry_after_ms = retry_after(response.header("retry-after"));
        Err(e)
    }
}

/// A `Retry-After` in seconds, as milliseconds; the HTTP-date form is treated as absent.
fn retry_after(value: Option<&str>) -> Option<u64> {
    value.and_then(|v| v.trim().parse::<u64>().ok()).map(|s| s.saturating_mul(1000))
}

/// A LiteLLM key's spend and budget, in USD; `None` where the proxy doesn't say (no budget set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpend {
    pub spent: Option<Money>,
    pub max_budget: Option<Money>,
}

fn reported_cost(headers: &[(String, String)]) -> Option<Money> {
    headers.iter().find(|(n, _)| n == "x-litellm-response-cost").and_then(|(_, v)| v.trim().parse::<f64>().ok()).and_then(usd)
}

/// Turns a provider job into a chat request: the conversation from its inputs (`system` text,
/// earlier `assistant` replies, and the user's text and images, in order; see [`JobInput`]), and
/// the `max_tokens` / `temperature` params.
pub fn chat_request(job: &ProviderJob) -> Result<ChatRequest, ProviderError> {
    let mut system = None;
    let mut messages: Vec<Message> = Vec::new();
    for input in &job.inputs {
        let (role, part) = match input {
            JobInput::Text { role, text } if role == "system" => {
                // Several system inputs are joined, not silently replaced.
                system = Some(match system.take() {
                    Some(prev) => format!("{prev}\n\n{text}"),
                    None => text.clone(),
                });
                continue;
            }
            JobInput::Text { role, text } if role == "assistant" => (Role::Assistant, Part::Text(text.clone())),
            JobInput::Text { text, .. } => (Role::User, Part::Text(text.clone())),
            JobInput::Media { mime, bytes, .. } if mime.starts_with("image/") => (Role::User, Part::Image { mime: mime.clone(), bytes: bytes.clone() }),
            // PDFs go as documents (#591); the catalog lets only models that read them take one.
            JobInput::Media { mime, bytes, .. } if mime == "application/pdf" => (Role::User, Part::Document { mime: mime.clone(), bytes: bytes.clone() }),
            // Chat models here read images and PDFs only; audio, video and meshes aren't sent as
            // if they were one.
            JobInput::Media { role, mime, .. } => {
                return Err(ProviderError::new(
                    ErrorKind::UnsupportedInput,
                    format!("chat models can't take {mime} media (input {role}); only images and PDFs"),
                ));
            }
        };
        // Consecutive inputs of one role make one message.
        match messages.last_mut() {
            Some(last) if last.role == role => last.parts.push(part),
            _ => messages.push(Message { role, parts: vec![part] }),
        }
    }
    let max_tokens = job.params.get("max_tokens").and_then(Value::as_u64).map(|n| u32::try_from(n).unwrap_or(u32::MAX).clamp(1, 128_000));
    // Range checks happen at send time, per dialect, so a bad value is reported, not altered.
    let temperature = job.params.get("temperature").and_then(Value::as_f64).filter(|t| t.is_finite()).map(|t| t as f32);
    Ok(ChatRequest { model: job.model.clone(), system, messages, max_tokens, temperature, tools: Vec::new() })
}

/// Text generations finish within one request, so `submit` returns the result directly
/// ([`Submission::Finished`]). Chat APIs offer no idempotency keys, so a request that may have been
/// sent is never sent again by itself (#1122): its charge stays unresolved at its quote.
impl Provider for LlmClient {
    fn id(&self) -> &str {
        &self.endpoint.id
    }

    fn custom_base_url(&self) -> Option<&str> {
        self.custom.then_some(self.endpoint.base_url.as_str())
    }

    fn retry_ambiguous_submit(&self) -> bool {
        // No chat endpoint offers an idempotency guarantee: a lost reply may already have been
        // billed, so a chat request is never resent (#1122, as Azure v1 and Bedrock always
        // were); its charge stays unresolved at its quote.
        false
    }

    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move {
            let reply = self.chat(&self.provider_request(job)?).await?;
            Ok(Submission::Finished(Completed { outputs: vec![Output::Text(reply.text)], usage: reply.usage }))
        })
    }

    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a crate::SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        // This constructor is pure; all effects of this single-request path are tracked below.
        context.arm();
        Box::pin(async move {
            let reply = if context.wants_text() {
                // The answer streams in, each piece reported to the runner as it arrives (#504).
                self.streamed(&self.provider_request(job)?, &mut |piece| context.push_text(piece), Some(context)).await?
            } else {
                self.sent(&self.provider_request(job)?, None, Some(context)).await?
            };
            Ok(Submission::Finished(Completed { outputs: vec![Output::Text(reply.text)], usage: reply.usage }))
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
            tools: vec![],
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
    fn pdfs_are_bounded_and_sent_as_documents() {
        let pdf = |n: usize| Part::Document { mime: "application/pdf".into(), bytes: Arc::from(vec![b'%'; n]) };
        for dialect in [Dialect::OpenAi, Dialect::Anthropic] {
            let mut r = request();
            r.messages[0].parts.push(pdf(1));
            assert!(r.validate(dialect).is_ok());
            r.messages[0].parts = (0..=MAX_DOCUMENTS).map(|_| pdf(1)).collect();
            assert!(r.validate(dialect).unwrap_err().message.contains("PDFs"), "too many PDFs");
            r.messages[0].parts = vec![pdf(MAX_DOCUMENT_BYTES_TOTAL / 2), pdf(MAX_DOCUMENT_BYTES_TOTAL / 2 + 1)];
            assert!(r.validate(dialect).unwrap_err().message.contains("PDF to text"), "too many bytes");
            r.messages[0].parts = vec![Part::Document { mime: "text/plain".into(), bytes: Arc::from(&b"x"[..]) }];
            assert!(r.validate(dialect).is_err(), "only PDFs");
            r.messages[0].parts = vec![Part::Text("hi".into())];
            r.messages.push(Message { role: Role::Assistant, parts: vec![pdf(1)] });
            r.messages.push(Message::user("and?"));
            assert!(r.validate(dialect).unwrap_err().message.contains("only user messages"));
        }
        // The shapes sent: `anthropic::tests` and `openai::tests`, `pdfs_are_sent_as_documents`.
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
        // Anthropic caps each image (10 MB once base64-encoded); OpenAI sets no per-image size.
        let mut r = request();
        r.messages[0].parts.push(Part::Image { mime: "image/png".into(), bytes: Arc::from(vec![0u8; ANTHROPIC_MAX_IMAGE_BYTES + 1]) });
        assert!(r.validate(oa).is_ok());
        assert_eq!(r.validate(Dialect::Anthropic).unwrap_err().kind, ErrorKind::UnsupportedInput);
        r.messages[0].parts.pop();
        r.messages[0].parts.push(Part::Image { mime: "image/png".into(), bytes: Arc::from(vec![0u8; ANTHROPIC_MAX_IMAGE_BYTES]) });
        assert!(r.validate(Dialect::Anthropic).is_ok());
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
        // A PDF goes as a document (#591).
        let mut pdf = job.clone();
        pdf.inputs.push(JobInput::Media { role: "document".into(), mime: "application/pdf".into(), bytes: Arc::from(&b"%PDF"[..]) });
        let r = chat_request(&pdf).unwrap();
        assert_eq!(r.messages[0].parts.last(), Some(&Part::Document { mime: "application/pdf".into(), bytes: Arc::from(&b"%PDF"[..]) }));
        // Other media is refused, not sent as if it were an image (#502).
        for mime in ["audio/wav", "video/mp4", "model/gltf-binary"] {
            let mut audio = job.clone();
            audio.inputs.push(JobInput::Media { role: "clip".into(), mime: mime.into(), bytes: Arc::from(&b"x"[..]) });
            let refused = chat_request(&audio).unwrap_err();
            assert_eq!(refused.kind, ErrorKind::UnsupportedInput);
            assert!(refused.message.contains(mime), "{}", refused.message);
        }
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
        let Submission::Finished(done) = done else { panic!("queued") };
        assert_eq!(done.outputs, vec![Output::Text("Hello!".into())]);
        assert_eq!((done.usage.input_tokens, done.usage.output_tokens), (Some(3), Some(2)), "usage reaches the job result");
        assert_eq!(done.usage.cost, Some(Money::new(Currency::USD, 1200)));
        let costed = Canned::with_headers(200, r#"{"choices":[{"message":{"content":"x"}}]}"#, vec![("x-litellm-response-cost".into(), "0.0012".into())]);
        let proxy = LlmClient::new(Endpoint::litellm("http://proxy:4000", None), costed);
        let reply = futures::executor::block_on(proxy.chat(&chat_request(&job).unwrap())).unwrap();
        assert_eq!(reply.usage.cost, Some(Money::new(Currency::USD, 1200)), "LiteLLM spend is reported");
        assert_eq!(http.request().url, "http://proxy:4000/v1/chat/completions");
        assert_eq!(client.id(), "litellm");
    }
}

//! The Anthropic Messages dialect (`POST /v1/messages`, `x-api-key`, `anthropic-version`).
//!
//! Spoken through `rig-core`'s Messages wire (#1122, docs/decisions/2026-11-06-llm-layer-rig.md)
//! over NodeCraft's own [`crate::Http`] ([`RigHttp`]): the crate builds the request and reads the
//! reply, whole or streamed. NodeCraft keeps validation, caps, keys, error kinds, money, and the
//! reply's blocks as Claude sent them (thinking goes back unchanged in later turns).
//!
//! Current Claude models reject sampling parameters and always think adaptively, so
//! `temperature` is only sent when the job sets one and `thinking` is never sent. The default
//! catalog model is `claude-opus-5-5`.

use std::collections::HashMap;
use std::sync::Arc;

use rig_core::Model;
use rig_core::completion::{CompletionRequest, CompletionResponse};
use rig_core::message::{
    Api, AssistantContent, AssistantMessage, CallId, Document, DocumentData, DocumentMediaType, DocumentSourceKind, Image, ImageMediaType,
    Message as RigMessage, MimeType as _, Opaque, Origin, Text, ToolCall as RigCall, ToolFunction, ToolName, ToolResult, ToolResultContent, UserContent,
};
use rig_core::providers::anthropic::wire::{AnthropicConfig, Dialect as RigDialect, MaxTokens, Quirks, ToolInputStreaming, compatible};
use serde_json::{Value, json};

use super::{ChatRequest, ChatResponse, Dialect, LlmClient, MAX_REPLY_BLOCKS, Part, Role, StopReason, base64, rig_chat};
use crate::provider::Usage;
use crate::rig_http::{Exchange, RigHttp};
use crate::{ErrorKind, ProviderError};

pub(super) const VERSION: &str = "2023-06-01";

/// The API the crate's Messages wire names its turns by.
const MESSAGES_API: &str = "anthropic.messages";

/// Claude models that reject sampling parameters (`temperature`, `top_p`): the 5-series and later.
pub(super) fn rejects_sampling(model: &str) -> bool {
    let m = model.rsplit('/').next().unwrap_or(model);
    ["claude-opus-5", "claude-opus-4-7", "claude-opus-4-8", "claude-sonnet-5", "claude-haiku-5", "claude-fable", "claude-mythos"]
        .iter()
        .any(|p| m.starts_with(p))
}
/// The default model for Anthropic text nodes.
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

/// What Anthropic is spoken as: the Messages wire under a name of its own, so the crate applies
/// none of a vendor's rules (model-bound thinking, its catalog's limits or prices).
const DIALECT: RigDialect = RigDialect { quirks: quirks(), ..compatible("anthropic-messages", "", "", None) };

const fn quirks() -> Quirks {
    let mut quirks = Quirks::gateway();
    // The Messages API requires max_tokens; a request without one gets ours.
    quirks.max_tokens = MaxTokens::Fixed(super::DEFAULT_MAX_TOKENS as u64);
    // Tools go as written: no strict rewriting, no per-tool streaming flag.
    quirks.strict_tool_schemas = false;
    quirks.tool_input_streaming = ToolInputStreaming::Off;
    // Money is NodeCraft's: replies are never priced from the crate's catalog.
    quirks.catalog_priced = false;
    quirks.cache_ttl_pricing = false;
    quirks
}

fn tool_name(name: &str) -> Result<ToolName, ProviderError> {
    ToolName::new(name).map_err(|_| ProviderError::new(ErrorKind::UnsupportedInput, "a tool call has no name"))
}

/// The request in the crate's terms, and each tool result's content as it goes out.
fn request(req: &ChatRequest) -> Result<(CompletionRequest, HashMap<String, String>), ProviderError> {
    let mut history = Vec::new();
    if let Some(system) = &req.system {
        history.push(RigMessage::System { content: system.clone() });
    }
    let mut names: HashMap<&str, &str> = HashMap::new();
    let mut results = HashMap::new();
    for m in &req.messages {
        match m.role {
            Role::User => {
                // The user's order is kept (captions like "Image 1:" sit next to their image),
                // except that tool results must come first.
                let mut content = Vec::new();
                for p in m.parts.iter().filter(|p| matches!(p, Part::ToolResult { .. })) {
                    if let Part::ToolResult { id, content: text, is_error } = p {
                        results.insert(id.clone(), text.clone());
                        content.push(UserContent::ToolResult(ToolResult {
                            call: CallId::from_wire(id.clone()),
                            name: tool_name(names.get(id.as_str()).copied().unwrap_or("tool"))?,
                            content: vec![ToolResultContent::Text(Text::new(text.clone()))],
                            is_error: *is_error,
                        }));
                    }
                }
                for p in &m.parts {
                    content.push(match p {
                        Part::Text(t) => UserContent::text(t.clone()),
                        Part::Image { mime, bytes } => UserContent::Image(Image {
                            data: DocumentSourceKind::Base64(base64(bytes)),
                            media_type: ImageMediaType::from_mime_type(mime),
                            ..Image::default()
                        }),
                        Part::Document { bytes, .. } => UserContent::Document(Document {
                            data: DocumentData::File(DocumentSourceKind::Base64(base64(bytes))),
                            media_type: Some(DocumentMediaType::PDF),
                            additional_params: None,
                        }),
                        _ => continue,
                    });
                }
                if !content.is_empty() {
                    history.push(RigMessage::User { content });
                }
            }
            Role::Assistant => {
                let mut content = Vec::new();
                for p in &m.parts {
                    content.push(match p {
                        Part::Text(t) => AssistantContent::text(t.clone()),
                        Part::ToolUse { id, name, input } => {
                            names.insert(id, name);
                            // Claude's own calls always carry an object; arguments from another
                            // server that weren't one go as `{}`.
                            let input = if input.is_object() { input.clone() } else { json!({}) };
                            AssistantContent::ToolCall(RigCall::new(CallId::from_wire(id.clone()), ToolFunction::new(tool_name(name)?, input)))
                        }
                        // Thinking and server tool blocks go back exactly as Claude sent them.
                        Part::Opaque { block, .. } => AssistantContent::Opaque(Opaque { item: block.clone(), replay: true }),
                        _ => continue,
                    });
                }
                if !content.is_empty() {
                    // Named as this wire's own turn, so the crate keeps its blocks: it drops a
                    // provider block from any other model's turn, where Claude takes them back
                    // from any model.
                    let mut turn = AssistantMessage::new(content);
                    turn.origin = Some(Origin::new(Api::from_static(MESSAGES_API), DIALECT.name, req.model.as_str()));
                    history.push(RigMessage::Assistant(turn));
                }
            }
        }
    }
    let mut out = CompletionRequest::new("");
    out.chat_history = history.into_iter().map(Arc::new).collect();
    // The Messages API requires max_tokens.
    out.max_tokens = Some(u64::from(req.max_tokens.unwrap_or(super::DEFAULT_MAX_TOKENS)));
    out.temperature = rig_chat::temperature(req.temperature);
    if !req.tools.is_empty() {
        // Sent as they are: no `tool_choice` (the default is `auto`, and current models reject
        // forcing a call), each tool strict or not as it asks.
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut tool = json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema });
                if t.strict {
                    tool["strict"] = json!(true);
                }
                tool
            })
            .collect();
        out.additional_params = Some(json!({ "tools": tools }));
    }
    out.accept_unknown_finish_reasons = true;
    Ok((out, results))
}

/// The request back in the shape Claude has been sent, where the wire differs:
/// - the system prompt is a string, not a list of text blocks;
/// - a tool result's content is its text as it was written, a string (the wire makes a list of
///   text blocks, and writes its own words for an empty failed result).
fn fix(body: &mut Value, results: &HashMap<String, String>) {
    if let Some(system) = body.get_mut("system")
        && let Some(blocks) = system.as_array().filter(|b| !b.is_empty() && b.iter().all(|b| b.get("type").and_then(Value::as_str) == Some("text")))
    {
        let text: Vec<&str> = blocks.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect();
        *system = Value::String(text.join("\n\n"));
    }
    let messages = body.get_mut("messages").and_then(Value::as_array_mut);
    for message in messages.into_iter().flatten() {
        for block in message.get_mut("content").and_then(Value::as_array_mut).into_iter().flatten() {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let id = block.get("tool_use_id").and_then(Value::as_str).map(str::to_owned);
            if let (Some(text), Some(block)) = (id.and_then(|id| results.get(&id)), block.as_object_mut()) {
                block.insert("content".into(), Value::String(text.clone()));
            }
        }
    }
}

fn stop(reason: Option<&str>) -> Result<StopReason, ProviderError> {
    Ok(match reason {
        None | Some("end_turn") | Some("stop_sequence") => StopReason::EndTurn,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("tool_use") => StopReason::ToolUse,
        Some("refusal") => return Err(ProviderError::new(ErrorKind::BlockedContent, "Claude declined this request")),
        Some(other) => StopReason::Other(other.to_owned()),
    })
}

fn malformed(what: &str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, what.to_owned())
}

/// A finished content block as a part: text and tool calls are understood, everything else
/// (thinking, redacted thinking, server tool blocks) is kept as it came. `invalid` is the input
/// text of a tool call whose input wasn't a JSON object, as it streamed.
fn part(block: Value, invalid: Option<&String>) -> Result<Option<Part>, ProviderError> {
    Ok(match block.get("type").and_then(Value::as_str) {
        Some("text") => block.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()).map(|t| Part::Text(t.to_owned())),
        Some("tool_use") => {
            let text = |k: &str| block.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_owned);
            let (Some(id), Some(name)) = (text("id"), text("name")) else { return Err(malformed("a tool call without an id or name")) };
            let input = match invalid {
                Some(raw) => Value::String(raw.clone()),
                None => block.get("input").cloned().filter(Value::is_object).unwrap_or_else(|| json!({})),
            };
            Some(Part::ToolUse { id, name, input })
        }
        _ => Some(Part::Opaque { dialect: Dialect::Anthropic, block }),
    })
}

/// Checks a streamed reply's events before the wire reads them:
/// - a block index past the reply's block limit is refused: the wire sizes the reply by it;
/// - a stream that reaches `message_stop` without a stop reason (a gateway that sends
///   `"stop_reason": null`) ends the turn, as it always did here: the wire ends a stream only at
///   a stop reason, so one is added just before, with the counts the last `message_delta` gave.
fn guard() -> crate::rig_http::events::Guard {
    let mut stopped = false;
    let mut usage: Option<Value> = None;
    Box::new(move |data| {
        let Ok(event) = serde_json::from_str::<Value>(data) else { return Ok(None) };
        // Stricter than the wire, which also reads a whole float or a numeric string as an index:
        // any index that isn't a plain integer under the limit is refused.
        if let Some(index) = event.get("index")
            && !index.as_u64().is_some_and(|i| i < MAX_REPLY_BLOCKS as u64)
        {
            return Err(malformed("the reply has too many parts"));
        }
        match event.get("type").and_then(Value::as_str) {
            Some("message_delta") => {
                stopped |= event.pointer("/delta/stop_reason").and_then(Value::as_str).is_some();
                if let Some(counts) = event.get("usage").filter(|u| u.is_object()) {
                    usage = Some(counts.clone());
                }
                Ok(None)
            }
            Some("message_stop") if !stopped => {
                stopped = true;
                let mut delta = json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" } });
                if let Some(counts) = usage.take() {
                    delta["usage"] = counts;
                }
                Ok(Some(format!("event: message_delta\ndata: {delta}\n\n")))
            }
            _ => Ok(None),
        }
    })
}

/// A whole reply the wire reads only when it says it is a `message`, which a compatible server
/// may leave out: a reply with `content` and no `type` is one.
fn whole_message(reply: &mut Value) {
    if let Some(reply) = reply.as_object_mut()
        && reply.contains_key("content")
        && !reply.contains_key("type")
    {
        reply.insert("type".into(), json!("message"));
    }
}

/// The reply as Claude sent it: the crate's provider document of it, whole or rebuilt from the
/// stream, so every block (thinking with its signature included) is kept as it came.
fn reply(response: &CompletionResponse, streamed: bool) -> Result<ChatResponse, ProviderError> {
    let v = &response.raw;
    // A stream that ended properly with no block in it is an empty reply.
    let none = Vec::new();
    let content = match v.get("content").and_then(Value::as_array) {
        Some(content) => content,
        None if streamed => &none,
        None => return Err(malformed("messages reply without content")),
    };
    // Stop reason first: a refusal must not surface partial text as a result.
    let stop = stop(v.get("stop_reason").and_then(Value::as_str))?;
    if content.len() > MAX_REPLY_BLOCKS {
        return Err(malformed("the reply has too many parts"));
    }
    // Streamed input that wasn't a JSON object stays text, which fails any object schema.
    let invalid: HashMap<String, String> = response
        .choice
        .iter()
        .filter_map(|c| match c {
            AssistantContent::ToolCall(call) => Some((call.id.wire().into_owned(), call.function.invalid_arguments.clone()?)),
            _ => None,
        })
        .collect();
    let mut parts = Vec::new();
    // A gap in a stream's block indexes leaves an empty slot: no block, nothing to send back.
    for b in content.iter().filter(|b| b.is_object()) {
        let id = b.get("id").and_then(Value::as_str).unwrap_or_default();
        parts.extend(part(b.clone(), invalid.get(id))?);
    }
    let usage = v.get("usage");
    let usage = Usage {
        input_tokens: usage.and_then(|u| u.get("input_tokens")).and_then(Value::as_u64),
        output_tokens: usage.and_then(|u| u.get("output_tokens")).and_then(Value::as_u64),
        cost: None,
        billable_units: None,
        execution_ms: None,
    };
    Ok(ChatResponse::new(parts, stop, usage))
}

/// How this dialect reads its errors.
const RULES: rig_chat::Rules = rig_chat::Rules { status: error, event: event_error, unended: "messages reply without content" };

fn event_error(err: &Value) -> ProviderError {
    error_from(err, None)
}

impl LlmClient {
    /// Sends `req` to Claude, streamed when `on_text` takes the pieces, through the runner's
    /// delivery scope when there is one.
    pub(super) async fn anthropic_chat(
        &self,
        req: &ChatRequest,
        on_text: Option<&mut (dyn FnMut(&str) + Send)>,
        context: Option<&crate::SubmitContext>,
    ) -> Result<ChatResponse, ProviderError> {
        let req = self.validated(req)?;
        let streamed = on_text.is_some();
        let (cap, timeout) = self.limits(&req, streamed);
        let (request, results) = request(&req)?;
        // Our own URL and headers: the crate's path is always `/v1/messages` under its root.
        let mut transport = RigHttp::new(Arc::clone(&self.http), timeout, cap)
            .with_delivery(context.map(crate::SubmitContext::link))
            .with_url(self.endpoint.url("messages"))
            .with_header("anthropic-version", VERSION)
            .with_reply_fix(whole_message)
            .with_guard(guard)
            .with_fix(Box::new(move |body| fix(body, &results)));
        // Anthropic answers a page's request only when it says it comes from a browser with the
        // person's own key (#118); the key is theirs, kept in their browser.
        #[cfg(target_arch = "wasm32")]
        {
            transport = transport.with_header("anthropic-dangerous-direct-browser-access", "true");
        }
        if let Some(key) = &self.endpoint.key {
            transport = transport.with_secret("x-api-key", key.expose().to_owned());
        }
        // The key goes with the transport, never through the crate.
        let config = AnthropicConfig::with_key(&DIALECT, "").with_base_url(self.endpoint.base_url.as_str()).with_version(VERSION);
        let mut wire = config.completion(req.model.as_str()).with_default_max_tokens(u64::from(super::DEFAULT_MAX_TOKENS));
        wire.tool_input_streaming = ToolInputStreaming::Off;
        // No idle timeout of the crate's: the stream's own timeout and size cap bound it, as before.
        let model = Model::new(wire, transport.clone()).with_idle_timeout(None);
        let response = rig_chat::send(model, &transport, request, on_text, &RULES).await?;
        let mut reply = reply(&response, streamed)?;
        if let Exchange::Answered { headers, .. } = transport.exchange() {
            reply.usage.cost = self.reported_cost(&headers);
        }
        Ok(reply)
    }
}

fn error_from(err: &Value, status: Option<u16>) -> ProviderError {
    let message = crate::error::excerpt(err.get("message").and_then(Value::as_str).unwrap_or("request failed").as_bytes());
    let kind = match err.get("type").and_then(Value::as_str).unwrap_or_default() {
        "invalid_request_error" | "request_too_large" => ErrorKind::UnsupportedInput,
        "authentication_error" | "permission_error" => ErrorKind::Auth,
        "billing_error" => ErrorKind::Budget,
        "not_found_error" => ErrorKind::NotFound,
        "rate_limit_error" => ErrorKind::RateLimited,
        "overloaded_error" => ErrorKind::Busy,
        "api_error" => ErrorKind::Server,
        _ => match status {
            Some(529) => ErrorKind::Busy,
            Some(s) => ProviderError::from_status(s, b"").kind,
            None => ErrorKind::Server,
        },
    };
    // The Messages API names a prompt past the context window only in its message.
    if kind == ErrorKind::UnsupportedInput && message.starts_with("prompt is too long") {
        return ProviderError::context_limit(&message);
    }
    ProviderError::new(kind, message)
}

pub(super) fn error(status: u16, body: &[u8]) -> ProviderError {
    match serde_json::from_slice::<Value>(body).ok().and_then(|v| v.get("error").cloned()) {
        Some(err) => error_from(&err, Some(status)),
        None if status == 529 => ProviderError::new(ErrorKind::Busy, "the API is overloaded"),
        None => ProviderError::from_status(status, body),
    }
}

#[cfg(test)]
mod tests;

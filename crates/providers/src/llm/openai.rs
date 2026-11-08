//! The OpenAI Chat Completions dialect (OpenAI, Ollama, LM Studio, vLLM, OpenRouter, LiteLLM, and
//! the endpoints that speak it: Azure v1, Bedrock's OpenAI route, Gemini, Hugging Face).
//!
//! Spoken through `rig-core`'s Chat Completions wire (#1122,
//! docs/decisions/2026-11-06-llm-layer-rig.md): the crate builds the request and reads the reply,
//! whole or streamed, and every byte goes through NodeCraft's own [`crate::Http`]
//! ([`RigHttp`]). NodeCraft keeps what the crate doesn't decide: validation, the size caps and
//! timeouts, keys, the error kinds, the spend a reply reports, and money (the crate's prices are
//! never read).
//!
//! The wire sends a few things differently from what these servers have been sent so far; the
//! request is brought back to that shape ([`fix`]), each for a reason given there.

use std::collections::HashMap;
use std::sync::Arc;

use rig_core::Model;
use rig_core::completion::{CompletionRequest, CompletionResponse, FinishReason};
use rig_core::message::{
    AssistantContent, AssistantMessage, CallId, Document, DocumentData, DocumentMediaType, DocumentSourceKind, Image, ImageMediaType, Message as RigMessage,
    MimeType as _, Reasoning, Text, ToolCall as RigCall, ToolFunction, ToolName, ToolResult, ToolResultContent, UserContent,
};
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{Auth, BodyRewrite, Chat, DEEPSEEK, Dialect as RigDialect, GROQ, MISTRAL, Quirks};
use rig_core::providers::xai::DIALECT as XAI;
use serde_json::Value;

use super::{ChatRequest, ChatResponse, LlmClient, MAX_REPLY_BLOCKS, Part, Role, StopReason, base64, bedrock, rig_chat};
use crate::provider::Usage;
use crate::rig_http::{Exchange, RigHttp};
use crate::{ErrorKind, ProviderError};

/// What every OpenAI-compatible endpoint here is spoken as: the shared Chat Completions wire,
/// under a name of its own so the crate applies none of a vendor's rules (OpenAI's id
/// shortening, its catalog's limits). A stream that ends with `[DONE]` but no finish reason is
/// whole, as it always was here.
const DIALECT: RigDialect = RigDialect::gateway("openai-compatible", "", "").with_quirks(quirks());

/// The wire's own dialect for a provider it knows, where that dialect's rules are what the
/// provider needs (#1122): DeepSeek's reasoning sent back with tool calls, Mistral's content
/// chunks. Every other endpoint speaks the shared [`DIALECT`].
fn dialect(id: &str) -> RigDialect {
    match id {
        DEEPSEEK_ID => DEEPSEEK,
        MISTRAL_ID => MISTRAL,
        GROQ_ID => GROQ,
        XAI_ID => XAI,
        _ => DIALECT,
    }
}

/// Endpoint ids of the providers spoken in their own dialect.
pub(super) const DEEPSEEK_ID: &str = "deepseek";
pub(super) const MISTRAL_ID: &str = "mistral";
pub(super) const GROQ_ID: &str = "groq";
pub(super) const XAI_ID: &str = "xai";

/// Whether the endpoint's replies keep their reasoning, to send back in later turns: DeepSeek's
/// thinking models want every earlier turn's `reasoning_content` back when the request carries
/// tools (https://api-docs.deepseek.com/guides/thinking_mode, read 2026-11-08).
fn keeps_reasoning(id: &str) -> bool {
    id == DEEPSEEK_ID
}

/// The reasoning a reply kept, as the part that carries it to the next turn.
fn reasoning_part(text: String) -> Part {
    Part::Opaque { dialect: super::Dialect::OpenAi, block: serde_json::json!({ "reasoning_content": text }) }
}

const fn quirks() -> Quirks {
    let mut quirks = Quirks::openai();
    quirks.rewrite = BodyRewrite::None;
    quirks.done_without_finish_reason = true;
    // Money is NodeCraft's: replies are never priced from the crate's catalog.
    quirks.catalog_priced = false;
    quirks
}

fn malformed(what: &str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, what.to_owned())
}

fn tool_name(name: &str) -> Result<ToolName, ProviderError> {
    ToolName::new(name).map_err(|_| ProviderError::new(ErrorKind::UnsupportedInput, "a tool call has no name"))
}

/// The request in the crate's terms, and the tool calls whose arguments must go back as they came.
/// `reasoning`: earlier turns' kept reasoning goes back as reasoning (see [`keeps_reasoning`]).
fn request(req: &ChatRequest, completion_cap: bool, reasoning: bool) -> Result<(CompletionRequest, HashMap<String, String>), ProviderError> {
    let mut history = Vec::new();
    if let Some(system) = &req.system {
        history.push(RigMessage::System { content: system.clone() });
    }
    // Tool names by call id, which a result names too; and arguments that weren't JSON objects.
    let mut names: HashMap<&str, &str> = HashMap::new();
    let mut raw = HashMap::new();
    for m in &req.messages {
        match m.role {
            Role::User => {
                let mut content = Vec::new();
                // Tool results are `tool` messages, which must come straight after the calls.
                for p in &m.parts {
                    if let Part::ToolResult { id, content: text, is_error } = p {
                        // The API has no error flag; say it in the text.
                        let text = if *is_error { format!("Error: {text}") } else { text.clone() };
                        let name = names.get(id.as_str()).copied().unwrap_or("tool");
                        content.push(UserContent::ToolResult(ToolResult {
                            call: CallId::from_wire(id.clone()),
                            name: tool_name(name)?,
                            content: vec![ToolResultContent::Text(Text::new(text))],
                            is_error: *is_error,
                        }));
                    }
                }
                let rest: Vec<&Part> = m.parts.iter().filter(|p| matches!(p, Part::Text(_) | Part::Image { .. } | Part::Document { .. })).collect();
                if rest.iter().all(|p| matches!(p, Part::Text(_))) {
                    // Text only: one string, as some compatible servers accept nothing else.
                    let text: Vec<&str> = rest.iter().filter_map(|p| if let Part::Text(t) = p { Some(t.as_str()) } else { None }).collect();
                    if !text.is_empty() {
                        content.push(UserContent::text(text.join("\n\n")));
                    }
                } else {
                    for p in rest {
                        content.push(match p {
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
                            Part::Text(t) => UserContent::text(t.clone()),
                            _ => continue,
                        });
                    }
                }
                if !content.is_empty() {
                    history.push(RigMessage::User { content });
                }
            }
            Role::Assistant => {
                let mut content = Vec::new();
                if reasoning {
                    for p in &m.parts {
                        if let Part::Opaque { block, .. } = p
                            && let Some(text) = block.get("reasoning_content").and_then(Value::as_str)
                        {
                            content.push(AssistantContent::Reasoning(Reasoning::new(text)));
                        }
                    }
                }
                let texts: Vec<&str> = m.parts.iter().filter_map(|p| if let Part::Text(t) = p { Some(t.as_str()) } else { None }).collect();
                if !texts.is_empty() {
                    content.push(AssistantContent::text(texts.join("\n\n")));
                }
                for p in &m.parts {
                    if let Part::ToolUse { id, name, input } = p {
                        names.insert(id, name);
                        // The wire sends any input but an object as `{}`.
                        match input {
                            Value::Object(_) => {}
                            Value::String(text) => {
                                raw.insert(id.clone(), text.clone());
                            }
                            other => {
                                raw.insert(id.clone(), other.to_string());
                            }
                        }
                        content
                            .push(AssistantContent::ToolCall(RigCall::new(CallId::from_wire(id.clone()), ToolFunction::new(tool_name(name)?, input.clone()))));
                    }
                }
                if !content.is_empty() {
                    history.push(RigMessage::Assistant(AssistantMessage::new(content)));
                }
            }
        }
    }
    let mut params = serde_json::Map::new();
    if !req.tools.is_empty() {
        // Sent as they are: the wire's strict mode applies to every tool and rewrites schemas;
        // here each tool asks for it or not.
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut function = serde_json::json!({ "name": t.name, "description": t.description, "parameters": t.input_schema });
                if t.strict {
                    function["strict"] = Value::Bool(true);
                }
                serde_json::json!({ "type": "function", "function": function })
            })
            .collect();
        params.insert("tools".into(), Value::Array(tools));
    }
    let mut out = CompletionRequest::new("");
    out.chat_history = history.into_iter().map(Arc::new).collect();
    match req.max_tokens {
        // OpenAI itself, Azure v1 and Bedrock's OpenAI route take `max_completion_tokens`
        // (reasoning models reject `max_tokens`), whatever the model.
        Some(n) if completion_cap => {
            params.insert("max_completion_tokens".into(), Value::from(n));
        }
        other => out.max_tokens = other.map(u64::from),
    }
    out.temperature = rig_chat::temperature(req.temperature);
    out.additional_params = (!params.is_empty()).then_some(Value::Object(params));
    out.accept_unknown_finish_reasons = true;
    // Kept reasoning is sent as reasoning, under the dialect's field, not turned into text.
    out.foreign_reasoning = reasoning;
    Ok((out, raw))
}

/// The request back in the shape these servers have been sent, where the wire differs:
/// - text-only content is a plain string, the system prompt's too: some compatible servers
///   accept nothing else (the wire's rewrite that does this also stops it sending PDFs);
/// - several PDFs in one message get distinct file names (`document-1.pdf`, …);
/// - images name no `detail`, which the wire adds to every one;
/// - an assistant turn of only tool calls says `"content": null`;
/// - tool arguments that weren't a JSON object go back as they came, not as `{}`.
fn fix(body: &mut Value, raw: &HashMap<String, String>) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else { return };
    for message in messages {
        if let Some(content) = message.get_mut("content")
            && let Some(parts) = content.as_array().filter(|p| !p.is_empty() && p.iter().all(|p| p.get("type").and_then(Value::as_str) == Some("text")))
        {
            let text: Vec<&str> = parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect();
            *content = Value::String(text.join("\n\n"));
        }
        if let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) {
            let mut files = 0usize;
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("file") => {
                        if let Some(file) = part.get_mut("file").and_then(Value::as_object_mut).filter(|f| f.contains_key("file_data")) {
                            files = files.saturating_add(1);
                            file.insert("filename".into(), Value::String(format!("document-{files}.pdf")));
                        }
                    }
                    Some("image_url") => {
                        if let Some(image) = part.get_mut("image_url").and_then(Value::as_object_mut) {
                            image.remove("detail");
                        }
                    }
                    _ => {}
                }
            }
        }
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if let Some(message) = message.as_object_mut() {
            message.entry("content").or_insert(Value::Null);
        }
        for call in message.get_mut("tool_calls").and_then(Value::as_array_mut).into_iter().flatten() {
            let id = call.get("id").and_then(Value::as_str).map(str::to_owned);
            if let (Some(text), Some(function)) = (id.and_then(|id| raw.get(&id)), call.get_mut("function").and_then(Value::as_object_mut)) {
                function.insert("arguments".into(), Value::String(text.clone()));
            }
        }
    }
}

/// Checks a streamed reply's events before the wire reads them: a tool call index past the
/// reply's limit is refused, as the wire sizes the reply by it.
fn guard() -> crate::rig_http::events::Guard {
    Box::new(|data| {
        let Ok(event) = serde_json::from_str::<Value>(data) else { return Ok(None) };
        let choices = event.get("choices").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let calls = choices.iter().filter_map(|c| c.pointer("/delta/tool_calls").and_then(Value::as_array)).flatten();
        if calls.filter_map(|c| c.get("index").and_then(Value::as_u64)).any(|i| i >= MAX_REPLY_BLOCKS as u64) {
            return Err(malformed("the reply has too many tool calls"));
        }
        Ok(None)
    })
}

/// The crate's reply as ours: its text, then its tool calls in order.
/// The reply tokens to settle, reasoning included: the largest of what the wire counted, the
/// reply's `completion_tokens` and its `total_tokens` less `prompt_tokens`. OpenAI counts
/// reasoning inside `completion_tokens` (and total is prompt plus completion), so nothing is
/// counted twice; xAI counts it apart (`reasoning_apart`): its total includes it, and without a
/// total its `completion_tokens_details.reasoning_tokens` is added. A count a server leaves out
/// stays unknown; no input makes this overflow.
fn output_tokens(usage: Option<&Value>, counted: Option<u64>, reasoning_apart: bool) -> Option<u64> {
    let count = |pointer: &str| usage.and_then(|u| u.pointer(pointer)).and_then(Value::as_u64);
    let completion = count("/completion_tokens");
    let total = count("/total_tokens");
    let beyond_prompt = total.and_then(|total| total.checked_sub(count("/prompt_tokens").unwrap_or(0)));
    let with_reasoning = match (reasoning_apart, total, completion, count("/completion_tokens_details/reasoning_tokens")) {
        (true, None, Some(completion), Some(reasoning)) => Some(completion.saturating_add(reasoning)),
        _ => None,
    };
    [counted, completion, beyond_prompt, with_reasoning].into_iter().flatten().max()
}

/// `reasoning`: the reply's reasoning is kept (see [`keeps_reasoning`]). `reasoning_apart`: the
/// endpoint counts reasoning tokens apart from `completion_tokens` (see [`output_tokens`]).
fn reply(response: &CompletionResponse, reasoning: bool, reasoning_apart: bool) -> Result<ChatResponse, ProviderError> {
    // The wire leaves out a call that never names its tool; here such a reply is malformed, as
    // the agent can't answer the call. The reply's provider document still has it.
    let calls = response.raw.pointer("/choices/0/message/tool_calls").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let unnamed = |call: &Value| {
        let kind = call.get("type").and_then(Value::as_str);
        let name = call.pointer("/function/name").or_else(|| call.pointer("/custom/name")).and_then(Value::as_str);
        matches!(kind, None | Some("function" | "custom")) && name.is_none_or(str::is_empty)
    };
    if calls.iter().any(unnamed) {
        return Err(malformed("a tool call without a name"));
    }
    let stop = match response.finish_reason() {
        None | Some(FinishReason::Stop) => StopReason::EndTurn,
        Some(FinishReason::Length) => StopReason::MaxTokens,
        Some(FinishReason::ToolCalls) => StopReason::ToolUse,
        Some(FinishReason::ContentFilter) => {
            return Err(ProviderError::new(ErrorKind::BlockedContent, "the reply was withheld by the provider's content filter"));
        }
        Some(FinishReason::Other(other)) => StopReason::Other(other),
        Some(other) => StopReason::Other(format!("{other:?}")),
    };
    let mut parts = Vec::new();
    let thought: String = response.choice.iter().filter_map(|c| if let AssistantContent::Reasoning(r) = c { Some(r.text.as_str()) } else { None }).collect();
    if reasoning && !thought.is_empty() {
        parts.push(reasoning_part(thought));
    }
    let text: String = response.choice.iter().filter_map(|c| if let AssistantContent::Text(t) = c { Some(t.text()) } else { None }).collect();
    if !text.is_empty() {
        parts.push(Part::Text(text));
    }
    let calls: Vec<&RigCall> = response.choice.iter().filter_map(|c| if let AssistantContent::ToolCall(call) = c { Some(call) } else { None }).collect();
    if calls.len() > MAX_REPLY_BLOCKS {
        return Err(malformed("the reply has too many tool calls"));
    }
    for (i, call) in calls.into_iter().enumerate() {
        let name = call.function.name.as_str();
        if name.is_empty() {
            return Err(malformed("a tool call without a name"));
        }
        // An id the server didn't send is numbered, as before.
        let id = match &call.id {
            CallId::Provider(id) => id.as_str().to_owned(),
            CallId::Local(_) => format!("call_{i}"),
        };
        // Arguments that weren't a JSON object stay text, which fails any object schema.
        let input = match &call.function.invalid_arguments {
            Some(text) => Value::String(text.clone()),
            None => call.function.arguments_value(),
        };
        parts.push(Part::ToolUse { id, name: name.to_owned(), input });
    }
    let output_tokens = output_tokens(response.raw.get("usage"), response.usage.output_tokens, reasoning_apart);
    let usage = Usage { input_tokens: response.usage.input_tokens, output_tokens, cost: None, billable_units: None, execution_ms: None };
    Ok(ChatResponse::new(parts, stop, usage))
}

/// How this dialect reads its errors.
const RULES: rig_chat::Rules = rig_chat::Rules { status: error, event: event_error, unended: "chat reply without choices" };

fn event_error(err: &Value) -> ProviderError {
    error_from(None, err)
}

impl LlmClient {
    /// Sends `req` to an OpenAI-compatible endpoint, streamed when `on_text` takes the pieces,
    /// through the runner's delivery scope when there is one.
    pub(super) async fn openai_chat(
        &self,
        req: &ChatRequest,
        on_text: Option<&mut (dyn FnMut(&str) + Send)>,
        context: Option<&crate::SubmitContext>,
    ) -> Result<ChatResponse, ProviderError> {
        let req = self.validated(req)?;
        let (cap, timeout) = self.limits(&req, on_text.is_some());
        let completion_cap = matches!(self.endpoint.id.as_str(), "openai" | "azure-openai" | bedrock::BEDROCK);
        let reasoning = keeps_reasoning(&self.endpoint.id);
        let (request, raw) = request(&req, completion_cap, reasoning)?;
        // Our own URL: some dialects put the version in their paths, ours is in the base URL.
        let mut transport = RigHttp::new(Arc::clone(&self.http), timeout, cap)
            .with_delivery(context.map(crate::SubmitContext::link))
            .with_url(self.endpoint.url("chat/completions"))
            .with_fix(Box::new(move |body| fix(body, &raw)))
            .with_guard(guard);
        if let Some(key) = &self.endpoint.key {
            transport = transport.with_secret("Authorization", format!("Bearer {}", key.expose()));
        }
        // The key goes with the transport, never through the crate.
        let config = OpenAIConfig::with_key(&dialect(&self.endpoint.id), "").with_auth(Auth::OptionalBearer).with_base_url(self.endpoint.base_url.as_str());
        // No idle timeout of the crate's: the stream's own timeout and size cap bound it, as before.
        let model = Model::new(Chat::new(config, req.model.as_str()), transport.clone()).with_idle_timeout(None);
        let response = rig_chat::send(model, &transport, request, on_text, &RULES).await?;
        let mut reply = reply(&response, reasoning, self.endpoint.id == XAI_ID)?;
        if let Exchange::Answered { headers, .. } = transport.exchange() {
            reply.usage.cost = self.reported_cost(&headers);
        }
        Ok(reply)
    }
}

fn error_from(status: Option<u16>, err: &Value) -> ProviderError {
    let raw = err.get("message").and_then(Value::as_str).or_else(|| err.as_str()).unwrap_or("request failed");
    // Bounded and single-line: a hostile server can't flood the UI or logs.
    let message = crate::error::excerpt(raw.as_bytes());
    // Only `code`, not `type`: OpenAI's missing-key 401 has type `invalid_request_error`.
    let code = err.get("code").and_then(Value::as_str).unwrap_or_default();
    let by_code = match code {
        "insufficient_quota" => Some(ErrorKind::Budget),
        "context_length_exceeded" => Some(ErrorKind::UnsupportedInput),
        "content_policy_violation" | "content_filter" => Some(ErrorKind::BlockedContent),
        "model_not_found" => Some(ErrorKind::NotFound),
        "rate_limit_exceeded" => Some(ErrorKind::RateLimited),
        _ => None,
    };
    // The status decides auth, not-found and rate limits; codes refine it (quota on a 429).
    let kind = match (status, by_code) {
        (Some(401 | 403), _) => ErrorKind::Auth,
        (Some(404), _) => ErrorKind::NotFound,
        (Some(429), Some(ErrorKind::Budget)) => ErrorKind::Budget,
        (Some(429), _) => ErrorKind::RateLimited,
        (_, Some(k)) => k,
        (Some(s), None) => ProviderError::from_status(s, b"").kind,
        (None, None) => ErrorKind::Server,
    };
    if code == "context_length_exceeded" && kind == ErrorKind::UnsupportedInput {
        return ProviderError::context_limit(&message);
    }
    ProviderError::new(kind, message)
}

pub(super) fn error(status: u16, body: &[u8]) -> ProviderError {
    match serde_json::from_slice::<Value>(body).ok().and_then(|v| v.get("error").cloned()) {
        Some(err) => error_from(Some(status), &err),
        None => ProviderError::from_status(status, body),
    }
}

#[cfg(test)]
mod tests;

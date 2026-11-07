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

use futures::StreamExt as _;
use rig_core::Model;
use rig_core::completion::{CompletionRequest, CompletionResponse, FinishReason};
use rig_core::message::{
    AssistantContent, AssistantMessage, CallId, Document, DocumentData, DocumentMediaType, DocumentSourceKind, Image, ImageMediaType, Message as RigMessage,
    MimeType as _, Text, ToolCall as RigCall, ToolFunction, ToolName, ToolResult, ToolResultContent, UserContent,
};
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{Auth, BodyRewrite, Chat, Dialect as RigDialect, Quirks};
use rig_core::streaming::{Item, StreamEvent};
use serde_json::Value;

use super::{ChatRequest, ChatResponse, LlmClient, MAX_REPLY_BLOCKS, Part, Role, StopReason, base64, bedrock};
use crate::provider::Usage;
use crate::rig_http::{Exchange, RigHttp};
use crate::{ErrorKind, ProviderError};

/// What every OpenAI-compatible endpoint here is spoken as: the shared Chat Completions wire,
/// under a name of its own so the crate applies none of a vendor's rules (OpenAI's id
/// shortening, its catalog's limits). A stream that ends with `[DONE]` but no finish reason is
/// whole, as it always was here.
const DIALECT: RigDialect = RigDialect::gateway("openai-compatible", "", "").with_quirks(quirks());

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
fn request(req: &ChatRequest, completion_cap: bool) -> Result<(CompletionRequest, HashMap<String, String>), ProviderError> {
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
                let texts: Vec<&str> = m.parts.iter().filter_map(|p| if let Part::Text(t) = p { Some(t.as_str()) } else { None }).collect();
                if !texts.is_empty() {
                    content.push(AssistantContent::text(texts.join("\n\n")));
                }
                for p in &m.parts {
                    if let Part::ToolUse { id, name, input } = p {
                        names.insert(id, name);
                        if let Value::String(text) = input {
                            raw.insert(id.clone(), text.clone());
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
    // Through its decimal text, so 0.7 goes as 0.7, not as the nearest f32.
    out.temperature = req.temperature.and_then(|t| t.to_string().parse::<f64>().ok());
    out.additional_params = (!params.is_empty()).then_some(Value::Object(params));
    out.accept_unknown_finish_reasons = true;
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

/// The crate's reply as ours: its text, then its tool calls in order.
fn reply(response: &CompletionResponse) -> Result<ChatResponse, ProviderError> {
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
    let usage =
        Usage { input_tokens: response.usage.input_tokens, output_tokens: response.usage.output_tokens, cost: None, billable_units: None, execution_ms: None };
    Ok(ChatResponse::new(parts, stop, usage))
}

/// The crate's error in NodeCraft's terms, from what the transport saw first: its own failure as
/// it raised it, an error status by the dialect's rules, then the crate's reading of the reply.
fn failure(error: &rig_core::ProviderError, exchange: Exchange, streamed: bool) -> ProviderError {
    match exchange {
        Exchange::Failed(e) => return e,
        Exchange::Answered { status, headers, body } if !(200..300).contains(&status) => {
            let mut e = self::error(status, &body);
            e.retry_after_ms = super::retry_after(headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("retry-after")).map(|(_, v)| v.as_str()));
            return e;
        }
        _ => {}
    }
    // An error event inside a 200 reply.
    if let Some(body) = error.provider_response_body() {
        return match serde_json::from_str::<Value>(body).ok().and_then(|v| v.get("error").cloned()) {
            Some(err) => error_from(None, &err),
            None => malformed(&format!("stream event: {}", crate::error::excerpt(body.as_bytes()))),
        };
    }
    let message = crate::error::excerpt(error.to_string().as_bytes());
    match error {
        rig_core::ProviderError::Truncated if streamed => ProviderError::new(ErrorKind::Network, "the reply stream ended early"),
        // A whole reply that doesn't end the turn: no choice in it.
        rig_core::ProviderError::Truncated => malformed("chat reply without choices"),
        rig_core::ProviderError::Stalled(_) => ProviderError::new(ErrorKind::Timeout, "the reply stream stalled"),
        // Refused before anything was sent.
        rig_core::ProviderError::Request(_) | rig_core::ProviderError::UnsupportedOption(_) => ProviderError::new(ErrorKind::UnsupportedInput, message),
        _ => malformed(&format!("chat reply: {message}")),
    }
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
        let streamed = on_text.is_some();
        let (cap, timeout) = self.limits(&req, streamed);
        let completion_cap = matches!(self.endpoint.id.as_str(), "openai" | "azure-openai" | bedrock::BEDROCK);
        let (request, raw) = request(&req, completion_cap)?;
        let mut transport = RigHttp::new(Arc::clone(&self.http), timeout, cap)
            .with_delivery(context.map(crate::SubmitContext::link))
            .with_fix(Box::new(move |body| fix(body, &raw)));
        if let Some(key) = &self.endpoint.key {
            transport = transport.with_secret("Authorization", format!("Bearer {}", key.expose()));
        }
        // The key goes with the transport, never through the crate.
        let config = OpenAIConfig::with_key(&DIALECT, "").with_auth(Auth::OptionalBearer).with_base_url(self.endpoint.base_url.as_str());
        // No idle timeout of the crate's: the stream's own timeout and size cap bound it, as before.
        let model = Model::new(Chat::new(config, req.model.as_str()), transport.clone()).with_idle_timeout(None);
        let response = match on_text {
            None => model.call(request).await,
            Some(on_text) => {
                let mut stream = model.stream(request).map_err(|e| failure(&e, transport.exchange(), streamed))?;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(Item::Event(StreamEvent::Text { text, .. })) => on_text(&text),
                        Ok(_) => {}
                        Err(e) => return Err(failure(&e, transport.exchange(), streamed)),
                    }
                }
                stream.finish().await
            }
        };
        let response = response.map_err(|e| failure(&e, transport.exchange(), streamed))?;
        let mut reply = reply(&response)?;
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

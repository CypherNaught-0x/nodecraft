//! Vertex AI's Gemini REST wire (#1151). Google Cloud supplies only a bearer token to
//! `RigHttp`; rig sees an empty key and never receives the service account or token.
use std::collections::HashMap;
use std::sync::Arc;

use rig_core::Model;
use rig_core::completion::{CompletionRequest, CompletionResponse};
use rig_core::message::{Api, Origin};
use rig_core::message::{
    AssistantContent, AssistantMessage, CallId, Document, DocumentData, DocumentMediaType, DocumentSourceKind, Image, ImageMediaType, Message as RigMessage,
    MimeType as _, Opaque, Text, ToolCall, ToolFunction, ToolName, ToolResult, ToolResultContent, UserContent,
};
use rig_core::providers::gemini::{GeminiConfig, completion::GenerateContent};
use serde_json::{Value, json};

use super::{ChatRequest, ChatResponse, Dialect, Endpoint, LlmClient, MAX_REPLY_BLOCKS, Part, Role, StopReason, base64, rig_chat};
use crate::google_cloud::{Clock, ServiceAccount, Tokens};
use crate::rig_http::RigHttp;
use crate::secrets::Secret;
use crate::{ErrorKind, Http, ProviderError, Usage};

const PROVIDER: &str = "vertex-llm";

fn unsupported(message: &str) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, message)
}
fn malformed(message: &str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, message)
}
fn name(value: &str) -> Result<ToolName, ProviderError> {
    ToolName::new(value).map_err(|_| unsupported("Vertex tool names must not be empty"))
}

impl LlmClient {
    /// Gemini chat in the service account's own project. `global` or a lowercase regional
    /// location such as `us-central1`; custom hosts and express-mode keys are not accepted.
    pub fn vertex(region: &str, secret: &Secret, http: Arc<dyn Http>, clock: Clock) -> Result<Self, ProviderError> {
        if region.is_empty()
            || region.len() > 63
            || !region.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !region.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            || !region.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
        {
            return Err(unsupported("Vertex location must be global or a lowercase Google Cloud region"));
        }
        let account = ServiceAccount::from_secret(secret)?;
        let host = if region == "global" { "aiplatform.googleapis.com".into() } else { format!("{region}-aiplatform.googleapis.com") };
        let base_url = format!("https://{host}/v1/projects/{}/locations/{region}/publishers/google/models", account.project());
        let tokens = Arc::new(Tokens::new(account, http.clone(), clock));
        let mut client = Self::new(Endpoint { id: PROVIDER.into(), dialect: Dialect::Vertex, base_url, key: None }, http);
        client.vertex = Some(tokens);
        Ok(client)
    }

    pub(super) async fn vertex_chat(
        &self,
        request: &ChatRequest,
        on_text: Option<&mut (dyn FnMut(&str) + Send)>,
        context: Option<&crate::SubmitContext>,
    ) -> Result<ChatResponse, ProviderError> {
        let req = self.validated(request)?;
        // A model is one publisher resource segment, never a path or query.
        if !req.model.starts_with("gemini-") || req.model.len() > 128 || !req.model.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) {
            return Err(unsupported("Vertex chat needs a Gemini publisher model id, without a path or query"));
        }
        if req.messages.iter().flat_map(|m| &m.parts).any(|p| matches!(p, Part::Image { mime, .. } if mime == "image/gif")) {
            return Err(unsupported("Vertex Gemini chat accepts PNG, JPEG and WebP images; convert GIF first"));
        }
        if req.tools.is_empty() && req.messages.iter().flat_map(|message| &message.parts).any(|part| matches!(part, Part::ToolUse { .. })) {
            return Err(unsupported("Vertex tool-history requests need their function declarations; keep the tools list when continuing a tool turn"));
        }
        let (request, originals) = request_body(&req)?;
        let tokens = self.vertex.as_ref().ok_or_else(|| unsupported("build Vertex chat with LlmClient::vertex and a Google Cloud service account"))?;
        let bearer = tokens.bearer().await?;
        let streamed = on_text.is_some();
        let (cap, timeout) = self.limits(&req, streamed);
        let verb = if streamed { "streamGenerateContent?alt=sse" } else { "generateContent" };
        let transport = RigHttp::new(self.http.clone(), timeout, cap)
            .with_delivery(context.map(crate::SubmitContext::link))
            .with_url(format!("{}/{}:{verb}", self.endpoint.base_url, req.model))
            .with_secret("Authorization", format!("Bearer {bearer}"))
            .with_fix(Box::new(move |body| {
                let turns = body
                    .get_mut("contents")
                    .and_then(Value::as_array_mut)
                    .into_iter()
                    .flatten()
                    .filter(|turn| turn.get("role").and_then(Value::as_str) == Some("model"));
                for (turn, original) in turns.zip(&originals) {
                    if let Some(parts) = original
                        && let Some(turn) = turn.as_object_mut()
                    {
                        turn.insert("parts".into(), Value::Array(parts.clone()));
                    }
                }
            }));
        let wire = GenerateContent::new(GeminiConfig::new(""), &req.model);
        let model = Model::new(wire, transport.clone()).with_idle_timeout(None);
        let response = rig_chat::send(model, &transport, request, on_text, &RULES).await;
        if response.as_ref().is_err_and(|e| e.kind == ErrorKind::Auth) {
            tokens.forget();
        }
        let response = response.map_err(|error| ProviderError {
            kind: error.kind,
            retry_after_ms: error.retry_after_ms,
            message: "Vertex chat failed; check the service account permissions, model availability and network, then retry explicitly".into(),
        })?;
        reply(&response)
    }
}

type OriginalTurns = Vec<Option<Vec<Value>>>;

fn request_body(req: &ChatRequest) -> Result<(CompletionRequest, OriginalTurns), ProviderError> {
    let mut history = Vec::new();
    let mut originals = Vec::new();
    if let Some(system) = &req.system {
        history.push(RigMessage::System { content: system.clone() });
    }
    let mut names = HashMap::new();
    for message in &req.messages {
        // Remember names even when an original signed turn is replayed as native parts.
        for part in &message.parts {
            if let Part::ToolUse { id, name, .. } = part {
                names.insert(id.as_str(), name.as_str());
            }
        }
        match message.role {
            Role::User => {
                let mut content = Vec::new();
                for part in &message.parts {
                    content.push(match part {
                        Part::Text(text) => UserContent::text(text.clone()),
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
                        Part::ToolResult { id, content, is_error } => UserContent::ToolResult(ToolResult {
                            call: CallId::from_wire(id.clone()),
                            name: name(names.get(id.as_str()).copied().unwrap_or("tool"))?,
                            content: vec![ToolResultContent::Text(Text::new(content.clone()))],
                            is_error: *is_error,
                        }),
                        _ => continue,
                    });
                }
                history.push(RigMessage::User { content });
            }
            Role::Assistant => {
                // A reply's original parts retain thought signatures and function ids intact.
                let original = message.parts.iter().find_map(|part| match part {
                    Part::Opaque { dialect: Dialect::Vertex, block } => block.get("parts").and_then(Value::as_array),
                    _ => None,
                });
                if original.is_some_and(|parts| parts.len() > MAX_REPLY_BLOCKS) {
                    return Err(unsupported("Vertex turn has too many parts"));
                }
                // Keep canonical calls for rig's pairing/ID pass. The transport restores the
                // signed native parts after encoding, so no signature or provider field is lost.
                let mut content = Vec::new();
                for part in &message.parts {
                    content.push(match part {
                        Part::Text(text) => AssistantContent::text(text.clone()),
                        Part::ToolUse { id, name: tool, input } => {
                            AssistantContent::ToolCall(ToolCall::new(CallId::from_wire(id.clone()), ToolFunction::new(name(tool)?, input.clone())))
                        }
                        _ => continue,
                    });
                }
                if content.is_empty()
                    && let Some(parts) = original
                {
                    content.extend(parts.iter().map(|part| AssistantContent::Opaque(Opaque { item: part.clone(), replay: true })));
                }
                if !content.is_empty() {
                    originals.push(original.cloned());
                    let mut turn = AssistantMessage::new(content);
                    // Only opaque-only turns need native replay identity. Canonical calls
                    // retain rig's normal pairing path, with native parts restored below.
                    if turn.content.iter().all(|part| matches!(part, AssistantContent::Opaque(_))) {
                        turn.origin = Some(Origin::new(Api::from_static("gemini.generate_content"), "gcp.gemini", req.model.as_str()));
                    }
                    history.push(RigMessage::Assistant(turn));
                }
            }
        }
    }
    let mut out = CompletionRequest::new("");
    out.chat_history = history.into_iter().map(Arc::new).collect();
    out.max_tokens = req.max_tokens.map(u64::from);
    out.temperature = rig_chat::temperature(req.temperature);
    if !req.tools.is_empty() {
        // Gemini's native declarations take JSON Schema, without OpenAI's strict flag.
        let declarations: Vec<_> =
            req.tools.iter().map(|tool| json!({"name": tool.name, "description": tool.description, "parametersJsonSchema": tool.input_schema})).collect();
        out.additional_params = Some(json!({"tools": [{"functionDeclarations": declarations}]}));
    }
    Ok((out, originals))
}

fn reply(response: &CompletionResponse) -> Result<ChatResponse, ProviderError> {
    let raw = &response.raw;
    let candidate = raw.get("candidates").and_then(Value::as_array).and_then(|c| c.first()).ok_or_else(|| malformed("Vertex reply has no candidate"))?;
    let stop = match candidate.get("finishReason").and_then(Value::as_str) {
        Some("STOP") => StopReason::EndTurn,
        Some("MAX_TOKENS") => StopReason::MaxTokens,
        Some("SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "IMAGE_SAFETY") => {
            return Err(ProviderError::new(ErrorKind::BlockedContent, "Vertex declined this request"));
        }
        Some(_) => return Err(malformed("Vertex reply ended with an unsupported finish reason")),
        None => return Err(malformed("Vertex reply ended without a finish reason")),
    };
    let native = candidate.pointer("/content/parts").and_then(Value::as_array).ok_or_else(|| malformed("Vertex reply has no content parts"))?;
    if native.len() > MAX_REPLY_BLOCKS {
        return Err(malformed("Vertex reply has too many parts"));
    }
    let mut parts = Vec::new();
    for (index, part) in native.iter().enumerate() {
        if part.get("thought").and_then(Value::as_bool) != Some(true)
            && let Some(text) = part.get("text").and_then(Value::as_str)
        {
            parts.push(Part::Text(text.into()));
        }
        if let Some(call) = part.get("functionCall") {
            let name = call
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty() && n.len() <= super::MAX_TOOL_NAME)
                .ok_or_else(|| malformed("Vertex function call has no usable name"))?;
            let input = call.get("args").cloned().unwrap_or_else(|| json!({}));
            if !input.is_object() {
                return Err(malformed("Vertex function arguments must be an object"));
            }
            let id = call.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("vertex-call-{index}"));
            if id.len() > super::MAX_TOOL_ID {
                return Err(malformed("Vertex function call id is too long"));
            }
            parts.push(Part::ToolUse { id, name: name.into(), input });
        }
    }
    let called = parts.iter().any(|p| matches!(p, Part::ToolUse { .. }));
    parts.push(Part::Opaque { dialect: Dialect::Vertex, block: json!({"parts": native}) });
    let usage = raw.get("usageMetadata");
    let count = |name| usage.and_then(|u| u.get(name)).and_then(Value::as_u64);
    let output = match (count("candidatesTokenCount"), count("thoughtsTokenCount")) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
    };
    Ok(ChatResponse::new(
        parts,
        if called { StopReason::ToolUse } else { stop },
        Usage { input_tokens: count("promptTokenCount"), output_tokens: output, ..Usage::default() },
    ))
}

pub(super) fn status_error(status: u16, _headers: &[(String, String)], _body: &[u8]) -> ProviderError {
    let (kind, message) = match status {
        401 | 403 => (ErrorKind::Auth, "Vertex refused the service account; check its project, API enablement and Vertex AI User role"),
        429 => (ErrorKind::RateLimited, "Vertex quota is exhausted; try again later"),
        500..=599 => (ErrorKind::Server, "Vertex is temporarily unavailable"),
        _ => (ErrorKind::UnsupportedInput, "Vertex refused the chat request"),
    };
    // Server replies can echo tokens or account details; never include their text.
    ProviderError::new(kind, format!("{message} (HTTP {status})"))
}
fn event_error(error: &Value) -> ProviderError {
    let status =
        error.get("code").and_then(Value::as_u64).and_then(|code| u16::try_from(code).ok()).or_else(|| match error.get("status").and_then(Value::as_str) {
            Some("RESOURCE_EXHAUSTED") => Some(429),
            Some("UNAUTHENTICATED" | "PERMISSION_DENIED") => Some(403),
            Some("INTERNAL" | "UNAVAILABLE") => Some(503),
            Some("INVALID_ARGUMENT") => Some(400),
            _ => None,
        });
    status.map_or_else(|| malformed("Vertex returned an in-band error"), |code| status_error(code, &[], &[]))
}

const RULES: rig_chat::Rules = rig_chat::Rules { status: status_error, event: event_error, unended: "Vertex reply ended early" };

#[cfg(test)]
mod tests;

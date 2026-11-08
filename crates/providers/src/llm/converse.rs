//! Amazon Bedrock's native Converse API (`POST /model/{modelId}/converse` and
//! `/converse-stream` on `bedrock-runtime`), spoken over NodeCraft's own [`crate::Http`] (#1150).
//!
//! `rig-core` has no Bedrock provider and the AWS SDK pulls in tokio, hyper and a TLS stack that
//! don't build for wasm, so this dialect is NodeCraft's: the request and reply JSON here, the
//! streamed reply's AWS event-stream framing in [`event_stream`]. It keeps the other dialects'
//! rules: the request is checked before anything is sent, a reply is size-capped, keys are
//! secret headers, a submit goes through the runner's delivery scope, and any error after the
//! request may have been sent leaves its charge unresolved (no automatic retry). Money comes
//! only from the catalog: Converse reports token counts, never a price.
//!
//! Auth is a Bedrock API key as `Authorization: Bearer`, which AWS documents for Converse
//! (<https://docs.aws.amazon.com/bedrock/latest/userguide/api-keys-use.html>, checked
//! 2026-11-08). SigV4 is not spoken here.
//!
//! Wire shapes, checked 2026-11-08 against the API reference:
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Converse.html>,
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStream.html>,
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStreamOutput.html>,
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Message.html>.

pub(crate) mod event_stream;

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::{ChatRequest, ChatResponse, Dialect, LlmClient, MAX_REPLY_BLOCKS, Part, Role, StopReason, base64, retry_after, rig_chat};
use crate::http::HttpRequest;
use crate::provider::Usage;
use crate::{ErrorKind, ProviderError};

/// Most bytes in one image (AWS: "no more than 3.75 MB").
pub const CONVERSE_MAX_IMAGE_BYTES: usize = 3_750_000;
/// Most bytes in one document (AWS: "no more than 4.5 MB").
pub const CONVERSE_MAX_DOCUMENT_BYTES: usize = 4_500_000;
/// Longest tool call id Converse accepts.
const MAX_TOOL_USE_ID: usize = 64;
/// Longest model id or ARN sent in the path.
const MAX_MODEL_ID: usize = 2048;

fn bad(what: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, what)
}

fn malformed(what: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, what)
}

/// What Converse refuses that the shared checks don't cover, before anything is sent.
pub(super) fn validate(req: &ChatRequest) -> Result<(), ProviderError> {
    if req.messages.first().is_some_and(|m| m.role != Role::User) {
        return Err(bad("a Bedrock conversation must start with a user message"));
    }
    for m in &req.messages {
        let mut document = false;
        let mut text = false;
        for p in &m.parts {
            match p {
                Part::Image { bytes, .. } if bytes.len() > CONVERSE_MAX_IMAGE_BYTES => {
                    return Err(bad(format!("an image of {} bytes is more than the {CONVERSE_MAX_IMAGE_BYTES} Bedrock accepts; send it smaller", bytes.len())));
                }
                Part::Document { bytes, .. } => {
                    if bytes.len() > CONVERSE_MAX_DOCUMENT_BYTES {
                        return Err(bad(format!(
                            "a PDF of {} bytes is more than the {CONVERSE_MAX_DOCUMENT_BYTES} Bedrock accepts; use PDF to text, or send fewer pages",
                            bytes.len()
                        )));
                    }
                    document = true;
                }
                Part::Text(_) => text = true,
                Part::ToolUse { id, .. } | Part::ToolResult { id, .. } if !tool_use_id(id) => {
                    return Err(bad(
                        "a tool call id from an earlier turn isn't one Bedrock accepts (at most 64 letters, digits, `_`, `.`, `:` or `-`); continue with the model that wrote it",
                    ));
                }
                _ => {}
            }
        }
        // "If you include a ContentBlock with a document field in the array, you must also
        // include a ContentBlock with a text field."
        if document && !text {
            return Err(bad("Bedrock needs some text next to a PDF; add a prompt"));
        }
    }
    let model = req.model.trim();
    if model.len() > MAX_MODEL_ID || model.chars().any(char::is_control) {
        return Err(bad("the Bedrock model id isn't valid"));
    }
    Ok(())
}

fn tool_use_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_TOOL_USE_ID && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}

/// A path segment with everything but RFC 3986's unreserved characters percent-encoded: model
/// ids hold `:` and ARNs `/`.
fn segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The image format Converse names, from a MIME type the shared checks allowed.
fn image_format(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpeg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// Whether `model` (an id, inference profile or ARN) takes a tool result's `status`: AWS
/// documents it for Amazon Nova and Anthropic Claude 3 and 4 only (`ToolResultBlock`).
fn takes_tool_status(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.contains("anthropic.") || model.contains("amazon.nova")
}

/// The request body.
pub(super) fn body(req: &ChatRequest) -> Value {
    let mut messages = Vec::new();
    let mut documents = 0usize;
    for m in &req.messages {
        let mut content = Vec::new();
        match m.role {
            Role::User => {
                // Tool results first, then the user's parts in their order.
                for p in &m.parts {
                    if let Part::ToolResult { id, content: text, is_error } = p {
                        // `status` is documented for Amazon Nova and Anthropic Claude only; other
                        // models are told of a failure in the result's text.
                        let status = takes_tool_status(&req.model);
                        let text = if *is_error && !status { format!("Error: {text}") } else { text.clone() };
                        let mut result = json!({ "toolUseId": id, "content": [{ "text": text }] });
                        if *is_error && status {
                            result["status"] = json!("error");
                        }
                        content.push(json!({ "toolResult": result }));
                    }
                }
                for p in &m.parts {
                    match p {
                        Part::Text(t) => content.push(json!({ "text": t })),
                        Part::Image { mime, bytes } => {
                            content.push(json!({ "image": { "format": image_format(mime), "source": { "bytes": base64(bytes) } } }));
                        }
                        Part::Document { bytes, .. } => {
                            documents = documents.saturating_add(1);
                            // A neutral name: the model reads it, so it carries nothing of the file's.
                            content.push(
                                json!({ "document": { "format": "pdf", "name": format!("Document {documents}"), "source": { "bytes": base64(bytes) } } }),
                            );
                        }
                        Part::Opaque { block, .. } => content.push(block.clone()),
                        Part::ToolResult { .. } | Part::ToolUse { .. } => {}
                    }
                }
            }
            Role::Assistant => {
                for p in &m.parts {
                    match p {
                        Part::Text(t) => content.push(json!({ "text": t })),
                        Part::ToolUse { id, name, input } => {
                            let input = if input.is_object() { input.clone() } else { json!({}) };
                            content.push(json!({ "toolUse": { "toolUseId": id, "name": name, "input": input } }));
                        }
                        // Reasoning goes back exactly as the model sent it, signature included.
                        Part::Opaque { block, .. } => content.push(block.clone()),
                        _ => {}
                    }
                }
            }
        }
        if !content.is_empty() {
            let role = if m.role == Role::User { "user" } else { "assistant" };
            messages.push(json!({ "role": role, "content": content }));
        }
    }
    let mut out = json!({ "messages": messages });
    if let Some(system) = req.system.as_deref().filter(|s| !s.trim().is_empty()) {
        out["system"] = json!([{ "text": system }]);
    }
    let mut inference = Map::new();
    if let Some(max) = req.max_tokens {
        inference.insert("maxTokens".into(), json!(max.max(1)));
    }
    if let Some(t) = rig_chat::temperature(req.temperature) {
        inference.insert("temperature".into(), json!(t));
    }
    if !inference.is_empty() {
        out["inferenceConfig"] = Value::Object(inference);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut spec = json!({ "name": t.name, "inputSchema": { "json": t.input_schema } });
                // Converse refuses an empty description.
                if !t.description.trim().is_empty() {
                    spec["description"] = json!(t.description);
                }
                if t.strict {
                    spec["strict"] = json!(true);
                }
                json!({ "toolSpec": spec })
            })
            .collect();
        out["toolConfig"] = json!({ "tools": tools });
    }
    out
}

/// A Converse stop reason. Content and guardrail stops are refusals; malformed output is an
/// error, since there is no reply to use.
fn stop(reason: Option<&str>) -> Result<StopReason, ProviderError> {
    Ok(match reason {
        Some("end_turn") | Some("stop_sequence") => StopReason::EndTurn,
        // Both cut the reply off.
        Some("max_tokens") | Some("model_context_window_exceeded") => StopReason::MaxTokens,
        Some("tool_use") => StopReason::ToolUse,
        Some("guardrail_intervened") => return Err(ProviderError::new(ErrorKind::BlockedContent, "a Bedrock guardrail blocked this request")),
        Some("content_filtered") => return Err(ProviderError::new(ErrorKind::BlockedContent, "Bedrock's content filter blocked this reply")),
        Some(what @ ("malformed_model_output" | "malformed_tool_use")) => return Err(malformed(format!("the model's output couldn't be read ({what})"))),
        Some(other) => StopReason::Other(crate::error::excerpt(other.as_bytes())),
        None => return Err(malformed("Converse reply without a stop reason")),
    })
}

/// The reported token counts.
fn usage(v: Option<&Value>) -> Usage {
    Usage {
        input_tokens: v.and_then(|u| u.get("inputTokens")).and_then(Value::as_u64),
        output_tokens: v.and_then(|u| u.get("outputTokens")).and_then(Value::as_u64),
        cost: None,
        billable_units: None,
        execution_ms: None,
    }
}

/// A finished content block as a part: text and tool calls are understood; reasoning and
/// everything else is kept as it came, to go back in later turns.
fn part(block: &Value) -> Result<Option<Part>, ProviderError> {
    let Some(object) = block.as_object() else { return Err(malformed("a Converse content block isn't an object")) };
    if let Some(text) = object.get("text") {
        let text = text.as_str().ok_or_else(|| malformed("a Converse text block isn't text"))?;
        return Ok((!text.is_empty()).then(|| Part::Text(text.to_owned())));
    }
    if let Some(call) = object.get("toolUse") {
        let field = |k: &str| call.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_owned);
        let (Some(id), Some(name)) = (field("toolUseId"), field("name")) else { return Err(malformed("a tool call without an id or name")) };
        let input = call.get("input").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
        return Ok(Some(Part::ToolUse { id, name, input }));
    }
    Ok(Some(Part::Opaque { dialect: Dialect::Converse, block: block.clone() }))
}

/// A whole Converse reply.
pub(super) fn reply(v: &Value) -> Result<ChatResponse, ProviderError> {
    // Stop reason first: a refusal must not surface partial text as a result.
    let stop = stop(v.get("stopReason").and_then(Value::as_str))?;
    let content = v.pointer("/output/message/content").and_then(Value::as_array).ok_or_else(|| malformed("Converse reply without content"))?;
    if content.len() > MAX_REPLY_BLOCKS {
        return Err(malformed("the reply has too many parts"));
    }
    let mut parts = Vec::new();
    for block in content {
        parts.extend(part(block)?);
    }
    Ok(ChatResponse::new(parts, stop, usage(v.get("usage"))))
}

/// The error a Converse error name means, with `message` from the server.
fn named(name: &str, message: &str, status: Option<u16>) -> ProviderError {
    // Names come as `ThrottlingException` (HTTP), `throttlingException` (stream events), or with
    // a namespace or URL around them.
    let name = name.rsplit('#').next().unwrap_or(name);
    let name = name.split(':').next().unwrap_or(name).trim().to_ascii_lowercase();
    let message = crate::error::excerpt(message.as_bytes());
    let kind = match name.as_str() {
        "throttlingexception" | "toomanyrequestsexception" => ErrorKind::RateLimited,
        // A quota, not a burst: waiting won't help, so it is not transient.
        "servicequotaexceededexception" => {
            return ProviderError::new(
                ErrorKind::Budget,
                format!("the AWS account's Bedrock quota for this model is used up; raise it in Service Quotas (the provider said: {message})"),
            );
        }
        "accessdeniedexception"
        | "unrecognizedclientexception"
        | "invalidsignatureexception"
        | "incompletesignature"
        | "invalidclienttokenid"
        | "expiredtokenexception"
        | "notauthorized"
        | "missingauthenticationtokenexception"
        | "unauthorizedexception" => ErrorKind::Auth,
        "validationexception" | "validationerror" => {
            // Bedrock names a prompt past the model's context window only in its message.
            let lower = message.to_ascii_lowercase();
            if lower.contains("too long") || lower.contains("context window") || lower.contains("too many input tokens") {
                return ProviderError::context_limit(&message);
            }
            ErrorKind::UnsupportedInput
        }
        "modelnotreadyexception" | "serviceunavailableexception" | "serviceunavailable" => ErrorKind::Busy,
        "modeltimeoutexception" => ErrorKind::Timeout,
        "resourcenotfoundexception" => ErrorKind::NotFound,
        "internalserverexception" | "internalfailure" | "modelerrorexception" | "modelstreamerrorexception" => ErrorKind::Server,
        _ => match status {
            Some(s) => ProviderError::from_status(s, b"").kind,
            None => ErrorKind::Server,
        },
    };
    ProviderError::new(kind, message)
}

/// An error status's reply: the error's name from `x-amzn-ErrorType` or the body's `__type`,
/// its message from the body.
pub(super) fn error(status: u16, headers: &[(String, String)], body: &[u8]) -> ProviderError {
    let json = serde_json::from_slice::<Value>(body).ok();
    let message = json.as_ref().and_then(|v| v.get("message").or_else(|| v.get("Message"))).and_then(Value::as_str);
    let name = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("x-amzn-errortype"))
        .map(|(_, v)| v.as_str())
        .or_else(|| json.as_ref().and_then(|v| v.get("__type")).and_then(Value::as_str));
    let mut e = match (name, message) {
        (Some(name), message) => named(name, message.unwrap_or("request failed"), Some(status)),
        (None, Some(message)) => {
            let mut e = ProviderError::from_status(status, b"");
            e.message = crate::error::excerpt(message.as_bytes());
            e
        }
        (None, None) => ProviderError::from_status(status, body),
    };
    e.retry_after_ms = retry_after(headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("retry-after")).map(|(_, v)| v.as_str()));
    e
}

/// A block being streamed.
enum Block {
    Text(String),
    Tool { id: String, name: String, input: String },
    Reasoning { text: String, signature: Option<String>, redacted: Option<String> },
}

/// A streamed reply, assembled one event at a time.
pub(super) struct Stream {
    decoder: event_stream::Decoder,
    blocks: BTreeMap<u64, Block>,
    /// `messageStop` arrived, with the stop reason it gave.
    stop: Option<Option<String>>,
    usage: Option<Value>,
}

impl Stream {
    pub(super) fn new() -> Self {
        Self { decoder: event_stream::Decoder::new(), blocks: BTreeMap::new(), stop: None, usage: None }
    }

    /// Takes the next piece of the body; `on_text` gets each piece of reply text.
    pub(super) fn push(&mut self, bytes: &[u8], on_text: &mut dyn FnMut(&str)) -> Result<(), ProviderError> {
        for message in self.decoder.push(bytes)? {
            self.event(&message, on_text)?;
        }
        Ok(())
    }

    fn event(&mut self, message: &event_stream::Message, on_text: &mut dyn FnMut(&str)) -> Result<(), ProviderError> {
        let json = || serde_json::from_slice::<Value>(&message.payload).map_err(|_| malformed("a Converse stream event isn't JSON"));
        match message.header(":message-type") {
            Some("event") => {}
            Some("exception") => {
                let name = message.header(":exception-type").unwrap_or("internalServerException");
                let body = json().unwrap_or(Value::Null);
                let text = body.get("message").or_else(|| body.get("Message")).and_then(Value::as_str).unwrap_or("the reply stream failed");
                return Err(named(name, text, None));
            }
            Some("error") => {
                let name = message.header(":error-code").unwrap_or("InternalFailure");
                return Err(named(name, message.header(":error-message").unwrap_or("the reply stream failed"), None));
            }
            _ => return Err(malformed("a Converse stream event has no known message type")),
        }
        let event = json()?;
        let index = || -> Result<u64, ProviderError> {
            // Stricter than the wire needs: any index that isn't a plain integer under the limit
            // is refused, so nothing is sized by it.
            event
                .get("contentBlockIndex")
                .and_then(Value::as_u64)
                .filter(|i| *i < MAX_REPLY_BLOCKS as u64)
                .ok_or_else(|| malformed("the reply has too many parts"))
        };
        match message.header(":event-type") {
            Some("contentBlockStart") => {
                let i = index()?;
                if let Some(call) = event.pointer("/start/toolUse") {
                    let field = |k: &str| call.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_owned);
                    let (Some(id), Some(name)) = (field("toolUseId"), field("name")) else { return Err(malformed("a tool call without an id or name")) };
                    self.insert(i, Block::Tool { id, name, input: String::new() })?;
                }
            }
            Some("contentBlockDelta") => {
                let i = index()?;
                let delta = event.get("delta").ok_or_else(|| malformed("a Converse delta without a delta"))?;
                if let Some(text) = delta.get("text").and_then(Value::as_str) {
                    match self.block(i, || Block::Text(String::new())) {
                        Block::Text(t) => t.push_str(text),
                        _ => return Err(malformed("a Converse stream mixed kinds in one block")),
                    }
                    on_text(text);
                } else if let Some(input) = delta.pointer("/toolUse/input").and_then(Value::as_str) {
                    match self.blocks.get_mut(&i) {
                        Some(Block::Tool { input: all, .. }) => all.push_str(input),
                        _ => return Err(malformed("a tool call's input arrived before the call")),
                    }
                } else if let Some(reasoning) = delta.get("reasoningContent") {
                    let new = || Block::Reasoning { text: String::new(), signature: None, redacted: None };
                    let Block::Reasoning { text, signature, redacted } = self.block(i, new) else {
                        return Err(malformed("a Converse stream mixed kinds in one block"));
                    };
                    if let Some(piece) = reasoning.get("text").and_then(Value::as_str) {
                        text.push_str(piece);
                    }
                    if let Some(piece) = reasoning.get("signature").and_then(Value::as_str) {
                        signature.get_or_insert_with(String::new).push_str(piece);
                    }
                    if let Some(piece) = reasoning.get("redactedContent").and_then(Value::as_str) {
                        redacted.get_or_insert_with(String::new).push_str(piece);
                    }
                }
                // Other deltas (citations, images) aren't read.
            }
            Some("contentBlockStop") => {
                index()?;
            }
            Some("messageStop") => {
                self.stop = Some(event.get("stopReason").and_then(Value::as_str).map(str::to_owned));
            }
            Some("metadata") => {
                if let Some(counts) = event.get("usage").filter(|u| u.is_object()) {
                    self.usage = Some(counts.clone());
                }
            }
            // `messageStart` and anything newer carry nothing this reads.
            _ => {}
        }
        Ok(())
    }

    fn insert(&mut self, i: u64, block: Block) -> Result<(), ProviderError> {
        if self.blocks.insert(i, block).is_some() {
            return Err(malformed("a Converse stream started one block twice"));
        }
        Ok(())
    }

    fn block(&mut self, i: u64, new: impl FnOnce() -> Block) -> &mut Block {
        self.blocks.entry(i).or_insert_with(new)
    }

    /// The stream ended: the whole reply, once it ended the turn.
    pub(super) fn finish(self) -> Result<ChatResponse, ProviderError> {
        self.decoder.finish()?;
        let Some(reason) = self.stop else { return Err(ProviderError::new(ErrorKind::Network, "the reply stream ended early")) };
        let stop = stop(reason.as_deref())?;
        let mut parts = Vec::new();
        for block in self.blocks.into_values() {
            parts.extend(match block {
                Block::Text(t) => (!t.is_empty()).then_some(Part::Text(t)),
                Block::Tool { id, name, input } => {
                    // Input that isn't a JSON object stays text, which fails any object schema.
                    let input = if input.trim().is_empty() {
                        json!({})
                    } else {
                        match serde_json::from_str::<Value>(&input) {
                            Ok(v) if v.is_object() => v,
                            _ => Value::String(input),
                        }
                    };
                    Some(Part::ToolUse { id, name, input })
                }
                Block::Reasoning { text, signature, redacted } => {
                    let block = match redacted {
                        Some(redacted) => json!({ "reasoningContent": { "redactedContent": redacted } }),
                        None => {
                            let mut reasoning = json!({ "text": text });
                            if let Some(signature) = signature {
                                reasoning["signature"] = json!(signature);
                            }
                            json!({ "reasoningContent": { "reasoningText": reasoning } })
                        }
                    };
                    Some(Part::Opaque { dialect: Dialect::Converse, block })
                }
            });
        }
        Ok(ChatResponse::new(parts, stop, usage(self.usage.as_ref())))
    }
}

impl LlmClient {
    /// Sends `req` to Converse, streamed when `on_text` takes the pieces, through the runner's
    /// delivery scope when there is one.
    pub(super) async fn converse_chat(
        &self,
        req: &ChatRequest,
        on_text: Option<&mut (dyn FnMut(&str) + Send)>,
        context: Option<&crate::SubmitContext>,
    ) -> Result<ChatResponse, ProviderError> {
        let req = self.validated(req)?;
        validate(&req)?;
        let streamed = on_text.is_some();
        let (cap, timeout) = self.limits(&req, streamed);
        let path = format!("model/{}/{}", segment(req.model.trim()), if streamed { "converse-stream" } else { "converse" });
        let mut request = HttpRequest::post_json(self.endpoint.url(&path), &body(&req))?.timeout_ms(timeout).max_response_bytes(cap);
        if streamed {
            request = request.header("Accept", "application/vnd.amazon.eventstream");
        }
        let request = self.auth(request);
        let Some(on_text) = on_text else {
            let response = match context {
                Some(context) => context.send(self.http.as_ref(), request).await?,
                None => self.http.send(request).await?,
            };
            if !response.is_success() {
                return Err(error(response.status, &response.headers, &response.body));
            }
            return reply(&response.json()?);
        };
        let mut stream = Stream::new();
        // The first error the reply's bytes raised: the transport's own error for a stopped sink
        // says less.
        let mut failure: Option<ProviderError> = None;
        let sent = {
            let mut sink = |bytes: &[u8]| -> Result<(), ProviderError> {
                let result = stream.push(bytes, &mut |text| on_text(text));
                if let Err(e) = &result {
                    failure.get_or_insert_with(|| e.clone());
                }
                result
            };
            match context {
                Some(context) => {
                    let link = context.link();
                    link.send_streaming(self.http.as_ref(), request, &mut sink).await
                }
                None => self.http.send_streaming(request, &mut sink).await,
            }
        };
        if let Some(e) = failure {
            return Err(e);
        }
        let response = sent?;
        if !response.is_success() {
            return Err(error(response.status, &response.headers, &response.body));
        }
        stream.finish()
    }
}

#[cfg(test)]
mod tests;

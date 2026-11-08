//! Gemini on Google Cloud's Vertex AI (#1151): `generateContent` and `streamGenerateContent`
//! under `projects/{project}/locations/{location}/publishers/google/models/{model}`.
//!
//! Spoken through `rig-core`'s Gemini GenerateContent wire (#1122,
//! docs/decisions/2026-11-08-vertex-gemini-chat.md): Vertex AI's REST request and reply are the
//! Gemini API's (`contents`, `systemInstruction`, `tools`, `generationConfig`; `candidates`,
//! `finishReason`, `usageMetadata`), so the crate builds the body and reads the reply, whole or
//! streamed (`?alt=sse`), and every byte goes through NodeCraft's own [`crate::Http`]
//! ([`RigHttp`]) at our own URL. Where Vertex AI's `v1` differs, the body is brought to its shape
//! ([`fix`]). The access token never reaches the crate: it is a secret header on the transport,
//! from the service account's [`Tokens`] (an hour's token, replaced five minutes before it
//! lapses, dropped when Vertex AI answers 401).
//!
//! NodeCraft keeps validation, caps, keys, error kinds and money; replies are never priced from
//! the crate's catalog, and a request that may have been sent is never sent again.

use std::sync::Arc;

use rig_core::Model;
use rig_core::completion::{CompletionRequest, CompletionResponse};
use rig_core::message::{
    Api, AssistantContent, AssistantMessage, CallId, Document, DocumentData, DocumentMediaType, DocumentSourceKind, Image, ImageMediaType,
    Message as RigMessage, MimeType as _, Opaque, Origin, Text, ToolCall as RigCall, ToolFunction, ToolName, ToolResult, ToolResultContent, UserContent,
};
use rig_core::providers::gemini::GeminiConfig;
use rig_core::providers::gemini::completion::{GenerateContent, PROVIDER_NAME};
use serde_json::{Map, Value, json};

use super::{ApiKey, ChatRequest, ChatResponse, Dialect, Endpoint, LlmClient, MAX_REPLY_BLOCKS, Part, Role, StopReason, base64, rig_chat};
use crate::google_cloud::{Clock, ServiceAccount, Tokens};
use crate::provider::Usage;
use crate::rig_http::{Exchange, RigHttp};
use crate::secrets::Secret;
use crate::{ErrorKind, Http, ProviderError};

/// The provider id and scheduler lane of Gemini chat on Google Cloud. Its key is the
/// `google-cloud` service account Veo, Cloud TTS and Gemini images use.
pub const GOOGLE_GEMINI_LLM: &str = "google-gemini-llm";
/// The global location: Vertex AI serves current Gemini models there, on
/// `aiplatform.googleapis.com`.
pub const VERTEX_GLOBAL: &str = "global";
/// The API the crate's GenerateContent wire names its turns by.
const API: &str = "gemini.generate_content";
/// Longest model id accepted in the path (`gemini-3.8-flash`, `gemini-2.5-pro-preview-06-05`).
const MAX_MODEL: usize = 128;
/// Longest location code accepted (`northamerica-northeast1` is 23).
const MAX_LOCATION: usize = 32;
/// The marker block that carries a function call's thought signature to the next turn.
const CALL_SIGNATURE: &str = "vertexCallSignature";
/// Most candidates a streamed event may carry: NodeCraft asks for one.
const MAX_CANDIDATES: usize = 8;
/// Most citations one event may carry.
const MAX_CITATIONS: usize = 64;

/// A Vertex AI location: `global`, or a region code such as `us-central1` or `europe-west4`
/// (lowercase words joined by hyphens, the last ending in a number). Errors never echo it.
pub fn validate_vertex_location(location: &str) -> Result<(), ProviderError> {
    let bad = || ProviderError::new(ErrorKind::UnsupportedInput, "set the Vertex AI location to global or a region such as us-central1");
    if location == VERTEX_GLOBAL {
        return Ok(());
    }
    if location.is_empty() || location.len() > MAX_LOCATION {
        return Err(bad());
    }
    let parts: Vec<&str> = location.split('-').collect();
    let Some((last, words)) = parts.split_last() else { return Err(bad()) };
    let letters = |w: &&str| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase());
    let numbered = last.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && last.bytes().last().is_some_and(|b| b.is_ascii_digit())
        && last.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if words.is_empty() || words.len() > 2 || !words.iter().all(letters) || !numbered {
        return Err(bad());
    }
    Ok(())
}

/// The API host for `location`.
fn host(location: &str) -> String {
    if location == VERTEX_GLOBAL { "aiplatform.googleapis.com".to_owned() } else { format!("{location}-aiplatform.googleapis.com") }
}

/// A model id as it may go in the path: lowercase letters, digits, `.`, `-`, `_` and `@`.
fn check_model(model: &str) -> Result<(), ProviderError> {
    let ok = !model.is_empty()
        && model.len() <= MAX_MODEL
        && model.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-_@".contains(&b))
        && model.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric());
    if ok { Ok(()) } else { Err(ProviderError::new(ErrorKind::UnsupportedInput, "that isn't a Vertex AI Gemini model id (such as gemini-3.8-flash)")) }
}

impl LlmClient {
    /// Gemini on Vertex AI in `location` (`global`, or a region), signed in as the service
    /// account stored as `secret` (see [`crate::google_cloud::compact`]) and billed to its
    /// project. Constructing it reads the account and sends nothing; the first chat exchanges a
    /// signed JWT for an access token, free, at Google's token endpoint.
    pub fn google_vertex(secret: &Secret, location: &str, http: Arc<dyn Http>, clock: Clock) -> Result<Self, ProviderError> {
        let location = location.trim();
        validate_vertex_location(location)?;
        let account = ServiceAccount::from_secret(secret)?;
        let base_url = format!("https://{}/v1/projects/{}/locations/{location}/publishers/google/models", host(location), account.project());
        let endpoint = Endpoint { id: GOOGLE_GEMINI_LLM.into(), dialect: Dialect::Vertex, base_url, key: None };
        let tokens = Tokens::new(account, Arc::clone(&http), clock);
        Ok(Self { google: Some(Arc::new(tokens)), ..Self::new(endpoint, http) })
    }

    /// Gemini on Vertex AI in express mode: an API key (`x-goog-api-key`) on the global
    /// endpoint, with no project or location in the path. Google offers it to new accounts for a
    /// limited set of models and quotas
    /// (<https://docs.cloud.google.com/vertex-ai/generative-ai/docs/start/express-mode/overview>).
    pub fn google_vertex_express(key: ApiKey, http: Arc<dyn Http>) -> Self {
        let base_url = format!("https://{}/v1/publishers/google/models", host(VERTEX_GLOBAL));
        Self::new(Endpoint { id: GOOGLE_GEMINI_LLM.into(), dialect: Dialect::Vertex, base_url, key: Some(key) }, http)
    }

    /// Sends `req` to Gemini on Vertex AI, streamed when `on_text` takes the pieces, through the
    /// runner's delivery scope when there is one.
    pub(super) async fn vertex_chat(
        &self,
        req: &ChatRequest,
        on_text: Option<&mut (dyn FnMut(&str) + Send)>,
        context: Option<&crate::SubmitContext>,
    ) -> Result<ChatResponse, ProviderError> {
        let req = self.validated(req)?;
        check_model(&req.model)?;
        let streamed = on_text.is_some();
        let (cap, timeout) = self.limits(&req, streamed);
        let (request, plan) = request(&req)?;
        // `alt=sse` makes the streamed reply server-sent events rather than one JSON array
        // (https://docs.cloud.google.com/apis/docs/system-parameters, `alt`).
        let verb = if streamed { "streamGenerateContent?alt=sse" } else { "generateContent" };
        let url = self.endpoint.url(&format!("{}:{verb}", req.model));
        // The token exchange is free; a refused one leaves nothing sent.
        let bearer = match &self.google {
            Some(tokens) => Some(tokens.bearer().await?),
            None => None,
        };
        let mut transport = RigHttp::new(Arc::clone(&self.http), timeout, cap)
            .with_delivery(context.map(crate::SubmitContext::link))
            .with_url(url)
            .with_fix(Box::new(move |body| fix(body, &plan)))
            .with_reply_fix(whole_reply)
            .with_guard(guard);
        match (&bearer, &self.endpoint.key) {
            (Some(token), _) => transport = transport.with_secret("Authorization", format!("Bearer {token}")),
            (None, Some(key)) => transport = transport.with_secret("x-goog-api-key", key.expose().to_owned()),
            (None, None) => {}
        }
        // The token goes with the transport, never through the crate: its key is empty, and the
        // URL it builds (with that key in its query) is replaced by ours.
        let config = GeminiConfig::new("").with_base_url(self.endpoint.base_url.as_str());
        let model = Model::new(GenerateContent::new(config, req.model.as_str()), transport.clone()).with_idle_timeout(None);
        let sent = rig_chat::send(model, &transport, request, on_text, &RULES).await;
        if let (Exchange::Answered { status: 401, .. }, Some(tokens)) = (transport.exchange(), &self.google) {
            tokens.forget();
        }
        let response = sent.map_err(|e| withheld(e, bearer.as_deref()))?;
        reply(&response)
    }
}

/// `error` without the access token, should Google's words ever quote it.
fn withheld(mut error: ProviderError, bearer: Option<&str>) -> ProviderError {
    if let Some(token) = bearer.filter(|t| !t.is_empty())
        && error.message.contains(token)
    {
        error.message = error.message.replace(token, "[redacted]");
    }
    error
}

fn tool_name(name: &str) -> Result<ToolName, ProviderError> {
    ToolName::new(name).map_err(|_| ProviderError::new(ErrorKind::UnsupportedInput, "a tool call has no name"))
}

fn malformed(what: &str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, what.to_owned())
}

/// What [`fix`] puts back into the body: for each assistant turn that made tool calls, in order,
/// the thought signature each of its calls carried when Gemini sent it (`None` where it carried
/// none, or the turn came from another model).
#[derive(Debug, Default)]
struct Plan {
    signatures: Vec<Vec<Option<String>>>,
}

/// The request in the crate's terms, and what [`fix`] needs.
fn request(req: &ChatRequest) -> Result<(CompletionRequest, Plan), ProviderError> {
    let mut history = Vec::new();
    if let Some(system) = &req.system {
        history.push(RigMessage::System { content: system.clone() });
    }
    let mut plan = Plan::default();
    // Each call's name, by id, and the order of the last turn's calls, which Vertex AI matches
    // its function responses by.
    let mut names: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::User => {
                let mut content = Vec::new();
                // Function responses first, in the order of the calls they answer: Vertex AI's
                // v1 matches them by name and position, not by id.
                let mut results: Vec<(usize, &Part)> = m
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::ToolResult { id, .. } => Some((order.iter().position(|c| c == id).unwrap_or(usize::MAX), p)),
                        _ => None,
                    })
                    .collect();
                results.sort_by_key(|(at, _)| *at);
                for (_, p) in results {
                    if let Part::ToolResult { id, content: text, is_error } = p {
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
                let marked: std::collections::HashMap<&str, &str> = m
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Opaque { block, .. } => {
                            let mark = block.get(CALL_SIGNATURE)?;
                            Some((mark.get("id")?.as_str()?, mark.get("thoughtSignature")?.as_str()?))
                        }
                        _ => None,
                    })
                    .collect();
                let mut content = Vec::new();
                let mut signatures = Vec::new();
                order.clear();
                for p in &m.parts {
                    content.push(match p {
                        Part::Text(t) => AssistantContent::text(t.clone()),
                        Part::ToolUse { id, name, input } => {
                            names.insert(id, name);
                            order.push(id);
                            signatures.push(marked.get(id.as_str()).map(|s| (*s).to_owned()));
                            // Gemini's own calls always carry an object; arguments from another
                            // server that weren't one go as `{}`.
                            let input = if input.is_object() { input.clone() } else { json!({}) };
                            AssistantContent::ToolCall(RigCall::new(CallId::from_wire(id.clone()), ToolFunction::new(tool_name(name)?, input)))
                        }
                        Part::Opaque { block, .. } if block.get(CALL_SIGNATURE).is_some() => continue,
                        // Thoughts and other parts go back exactly as Gemini sent them.
                        Part::Opaque { block, .. } => AssistantContent::Opaque(Opaque { item: block.clone(), replay: true }),
                        _ => continue,
                    });
                }
                if !signatures.is_empty() {
                    plan.signatures.push(signatures);
                }
                if !content.is_empty() {
                    // Named as this wire's own turn, so the crate keeps its parts.
                    let mut turn = AssistantMessage::new(content);
                    turn.origin = Some(Origin::new(Api::from_static(API), PROVIDER_NAME, req.model.as_str()));
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
        // Sent as written, as JSON Schema (`parametersJsonSchema`, which Vertex AI's v1 takes in
        // place of its OpenAPI `parameters`). Gemini has no strict mode; the model picks (`AUTO`).
        let declarations: Vec<Value> =
            req.tools.iter().map(|t| json!({ "name": t.name, "description": t.description, "parametersJsonSchema": t.input_schema })).collect();
        out.additional_params = Some(json!({ "tools": [{ "functionDeclarations": declarations }] }));
    }
    out.accept_unknown_finish_reasons = true;
    Ok((out, plan))
}

/// Drops `null` members of `object`, which Vertex AI's JSON reader has no use for.
fn drop_nulls(object: &mut Map<String, Value>) {
    object.retain(|_, v| !v.is_null());
}

/// The request in the shape Vertex AI's `v1` takes, where the wire differs:
/// - members the wire spells `null` (`safetySettings`, `toolConfig`, an empty
///   `generationConfig`, a tool's `codeExecution`) are left out;
/// - the system instruction names no role (Vertex AI ignores it; the wire writes `model`);
/// - function calls and responses carry no `id`: `v1` has no such field (it matches responses
///   to calls by name and order), and the wire adds one for Gemini 3;
/// - a function's result goes under `output`, the key Vertex AI documents (`error` stays);
/// - each call Gemini signed carries its thought signature back as it came; in a turn that has
///   one, the wire's placeholder on the other calls is removed (Gemini signs only the first of
///   parallel calls). A turn from another model keeps the placeholder Gemini 3 requires.
fn fix(body: &mut Value, plan: &Plan) {
    let Some(top) = body.as_object_mut() else { return };
    drop_nulls(top);
    if let Some(system) = top.get_mut("systemInstruction").and_then(Value::as_object_mut) {
        system.remove("role");
    }
    for tool in top.get_mut("tools").and_then(Value::as_array_mut).into_iter().flatten() {
        if let Some(tool) = tool.as_object_mut() {
            drop_nulls(tool);
        }
    }
    let mut turns = plan.signatures.iter();
    for content in top.get_mut("contents").and_then(Value::as_array_mut).into_iter().flatten() {
        let model = content.get("role").and_then(Value::as_str) == Some("model");
        let Some(parts) = content.get_mut("parts").and_then(Value::as_array_mut) else { continue };
        for part in parts.iter_mut().filter_map(Value::as_object_mut) {
            if let Some(call) = part.get_mut("functionCall").and_then(Value::as_object_mut) {
                call.remove("id");
            }
            if let Some(response) = part.get_mut("functionResponse").and_then(Value::as_object_mut) {
                response.remove("id");
                if let Some(answer) = response.get_mut("response").and_then(Value::as_object_mut)
                    && let Some(result) = answer.remove("result")
                {
                    answer.insert("output".into(), result);
                }
            }
        }
        let calls = parts.iter().filter(|p| p.get("functionCall").is_some()).count();
        if !model || calls == 0 {
            continue;
        }
        let Some(signatures) = turns.next() else { continue };
        let signed = signatures.iter().any(Option::is_some);
        let mut at = 0usize;
        for part in parts.iter_mut().filter_map(Value::as_object_mut).filter(|p| p.contains_key("functionCall")) {
            match signatures.get(at).cloned().flatten() {
                Some(signature) => {
                    part.insert("thoughtSignature".into(), Value::String(signature));
                }
                None if signed => {
                    part.remove("thoughtSignature");
                }
                None => {}
            }
            at = at.saturating_add(1);
        }
    }
}

/// A whole reply with what the crate would search at length taken out: grounding and citation
/// metadata, which NodeCraft never asks for and doesn't show. A hostile reply could make the
/// crate match every grounding segment against the whole answer.
fn whole_reply(reply: &mut Value) {
    for candidate in reply.get_mut("candidates").and_then(Value::as_array_mut).into_iter().flatten() {
        if let Some(candidate) = candidate.as_object_mut() {
            candidate.remove("groundingMetadata");
            candidate.remove("citationMetadata");
        }
    }
}

/// Checks a streamed reply's events before the wire reads them:
/// - more candidates than NodeCraft could have asked for, or a candidate index past them, is
///   refused: the wire keeps candidates by index and looks each one up among those it holds;
/// - the parts that open a block of their own (calls, thoughts after text and back, signed text,
///   anything that isn't text) are counted, and a reply past the block limit is refused;
/// - grounding segments are refused (the wire matches each against the whole answer so far, on
///   every event: NodeCraft never asks for grounding), and an event's citations are capped.
fn guard() -> crate::rig_http::events::Guard {
    let mut blocks = 0usize;
    let mut last_thought: Option<bool> = None;
    Box::new(move |data| {
        let Ok(event) = serde_json::from_str::<Value>(data) else { return Ok(None) };
        let Some(candidates) = event.get("candidates").and_then(Value::as_array) else { return Ok(None) };
        if candidates.len() > MAX_CANDIDATES
            || candidates.iter().any(|c| c.get("index").is_some_and(|i| !i.as_u64().is_some_and(|i| i < MAX_CANDIDATES as u64)))
        {
            return Err(malformed("the reply has more candidates than were asked for"));
        }
        for candidate in candidates {
            if candidate.pointer("/groundingMetadata/groundingSupports").and_then(Value::as_array).is_some_and(|s| !s.is_empty()) {
                return Err(malformed("the reply carries grounding NodeCraft didn't ask for"));
            }
            let citations = ["/citationMetadata/citations", "/citationMetadata/citationSources"]
                .iter()
                .filter_map(|p| candidate.pointer(p).and_then(Value::as_array))
                .map(Vec::len)
                .fold(0usize, usize::saturating_add);
            if citations > MAX_CITATIONS {
                return Err(malformed("the reply has too many citations"));
            }
            let parts = candidate.pointer("/content/parts").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
            for part in parts {
                let thought = part.get("thought").and_then(Value::as_bool) == Some(true);
                let signed = part.get("thoughtSignature").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
                let opens = match part.get("text") {
                    Some(_) => signed || last_thought != Some(thought),
                    None => part.as_object().is_some_and(|p| p.keys().any(|k| !["thought", "thoughtSignature", "partMetadata"].contains(&k.as_str()))),
                };
                last_thought = if part.get("text").is_some() { Some(thought) } else { None };
                if opens {
                    blocks = blocks.saturating_add(1);
                }
            }
        }
        if blocks > MAX_REPLY_BLOCKS {
            return Err(malformed("the reply has too many parts"));
        }
        Ok(None)
    })
}

/// A finish reason that means one of Google's filters stopped the answer.
fn filtered(reason: &str) -> bool {
    matches!(
        reason,
        "SAFETY"
            | "BLOCKLIST"
            | "PROHIBITED_CONTENT"
            | "SPII"
            | "RECITATION"
            | "IMAGE_SAFETY"
            | "IMAGE_PROHIBITED_CONTENT"
            | "IMAGE_RECITATION"
            | "MODEL_ARMOR"
    )
}

/// A reason code as Google spells it, or nothing for anything else.
fn code(reason: &str) -> Option<&str> {
    Some(reason).filter(|r| !r.is_empty() && r.len() <= 40 && r.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'))
}

/// The reply as Gemini sent it, from the crate's document of it (whole, or rebuilt from the
/// stream): its text, its calls (each signature kept for the next turn), and its thoughts and
/// any other part kept as they came.
fn reply(response: &CompletionResponse) -> Result<ChatResponse, ProviderError> {
    let v = &response.raw;
    let candidate = v.pointer("/candidates/0").filter(|c| c.is_object()).ok_or_else(|| malformed("Vertex AI reply without candidates"))?;
    let reason = candidate.get("finishReason").and_then(Value::as_str).unwrap_or_default();
    // A filter's stop first: a blocked answer must not surface its partial text.
    if filtered(reason) {
        let words = candidate.get("finishMessage").and_then(Value::as_str).map(|m| crate::error::excerpt(m.as_bytes()));
        let base = format!("Google's filters stopped the answer ({})", code(reason).unwrap_or("filtered"));
        return Err(ProviderError::new(
            ErrorKind::BlockedContent,
            match words {
                Some(words) => format!("{base}: {words}"),
                None => base,
            },
        ));
    }
    let raw = candidate.pointer("/content/parts").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    if raw.len() > MAX_REPLY_BLOCKS {
        return Err(malformed("the reply has too many parts"));
    }
    let mut parts: Vec<Part> = Vec::new();
    let mut calls = 0usize;
    for part in raw {
        let Some(fields) = part.as_object() else { continue };
        let thought = part.get("thought").and_then(Value::as_bool) == Some(true);
        let signature = part.get("thoughtSignature").and_then(Value::as_str).filter(|s| !s.is_empty());
        if let Some(call) = part.get("functionCall") {
            let name = call.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).ok_or_else(|| malformed("a tool call without a name"))?;
            // Vertex AI's v1 sends no call ids; calls are numbered, as before for other servers.
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|i| !i.is_empty() && i.len() <= super::MAX_TOOL_ID)
                .map_or_else(|| format!("call_{calls}"), str::to_owned);
            calls = calls.saturating_add(1);
            let input = call.get("args").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
            parts.push(Part::ToolUse { id: id.clone(), name: name.to_owned(), input });
            if let Some(signature) = signature {
                parts.push(Part::Opaque { dialect: Dialect::Vertex, block: json!({ CALL_SIGNATURE: { "id": id, "thoughtSignature": signature } }) });
            }
            continue;
        }
        match part.get("text").and_then(Value::as_str) {
            // Answer text; a signature on it may be left out of later turns (Google enforces only
            // the calls').
            Some(text) if !thought => {
                if text.is_empty() {
                    continue;
                }
                match parts.last_mut() {
                    Some(Part::Text(held)) => held.push_str(text),
                    _ => parts.push(Part::Text(text.to_owned())),
                }
            }
            _ => {
                let data = fields.keys().any(|k| !["thought", "thoughtSignature", "partMetadata"].contains(&k.as_str()));
                if data || thought {
                    parts.push(Part::Opaque { dialect: Dialect::Vertex, block: part.clone() });
                }
            }
        }
    }
    let stop = match reason {
        "" | "STOP" if calls > 0 => StopReason::ToolUse,
        "" | "STOP" => StopReason::EndTurn,
        "MAX_TOKENS" => StopReason::MaxTokens,
        other => StopReason::Other(code(other).unwrap_or("OTHER").to_owned()),
    };
    let usage = v.get("usageMetadata");
    let count = |key: &str| usage.and_then(|u| u.get(key)).and_then(Value::as_u64);
    // Thinking is billed as output: it counts with the answer, which `candidatesTokenCount`
    // leaves it out of (`totalTokenCount` adds `thoughtsTokenCount` apart).
    let output = match (count("candidatesTokenCount"), count("thoughtsTokenCount")) {
        (None, None) => response.usage.output_tokens,
        (answer, thoughts) => Some(answer.unwrap_or(0).saturating_add(thoughts.unwrap_or(0))),
    };
    let input = match (count("promptTokenCount"), count("toolUsePromptTokenCount")) {
        (None, None) => response.usage.input_tokens,
        (prompt, tools) => Some(prompt.unwrap_or(0).saturating_add(tools.unwrap_or(0))),
    };
    let usage = Usage { input_tokens: input, output_tokens: output, cost: None, billable_units: None, execution_ms: None };
    Ok(ChatResponse::new(parts, stop, usage))
}

/// How this dialect reads its errors.
const RULES: rig_chat::Rules = rig_chat::Rules {
    status: |status, _, body| error(status, body),
    event: event_error,
    unended: "Vertex AI reply without candidates",
    crate_error: Some(blocked),
};

/// The crate's blocked-prompt error (`promptFeedback.blockReason`), as a filter's refusal.
fn blocked(error: &rig_core::ProviderError) -> Option<ProviderError> {
    let rig_core::ProviderError::ProviderResponse(response) = error else { return None };
    if !error.provider_response_body().is_some_and(|b| b.starts_with("Gemini blocked the prompt")) {
        return None;
    }
    let reason = response.machine_code();
    let reason = reason.as_deref().and_then(code).unwrap_or("unknown reason");
    Some(ProviderError::new(ErrorKind::BlockedContent, format!("Google blocked the prompt ({reason}); try other words or inputs")))
}

fn event_error(err: &Value) -> ProviderError {
    error_from(None, err)
}

/// Whether a `RESOURCE_EXHAUSTED` names a daily quota, which no early retry gets past.
fn daily_quota(err: &Value) -> bool {
    let text = err.to_string().to_ascii_lowercase();
    text.contains("per_day") || text.contains("perday") || text.contains("per day")
}

/// Whether the error's details name `reason` (`google.rpc.ErrorInfo`).
fn has_reason(err: &Value, reason: &str) -> bool {
    err.get("details").and_then(Value::as_array).into_iter().flatten().any(|d| d.get("reason").and_then(Value::as_str) == Some(reason))
}

/// Google's canonical error (`{"code", "message", "status", "details"}`) in NodeCraft's terms.
fn error_from(status: Option<u16>, err: &Value) -> ProviderError {
    let said = crate::error::excerpt(err.get("message").and_then(Value::as_str).unwrap_or("request failed").as_bytes());
    let canonical = err.get("status").and_then(Value::as_str).unwrap_or_default();
    let (kind, message) = match canonical {
        "INVALID_ARGUMENT" | "FAILED_PRECONDITION" | "OUT_OF_RANGE" => {
            let lower = said.to_ascii_lowercase();
            if lower.contains("token") && (lower.contains("exceed") || lower.contains("maximum") || lower.contains("too long")) {
                return ProviderError::context_limit(&said);
            }
            (ErrorKind::UnsupportedInput, format!("Vertex AI refused the request: {said}"))
        }
        "UNAUTHENTICATED" => (ErrorKind::Auth, "Vertex AI refused the access token or API key".to_owned()),
        "PERMISSION_DENIED" if has_reason(err, "BILLING_DISABLED") => (ErrorKind::Budget, "billing is disabled on the Google Cloud project".to_owned()),
        "PERMISSION_DENIED" => (
            ErrorKind::Auth,
            "Vertex AI refused the account: check that the Vertex AI API is enabled in its project and the account has the Vertex AI User role".to_owned(),
        ),
        "NOT_FOUND" => (ErrorKind::NotFound, format!("Vertex AI has no such Gemini model here: {said}")),
        "RESOURCE_EXHAUSTED" if daily_quota(err) => (ErrorKind::Budget, format!("a daily Vertex AI quota is used up: {said}")),
        "RESOURCE_EXHAUSTED" => (ErrorKind::RateLimited, "Vertex AI's quota is used up for now; try again shortly".to_owned()),
        "UNAVAILABLE" => (ErrorKind::Busy, "Vertex AI is unavailable; try again shortly".to_owned()),
        "DEADLINE_EXCEEDED" => (ErrorKind::Timeout, "Vertex AI took too long".to_owned()),
        _ => {
            let kind = match status {
                Some(s) => ProviderError::from_status(s, b"").kind,
                None => ErrorKind::Server,
            };
            (kind, format!("Vertex AI: {said}"))
        }
    };
    ProviderError::new(kind, message)
}

/// An error status and Google's body: an `error` object, or a list of them (a stream asked for
/// without `alt=sse` answers one).
pub(super) fn error(status: u16, body: &[u8]) -> ProviderError {
    let doc = serde_json::from_slice::<Value>(body).ok();
    let err = doc.as_ref().and_then(|v| v.get("error").or_else(|| v.pointer("/0/error")));
    match err {
        Some(err) if err.is_object() => error_from(Some(status), err),
        _ => ProviderError::from_status(status, body),
    }
}

#[cfg(test)]
mod tests;

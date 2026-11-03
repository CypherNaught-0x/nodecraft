//! The OpenAI Chat Completions dialect (OpenAI, Ollama, LM Studio, vLLM, OpenRouter, LiteLLM).

use serde_json::{Value, json};

use super::sse::SseEvent;
use super::{ChatRequest, ChatResponse, MAX_REPLY_BLOCKS, Part, Role, StopReason, StreamState, base64, tool_input};
use crate::provider::Usage;
use crate::{ErrorKind, ProviderError};

/// Text-only content goes as a plain string: some OpenAI-compatible servers accept nothing else.
fn content(parts: &[&Part]) -> Value {
    if parts.iter().all(|p| matches!(p, Part::Text(_))) {
        return Value::String(parts.iter().filter_map(|p| if let Part::Text(t) = p { Some(t.as_str()) } else { None }).collect::<Vec<_>>().join("\n\n"));
    }
    let mut files = 0usize;
    Value::Array(
        parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) => Some(json!({ "type": "text", "text": t })),
                Part::Image { mime, bytes } => Some(json!({ "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{}", base64(bytes)) } })),
                // The API wants a file name with inline file data; numbered, so several differ.
                Part::Document { mime, bytes } => {
                    files += 1;
                    Some(json!({ "type": "file", "file": { "filename": format!("document-{files}.pdf"), "file_data": format!("data:{mime};base64,{}", base64(bytes)) } }))
                }
                _ => None,
            })
            .collect(),
    )
}

/// `openai` is the OpenAI API or Azure v1, which take `max_completion_tokens` (reasoning models
/// reject `max_tokens`); other servers get `max_tokens`, and only when the job sets a limit.
pub(super) fn body(req: &ChatRequest, stream: bool, openai: bool) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = &req.system {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for m in &req.messages {
        match m.role {
            Role::User => {
                // Tool results are `tool` messages, which must come straight after the calls.
                for p in &m.parts {
                    if let Part::ToolResult { id, content, is_error } = p {
                        // The API has no error flag; say it in the text.
                        let content = if *is_error { format!("Error: {content}") } else { content.clone() };
                        messages.push(json!({ "role": "tool", "tool_call_id": id, "content": content }));
                    }
                }
                let rest: Vec<&Part> = m.parts.iter().filter(|p| matches!(p, Part::Text(_) | Part::Image { .. } | Part::Document { .. })).collect();
                if !rest.is_empty() {
                    messages.push(json!({ "role": "user", "content": content(&rest) }));
                }
            }
            Role::Assistant => {
                let texts: Vec<&Part> = m.parts.iter().filter(|p| matches!(p, Part::Text(_))).collect();
                let calls: Vec<Value> = m
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::ToolUse { id, name, input } => {
                            // Arguments that weren't JSON go back as they came.
                            let arguments = match input {
                                Value::String(raw) => raw.clone(),
                                other => other.to_string(),
                            };
                            Some(json!({ "id": id, "type": "function", "function": { "name": name, "arguments": arguments } }))
                        }
                        _ => None,
                    })
                    .collect();
                let mut msg = json!({ "role": "assistant", "content": if texts.is_empty() { Value::Null } else { content(&texts) } });
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                messages.push(msg);
            }
        }
    }
    let mut body = json!({ "model": req.model, "messages": messages });
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut function = json!({ "name": t.name, "description": t.description, "parameters": t.input_schema });
                if t.strict {
                    function["strict"] = json!(true);
                }
                json!({ "type": "function", "function": function })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }
    if let Some(n) = req.max_tokens {
        body[if openai { "max_completion_tokens" } else { "max_tokens" }] = json!(n);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if stream {
        body["stream"] = json!(true);
        body["stream_options"] = json!({ "include_usage": true });
    }
    body
}

fn stop(reason: Option<&str>) -> Result<StopReason, ProviderError> {
    Ok(match reason {
        None | Some("stop") => StopReason::EndTurn,
        Some("length") => StopReason::MaxTokens,
        Some("tool_calls" | "function_call") => StopReason::ToolUse,
        Some("content_filter") => return Err(ProviderError::new(ErrorKind::BlockedContent, "the reply was withheld by the provider's content filter")),
        Some(other) => StopReason::Other(other.to_owned()),
    })
}

fn malformed(what: &str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, what.to_owned())
}

fn usage(u: Option<&Value>) -> Usage {
    Usage {
        input_tokens: u.and_then(|u| u.get("prompt_tokens")).and_then(Value::as_u64),
        output_tokens: u.and_then(|u| u.get("completion_tokens")).and_then(Value::as_u64),
        cost: None,
        billable_units: None,
        execution_ms: None,
    }
}

/// Arguments arrive as a JSON string; some local servers send the object itself.
fn arguments(v: Option<&Value>) -> Value {
    match v {
        Some(Value::String(raw)) => tool_input(raw),
        Some(Value::Null) | None => tool_input(""),
        Some(other) => other.clone(),
    }
}

pub(super) fn parse(body: &[u8]) -> Result<ChatResponse, ProviderError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| malformed(&format!("chat reply: {e}")))?;
    let choice = v.get("choices").and_then(|c| c.get(0)).ok_or_else(|| malformed("chat reply without choices"))?;
    let message = choice.get("message");
    let stop = stop(choice.get("finish_reason").and_then(Value::as_str))?;
    let mut parts = Vec::new();
    let text = message.and_then(|m| m.get("content")).and_then(Value::as_str).unwrap_or_default();
    if !text.is_empty() {
        parts.push(Part::Text(text.to_owned()));
    }
    let calls = message.and_then(|m| m.get("tool_calls")).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    if calls.len() > MAX_REPLY_BLOCKS {
        return Err(malformed("the reply has too many tool calls"));
    }
    for (i, call) in calls.iter().enumerate() {
        let function = call.get("function");
        let name =
            function.and_then(|f| f.get("name")).and_then(Value::as_str).filter(|n| !n.is_empty()).ok_or_else(|| malformed("a tool call without a name"))?;
        let id = call.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()).map_or_else(|| format!("call_{i}"), str::to_owned);
        parts.push(Part::ToolUse { id, name: name.to_owned(), input: arguments(function.and_then(|f| f.get("arguments"))) });
    }
    Ok(ChatResponse::new(parts, stop, usage(v.get("usage"))))
}

/// Handles one streamed event; returns new text, if any.
pub(super) fn stream_event(e: &SseEvent, state: &mut StreamState) -> Result<Option<String>, ProviderError> {
    if e.data.trim() == "[DONE]" {
        state.finished = true;
        return Ok(None);
    }
    let v: Value = serde_json::from_str(&e.data).map_err(|err| malformed(&format!("stream event: {err}")))?;
    if let Some(err) = v.get("error") {
        return Err(error_from(None, err));
    }
    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
        state.usage = usage(Some(u));
    }
    let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else { return Ok(None) };
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        state.stop = Some(stop(Some(reason))?);
        // Some servers end the stream without `[DONE]`; a finish reason means the reply is whole.
        state.finished = true;
    }
    let delta = choice.get("delta");
    // Tool calls stream as pieces keyed by index: the id and name first, then the arguments.
    let calls = delta.and_then(|d| d.get("tool_calls")).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    for (i, call) in calls.iter().enumerate() {
        let id = call.get("id").and_then(Value::as_str).filter(|id| !id.is_empty());
        let index = match call.get("index").and_then(Value::as_u64) {
            Some(index) => index,
            // No index (some compatibility layers): a new id starts a new call, anything else
            // continues the last one.
            None => match state.blocks.iter().next_back() {
                Some((last, block)) => match (id, block.value["id"].as_str()) {
                    (Some(new), Some(known)) if !known.is_empty() && new != known => last.saturating_add(1),
                    _ => *last,
                },
                None => i as u64,
            },
        };
        let block = state.block(index, || json!({ "id": "", "name": "" }))?;
        if let Some(id) = id {
            block.value["id"] = json!(id);
        }
        let function = call.get("function");
        if let Some(name) = function.and_then(|f| f.get("name")).and_then(Value::as_str).filter(|n| !n.is_empty()) {
            // Most servers send the name once; some repeat it on every piece, a few split it.
            let known = block.value["name"].as_str().unwrap_or_default().to_owned();
            if known != name {
                block.value["name"] = json!(format!("{known}{name}"));
            }
        }
        match function.and_then(|f| f.get("arguments")) {
            Some(Value::String(piece)) => block.args.push_str(piece),
            Some(Value::Null) | None => {}
            Some(whole) => block.args = whole.to_string(),
        }
    }
    let text = delta.and_then(|d| d.get("content")).and_then(Value::as_str).filter(|t| !t.is_empty());
    if let Some(t) = text {
        state.text.push_str(t);
    }
    Ok(text.map(str::to_owned))
}

/// The streamed reply's parts: its text, then its tool calls in index order.
pub(super) fn stream_parts(state: &mut StreamState) -> Result<Vec<Part>, ProviderError> {
    let mut parts = Vec::new();
    let text = std::mem::take(&mut state.text);
    if !text.is_empty() {
        parts.push(Part::Text(text));
    }
    for (index, block) in std::mem::take(&mut state.blocks) {
        let name = block.value["name"].as_str().filter(|n| !n.is_empty()).ok_or_else(|| malformed("a tool call without a name"))?.to_owned();
        let id = block.value["id"].as_str().filter(|id| !id.is_empty()).map_or_else(|| format!("call_{index}"), str::to_owned);
        parts.push(Part::ToolUse { id, name, input: tool_input(&block.args) });
    }
    Ok(parts)
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
mod tests {
    use super::super::tests::{Canned, request};
    use super::super::{ApiKey, Endpoint, LlmClient};
    use super::*;
    use futures::executor::block_on;

    #[test]
    fn request_shape() {
        let http = Canned::new(200, r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}"#);
        let client = LlmClient::new(Endpoint::openai(ApiKey::new("sk-1")), http.clone());
        block_on(client.chat(&request())).unwrap();
        let req = http.request();
        assert_eq!(req.url, "https://api.openai.com/v1/chat/completions");
        assert!(req.headers.contains(&("Authorization".into(), "Bearer sk-1".into())));
        let b = http.body();
        assert_eq!(b["messages"][0], json!({"role": "system", "content": "be brief"}));
        assert_eq!(b["messages"][1]["content"][1]["image_url"]["url"], "data:image/png;base64,UE5H");
        assert_eq!(b["max_completion_tokens"], 100, "OpenAI itself takes max_completion_tokens");
        assert!(b.get("max_tokens").is_none());
        let text_only = body(&ChatRequest { messages: vec![super::super::Message::user("hi")], max_tokens: None, ..request() }, false, false);
        assert_eq!(text_only["messages"][1]["content"], "hi", "text-only content is a plain string");
        assert!(text_only.get("max_tokens").is_none(), "no limit unless the job sets one");
        let other = body(&request(), false, false);
        assert_eq!(other["max_tokens"], 100);
        assert!(b.get("stream").is_none());
        let local = LlmClient::new(Endpoint::ollama(None), http.clone());
        assert_eq!(local.endpoint().base_url, "http://127.0.0.1:11434/v1");
    }

    #[test]
    fn replies_and_finish_reasons() {
        let r = parse(br#"{"choices":[{"message":{"content":"Hi"},"finish_reason":"length"}],"usage":{"prompt_tokens":5,"completion_tokens":7}}"#).unwrap();
        assert_eq!((r.text.as_str(), r.stop, r.usage.input_tokens, r.usage.output_tokens), ("Hi", StopReason::MaxTokens, Some(5), Some(7)));
        assert_eq!(parse(br#"{"choices":[{"message":{"content":null},"finish_reason":"content_filter"}]}"#).unwrap_err().kind, ErrorKind::BlockedContent);
        assert_eq!(parse(br#"{"choices":[]}"#).unwrap_err().kind, ErrorKind::Malformed);
        assert_eq!(parse(b"<html>").unwrap_err().kind, ErrorKind::Malformed);
    }

    #[test]
    fn errors() {
        let e = |s, b: &str| error(s, b.as_bytes()).kind;
        assert_eq!(
            e(429, r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#),
            ErrorKind::Budget
        );
        assert_eq!(e(429, r#"{"error":{"message":"slow down","type":"requests","code":"rate_limit_exceeded"}}"#), ErrorKind::RateLimited);
        assert_eq!(e(400, r#"{"error":{"message":"too long","code":"context_length_exceeded"}}"#), ErrorKind::UnsupportedInput);
        let long = error(400, br#"{"error":{"message":"This model's maximum context length is 8192 tokens.","code":"context_length_exceeded"}}"#);
        assert!(long.is_context_limit() && long.message.contains("8192 tokens"), "named as a context limit, keeping the provider's words: {long}");
        assert!(!error(400, br#"{"error":{"message":"bad","code":null}}"#).is_context_limit());
        assert_eq!(e(401, r#"{"error":{"message":"bad key","code":"invalid_api_key"}}"#), ErrorKind::Auth);
        assert_eq!(e(404, r#"{"error":{"message":"no such model","code":"model_not_found"}}"#), ErrorKind::NotFound);
        assert_eq!(e(503, "Service Unavailable"), ErrorKind::Busy);
        assert_eq!(
            e(401, r#"{"error":{"message":"You didn't provide an API key.","type":"invalid_request_error","code":null}}"#),
            ErrorKind::Auth,
            "status wins over type"
        );
        assert_eq!(e(400, r#"{"error":{"message":"bad","type":"invalid_request_error","code":null}}"#), ErrorKind::UnsupportedInput);
        let long = format!(r#"{{"error":{{"message":"{}"}}}}"#, "x".repeat(100_000));
        assert!(error(500, long.as_bytes()).message.len() <= 300, "messages are capped");
        assert_eq!(e(500, r#"{"error":"boom"}"#), ErrorKind::Server);
    }

    #[test]
    fn streaming_events() {
        let mut s = StreamState::default();
        let ev = |d: &str| SseEvent { event: None, data: d.into() };
        assert_eq!(stream_event(&ev(r#"{"choices":[{"delta":{"role":"assistant","content":""}}]}"#), &mut s).unwrap(), None);
        assert_eq!(stream_event(&ev(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#), &mut s).unwrap().as_deref(), Some("Hel"));
        assert_eq!(stream_event(&ev(r#"{"choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}]}"#), &mut s).unwrap().as_deref(), Some("lo"));
        stream_event(&ev(r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#), &mut s).unwrap();
        stream_event(&ev("[DONE]"), &mut s).unwrap();
        assert_eq!((s.text.as_str(), s.finished, s.usage.output_tokens), ("Hello", true, Some(2)));
        assert_eq!(stream_event(&ev(r#"{"error":{"message":"overloaded","code":"rate_limit_exceeded"}}"#), &mut s).unwrap_err().kind, ErrorKind::RateLimited);
        assert!(stream_event(&ev("{"), &mut s).is_err());
    }
}

//! The Anthropic Messages dialect (`POST /v1/messages`, `x-api-key`, `anthropic-version`).
//!
//! Current Claude models reject sampling parameters and always think adaptively, so
//! `temperature` is only sent when the job sets one and `thinking` is never sent. The default
//! catalog model is `claude-opus-5-5`.

use serde_json::{Value, json};

use super::sse::SseEvent;
use super::{ChatRequest, ChatResponse, Dialect, MAX_REPLY_BLOCKS, Part, Role, StopReason, StreamState, base64, tool_input};
use crate::provider::Usage;
use crate::{ErrorKind, ProviderError};

pub(super) const VERSION: &str = "2023-06-01";

/// Claude models that reject sampling parameters (`temperature`, `top_p`): the 5-series and later.
pub(super) fn rejects_sampling(model: &str) -> bool {
    let m = model.rsplit('/').next().unwrap_or(model);
    ["claude-opus-5", "claude-opus-4-7", "claude-opus-4-8", "claude-sonnet-5", "claude-haiku-5", "claude-fable", "claude-mythos"]
        .iter()
        .any(|p| m.starts_with(p))
}
/// The default model for Anthropic text nodes.
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

pub(super) fn body(req: &ChatRequest, stream: bool) -> Value {
    let block = |p: &Part| match p {
        Part::Text(t) => json!({ "type": "text", "text": t }),
        Part::Image { mime, bytes } => json!({ "type": "image", "source": { "type": "base64", "media_type": mime, "data": base64(bytes) } }),
        Part::Document { mime, bytes } => json!({ "type": "document", "source": { "type": "base64", "media_type": mime, "data": base64(bytes) } }),
        // Claude's own calls always carry an object; arguments from another server that weren't
        // JSON can't be sent as tool input.
        Part::ToolUse { id, name, input } => {
            json!({ "type": "tool_use", "id": id, "name": name, "input": if input.is_object() { input.clone() } else { json!({}) } })
        }
        Part::ToolResult { id, content, is_error } => {
            let mut b = json!({ "type": "tool_result", "tool_use_id": id, "content": content });
            if *is_error {
                b["is_error"] = json!(true);
            }
            b
        }
        Part::Opaque { block, .. } => block.clone(),
    };
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            // The user's order is kept (captions like "Image 1:" sit next to their image), except
            // that tool results must come first.
            let results = m.parts.iter().filter(|p| matches!(p, Part::ToolResult { .. }));
            let others = m.parts.iter().filter(|p| !matches!(p, Part::ToolResult { .. }));
            let content: Vec<Value> = results.chain(others).map(block).collect();
            json!({ "role": role, "content": content })
        })
        .collect();
    // The Messages API requires max_tokens.
    let max_tokens = req.max_tokens.unwrap_or(super::DEFAULT_MAX_TOKENS);
    let mut body = json!({ "model": req.model, "max_tokens": max_tokens, "messages": messages });
    if let Some(system) = &req.system {
        body["system"] = json!(system);
    }
    if !req.tools.is_empty() {
        // No `tool_choice`: the default is `auto`, and current models reject forcing a call.
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
        body["tools"] = Value::Array(tools);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if stream {
        body["stream"] = json!(true);
    }
    body
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
/// (thinking, redacted thinking, server tool blocks) is kept as it came. `args` is a streamed
/// block's input text (tool calls, and server tool calls such as web search).
fn part(mut block: Value, args: Option<&str>) -> Result<Option<Part>, ProviderError> {
    let streamed = args.filter(|raw| !raw.trim().is_empty()).map(tool_input);
    Ok(match block.get("type").and_then(Value::as_str) {
        Some("text") => block.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()).map(|t| Part::Text(t.to_owned())),
        Some("tool_use") => {
            let text = |k: &str| block.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_owned);
            let (Some(id), Some(name)) = (text("id"), text("name")) else { return Err(malformed("a tool call without an id or name")) };
            let input = streamed.unwrap_or_else(|| block.get("input").cloned().filter(Value::is_object).unwrap_or_else(|| json!({})));
            Some(Part::ToolUse { id, name, input })
        }
        _ => {
            if let (Some(input), Some(obj)) = (streamed, block.as_object_mut()) {
                obj.insert("input".into(), input);
            }
            Some(Part::Opaque { dialect: Dialect::Anthropic, block })
        }
    })
}

pub(super) fn parse(body: &[u8]) -> Result<ChatResponse, ProviderError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| malformed(&format!("messages reply: {e}")))?;
    let content = v.get("content").and_then(Value::as_array).ok_or_else(|| malformed("messages reply without content"))?;
    // Stop reason first: a refusal must not surface partial text as a result.
    let stop = stop(v.get("stop_reason").and_then(Value::as_str))?;
    if content.len() > MAX_REPLY_BLOCKS {
        return Err(malformed("the reply has too many parts"));
    }
    let mut parts = Vec::new();
    for b in content {
        parts.extend(part(b.clone(), None)?);
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

/// Appends `piece` to the string field `key` of a streamed block.
fn append(block: &mut Value, key: &str, piece: &str) {
    if let Some(obj) = block.as_object_mut() {
        // In place: copying the string on every delta would make long replies quadratic.
        match obj.get_mut(key) {
            Some(Value::String(s)) => s.push_str(piece),
            _ => {
                obj.insert(key.to_owned(), Value::String(piece.to_owned()));
            }
        }
    }
}

pub(super) fn stream_event(e: &SseEvent, state: &mut StreamState) -> Result<Option<String>, ProviderError> {
    let v: Value = serde_json::from_str(&e.data).map_err(|err| malformed(&format!("stream event: {err}")))?;
    let kind = v.get("type").and_then(Value::as_str).or(e.event.as_deref()).unwrap_or_default();
    let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
    match kind {
        "message_start" => {
            state.usage.input_tokens = v.get("message").and_then(|m| m.get("usage")).and_then(|u| u.get("input_tokens")).and_then(Value::as_u64);
            Ok(None)
        }
        "content_block_start" => {
            let start = v.get("content_block").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
            state.block(index, || start)?;
            Ok(None)
        }
        "content_block_delta" => {
            let Some(delta) = v.get("delta") else { return Ok(None) };
            let piece = |key: &str| delta.get(key).and_then(Value::as_str).unwrap_or_default();
            match delta.get("type").and_then(Value::as_str).unwrap_or_default() {
                "text_delta" => {
                    let text = piece("text");
                    append(&mut state.block(index, || json!({ "type": "text", "text": "" }))?.value, "text", text);
                    state.text.push_str(text);
                    Ok((!text.is_empty()).then(|| text.to_owned()))
                }
                "input_json_delta" => {
                    state.block(index, || json!({ "type": "tool_use" }))?.args.push_str(piece("partial_json"));
                    Ok(None)
                }
                "thinking_delta" => {
                    append(&mut state.block(index, || json!({ "type": "thinking", "thinking": "" }))?.value, "thinking", piece("thinking"));
                    Ok(None)
                }
                "signature_delta" => {
                    let block = state.block(index, || json!({ "type": "thinking", "thinking": "" }))?;
                    if let Some(obj) = block.value.as_object_mut() {
                        obj.insert("signature".into(), json!(piece("signature")));
                    }
                    Ok(None)
                }
                _ => Ok(None), // citations, future deltas
            }
        }
        "message_delta" => {
            state.stop = Some(stop(v.get("delta").and_then(|d| d.get("stop_reason")).and_then(Value::as_str))?);
            if let Some(n) = v.get("usage").and_then(|u| u.get("output_tokens")).and_then(Value::as_u64) {
                state.usage.output_tokens = Some(n);
            }
            Ok(None)
        }
        "message_stop" => {
            state.finished = true;
            Ok(None)
        }
        "error" => Err(error_from(v.get("error").unwrap_or(&Value::Null), None)),
        _ => Ok(None), // ping, content_block_stop, future events
    }
}

/// The streamed reply's parts, in block order.
pub(super) fn stream_parts(state: &mut StreamState) -> Result<Vec<Part>, ProviderError> {
    let mut parts = Vec::new();
    for b in std::mem::take(&mut state.blocks).into_values() {
        parts.extend(part(b.value, Some(&b.args))?);
    }
    Ok(parts)
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
mod tests {
    use super::super::tests::{Canned, request};
    use super::super::{ApiKey, Endpoint, LlmClient};
    use super::*;
    use futures::executor::block_on;

    #[test]
    fn request_shape() {
        let http = Canned::new(200, r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#);
        let client = LlmClient::new(Endpoint::anthropic(ApiKey::new("sk-ant")), http.clone());
        let mut r = request();
        r.model = DEFAULT_MODEL.into();
        r.temperature = None;
        block_on(client.chat(&r)).unwrap();
        let req = http.request();
        assert_eq!(req.url, "https://api.anthropic.com/v1/messages");
        assert!(req.headers.contains(&("x-api-key".into(), "sk-ant".into())));
        assert!(req.headers.contains(&("anthropic-version".into(), VERSION.into())));
        assert!(!req.headers.iter().any(|(n, _)| n == "Authorization"));
        let b = http.body();
        assert_eq!(b["model"], "claude-opus-5-5");
        assert_eq!(b["system"], "be brief");
        assert_eq!(b["messages"][0]["content"][0], json!({"type":"text","text":"hi"}), "the user's order is kept");
        assert_eq!(b["messages"][0]["content"][1], json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"UE5H"}}));
        assert_eq!(b["max_tokens"], 100);
        let unset = body(&ChatRequest { max_tokens: None, ..request() }, false);
        assert_eq!(unset["max_tokens"], super::super::DEFAULT_MAX_TOKENS, "required by the API");
        assert!(rejects_sampling("claude-opus-5-5") && rejects_sampling("anthropic/claude-sonnet-5-5") && !rejects_sampling("claude-3-5-haiku"));
        assert!(b.get("temperature").is_none() && b.get("thinking").is_none());
    }

    #[test]
    fn replies() {
        let r = parse(br#"{"content":[{"type":"thinking","thinking":""},{"type":"text","text":"Hello "},{"type":"text","text":"there"}],"stop_reason":"max_tokens","usage":{"input_tokens":4,"output_tokens":9}}"#).unwrap();
        assert_eq!((r.text.as_str(), r.stop, r.usage.output_tokens), ("Hello there", StopReason::MaxTokens, Some(9)));
        let refused = parse(br#"{"content":[{"type":"text","text":"partial"}],"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"}}"#)
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::BlockedContent);
        assert_eq!(parse(br#"{"type":"message"}"#).unwrap_err().kind, ErrorKind::Malformed);
    }

    #[test]
    fn errors() {
        let e = |s, b: &str| error(s, b.as_bytes()).kind;
        assert_eq!(e(529, r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#), ErrorKind::Busy);
        assert_eq!(e(529, ""), ErrorKind::Busy);
        assert_eq!(e(429, r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow"}}"#), ErrorKind::RateLimited);
        assert_eq!(e(402, r#"{"type":"error","error":{"type":"billing_error","message":"pay"}}"#), ErrorKind::Budget);
        assert_eq!(e(401, r#"{"type":"error","error":{"type":"authentication_error","message":"bad key"}}"#), ErrorKind::Auth);
        assert_eq!(e(404, r#"{"type":"error","error":{"type":"not_found_error","message":"model"}}"#), ErrorKind::NotFound);
        assert_eq!(e(400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#), ErrorKind::UnsupportedInput);
        assert_eq!(e(500, r#"{"type":"error","error":{"type":"api_error","message":"oops"}}"#), ErrorKind::Server);
        assert!(ErrorKind::Busy.is_transient() && !ErrorKind::Budget.is_transient());
    }

    #[test]
    fn streaming_events() {
        let mut s = StreamState::default();
        let ev = |name: &str, d: &str| SseEvent { event: Some(name.into()), data: d.into() };
        let events = [
            ev("message_start", r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":12,"output_tokens":1}}}"#),
            ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
            ev("ping", r#"{"type":"ping"}"#),
            ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#),
            ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"…"}}"#),
            ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#),
            ev("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ev("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}"#),
            ev("message_stop", r#"{"type":"message_stop"}"#),
        ];
        let mut pieces = Vec::new();
        for e in &events {
            if let Some(t) = stream_event(e, &mut s).unwrap() {
                pieces.push(t);
            }
        }
        assert_eq!(pieces, ["Hel", "lo"]);
        assert_eq!((s.text.as_str(), s.finished, s.usage.input_tokens, s.usage.output_tokens), ("Hello", true, Some(12), Some(12)));
        let err = stream_event(&ev("error", r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#), &mut s).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Busy);
        let refusal = stream_event(&ev("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#), &mut s).unwrap_err();
        assert_eq!(refusal.kind, ErrorKind::BlockedContent);
    }
}

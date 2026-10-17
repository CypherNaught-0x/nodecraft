//! The OpenAI Chat Completions dialect (OpenAI, Ollama, LM Studio, vLLM, OpenRouter, LiteLLM).

use serde_json::{Value, json};

use super::sse::SseEvent;
use super::{ChatRequest, ChatResponse, Part, Role, StopReason, StreamState, base64};
use crate::{ErrorKind, ProviderError};

/// `openai` is the OpenAI API itself, which takes `max_completion_tokens` (its reasoning models
/// reject `max_tokens`); other servers get `max_tokens`, and only when the job sets a limit.
pub(super) fn body(req: &ChatRequest, stream: bool, openai: bool) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = &req.system {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for m in &req.messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        // Text-only messages go as a plain string: some OpenAI-compatible servers accept nothing else.
        let text_only = m.parts.iter().all(|p| matches!(p, Part::Text(_)));
        let content = if text_only {
            Value::String(m.parts.iter().filter_map(|p| if let Part::Text(t) = p { Some(t.as_str()) } else { None }).collect::<Vec<_>>().join("\n\n"))
        } else {
            Value::Array(
                m.parts
                    .iter()
                    .map(|p| match p {
                        Part::Text(t) => json!({ "type": "text", "text": t }),
                        Part::Image { mime, bytes } => json!({ "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{}", base64(bytes)) } }),
                    })
                    .collect(),
            )
        };
        messages.push(json!({ "role": role, "content": content }));
    }
    let mut body = json!({ "model": req.model, "messages": messages });
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
        Some("content_filter") => return Err(ProviderError::new(ErrorKind::BlockedContent, "the reply was withheld by the provider's content filter")),
        Some(other) => StopReason::Other(other.to_owned()),
    })
}

fn malformed(what: &str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, what.to_owned())
}

pub(super) fn parse(body: &[u8]) -> Result<ChatResponse, ProviderError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| malformed(&format!("chat reply: {e}")))?;
    let choice = v.get("choices").and_then(|c| c.get(0)).ok_or_else(|| malformed("chat reply without choices"))?;
    let text = choice.get("message").and_then(|m| m.get("content")).and_then(Value::as_str).unwrap_or_default().to_owned();
    let stop = stop(choice.get("finish_reason").and_then(Value::as_str))?;
    let usage = v.get("usage");
    Ok(ChatResponse {
        text,
        stop,
        input_tokens: usage.and_then(|u| u.get("prompt_tokens")).and_then(Value::as_u64),
        output_tokens: usage.and_then(|u| u.get("completion_tokens")).and_then(Value::as_u64),
        reported_cost_usd: None,
    })
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
    if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
        state.input_tokens = usage.get("prompt_tokens").and_then(Value::as_u64);
        state.output_tokens = usage.get("completion_tokens").and_then(Value::as_u64);
    }
    let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else { return Ok(None) };
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        state.stop = Some(stop(Some(reason))?);
        // Some servers end the stream without `[DONE]`; a finish reason means the reply is whole.
        state.finished = true;
    }
    let delta = choice.get("delta").and_then(|d| d.get("content")).and_then(Value::as_str).filter(|t| !t.is_empty());
    if let Some(t) = delta {
        state.text.push_str(t);
    }
    Ok(delta.map(str::to_owned))
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
        assert_eq!((r.text.as_str(), r.stop, r.input_tokens, r.output_tokens), ("Hi", StopReason::MaxTokens, Some(5), Some(7)));
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
        assert_eq!((s.text.as_str(), s.finished, s.output_tokens), ("Hello", true, Some(2)));
        assert_eq!(stream_event(&ev(r#"{"error":{"message":"overloaded","code":"rate_limit_exceeded"}}"#), &mut s).unwrap_err().kind, ErrorKind::RateLimited);
        assert!(stream_event(&ev("{"), &mut s).is_err());
    }
}

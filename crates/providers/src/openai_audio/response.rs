use super::*;
use crate::{HttpResponse, Output, Usage};
use nodecraft_types::{Currency, Money};
use serde::Deserialize;

/// Only parsed, bounded refusals can clear the paid-send evidence. Unknown bodies, redirects,
/// 5xx and transport failures remain uncertain even if they mention one of these error codes.
/// https://developers.openai.com/api/docs/guides/error-codes (checked 2026-10-24).
pub(super) fn refusal_kind(reply: &HttpResponse) -> Option<ErrorKind> {
    if reply.body.len() > TRANSCRIPT_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&reply.body).ok()?;
    let error = value.get("error")?.as_object()?;
    let code = error.get("code").and_then(serde_json::Value::as_str);
    let kind = error.get("type").and_then(serde_json::Value::as_str);
    match reply.status {
        401 | 403 if code.is_some_and(|s| !s.is_empty()) || kind.is_some_and(|s| !s.is_empty()) => Some(ErrorKind::Auth),
        400 if kind == Some("invalid_request_error") && !matches!(code, Some("content_policy_violation" | "content_filter")) => {
            Some(ErrorKind::UnsupportedInput)
        }
        429 if kind == Some("insufficient_quota")
            || matches!(
                code,
                Some(
                    "insufficient_quota"
                        | "credit_balance_exhausted"
                        | "organization_spend_limit_exceeded"
                        | "project_spend_limit_exceeded"
                        | "organization_usage_limit_exceeded"
                )
            ) =>
        {
            Some(ErrorKind::Budget)
        }
        429 if matches!(code, Some("rate_limit_exceeded" | "slow_down")) || kind == Some("rate_limit_error") => Some(ErrorKind::RateLimited),
        _ => None,
    }
}

pub(super) fn parse(reply: HttpResponse, model: &str) -> Result<Completed, ProviderError> {
    let cap = if model == SPEECH { OUTPUT_BYTES } else { TRANSCRIPT_BYTES };
    if reply.body.len() > cap {
        return Err(malformed("OpenAI audio reply exceeds its byte limit; the request may have been billed"));
    }
    if !reply.is_success() {
        let value: serde_json::Value = serde_json::from_slice(&reply.body).unwrap_or_default();
        let code = value.get("error").and_then(|e| e.get("code")).and_then(serde_json::Value::as_str);
        let kind = refusal_kind(&reply).unwrap_or_else(|| match (reply.status, code) {
            (401 | 403, _) => ErrorKind::Auth,
            (_, Some("content_policy_violation" | "content_filter")) => ErrorKind::BlockedContent,
            (429, Some("insufficient_quota")) => ErrorKind::Budget,
            _ => ProviderError::from_status(reply.status, b"").kind,
        });
        let mut error = ProviderError::new(kind, format!("OpenAI audio refused or failed (HTTP {}); check the model, input and account", reply.status));
        error.retry_after_ms = reply
            .header("retry-after")
            .filter(|s| s.len() <= 16)
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|s| s.checked_mul(1000))
            .filter(|ms| *ms <= 3_600_000);
        return Err(error);
    }
    if model == SPEECH {
        if nodecraft_media::sniff(&reply.body).map(|s| s.mime) != Some("audio/mpeg") {
            return Err(malformed("OpenAI speech did not return MP3 audio"));
        }
        return Ok(Completed { outputs: vec![Output::Bytes { mime: "audio/mpeg".into(), bytes: Arc::from(reply.body) }], usage: Usage::default() });
    }
    #[derive(Deserialize)]
    struct Transcript {
        text: String,
        usage: Option<DurationUsage>,
    }
    #[derive(Deserialize)]
    struct DurationUsage {
        #[serde(rename = "type")]
        kind: String,
        seconds: Option<f64>,
    }
    let result: Transcript = serde_json::from_slice(&reply.body).map_err(|_| malformed("OpenAI transcription returned an invalid transcript"))?;
    // Current GPT-Transcribe is $0.0045/minute = 75 micro-USD/second (2026-10-24).
    // Missing/unknown usage stays unknown. A hostile duration never wraps into a small charge.
    let cost = result.usage.filter(|u| u.kind == "duration").and_then(|u| u.seconds).and_then(|seconds| {
        let micros = (seconds * 75.0).ceil();
        (seconds.is_finite() && seconds >= 0.0 && micros.is_finite() && micros >= 0.0 && micros < u64::MAX as f64)
            .then(|| Money::new(Currency::USD, micros as u64))
    });
    Ok(Completed { outputs: vec![Output::Text(result.text)], usage: Usage { cost, ..Usage::default() } })
}

use super::*;
use crate::{HttpResponse, Output, Usage};
use serde::Deserialize;

#[derive(Deserialize)]
struct Reply {
    results: Results,
}
#[derive(Deserialize)]
struct Results {
    channels: Vec<Channel>,
}
#[derive(Deserialize)]
struct Channel {
    alternatives: Vec<Alternative>,
}
#[derive(Deserialize)]
struct Alternative {
    transcript: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Refusal {
    err_code: String,
    err_msg: String,
    #[serde(default)]
    request_id: Option<String>,
}

/// Only documented, typed refusals clear send evidence; HTTP status alone is insufficient.
/// https://developers.deepgram.com/docs/errors (checked 2026-10-26).
pub(super) fn refusal_kind(reply: &HttpResponse) -> Option<ErrorKind> {
    if !matches!(reply.status, 400 | 401 | 402 | 403 | 422 | 429) || reply.body.len() > MAX_RESPONSE_BYTES {
        return None;
    }
    let refusal: Refusal = serde_json::from_slice(&reply.body).ok()?;
    if refusal.err_msg.trim().is_empty() || refusal.request_id.as_ref().is_some_and(|id| id.is_empty() || id.len() > 128) {
        return None;
    }
    match (reply.status, refusal.err_code.as_str()) {
        (401 | 403, "INVALID_AUTH" | "INSUFFICIENT_PERMISSIONS") => Some(ErrorKind::Auth),
        (402, "ASR_PAYMENT_REQUIRED") => Some(ErrorKind::Budget),
        (400, "Bad Request" | "INVALID_QUERY_PARAMETER" | "INVALID_AUDIO") | (422, "ASR_UNPROCESSABLE_ENTITY" | "INVALID_AUDIO") => {
            Some(ErrorKind::UnsupportedInput)
        }
        (429, "TOO_MANY_REQUESTS" | "Too Many Requests") => Some(ErrorKind::RateLimited),
        _ => None,
    }
}

pub(super) fn parse(reply: HttpResponse, key: &Secret) -> Result<Completed, ProviderError> {
    if reply.body.len() > MAX_RESPONSE_BYTES {
        return Err(malformed());
    }
    if reply.status != 200 {
        // Never publish provider text, which may contain credentials or private transcript data.
        let kind = if (200..400).contains(&reply.status) { ErrorKind::Malformed } else { ProviderError::from_status(reply.status, b"").kind };
        let mut error = ProviderError::new(kind, format!("Deepgram transcription failed (HTTP {})", reply.status));
        error.retry_after_ms = reply
            .header("retry-after")
            .filter(|v| v.len() <= 16)
            .and_then(|v| v.trim().parse::<u64>().ok())
            .and_then(|n| n.checked_mul(1000))
            .filter(|n| *n <= 3_600_000);
        return Err(error);
    }
    if reply.body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(malformed());
    }
    let parsed: Reply = serde_json::from_slice(&reply.body).map_err(|_| malformed())?;
    let [channel] = parsed.results.channels.as_slice() else { return Err(malformed()) };
    let [alternative] = channel.alternatives.as_slice() else { return Err(malformed()) };
    if alternative.transcript.len() > MAX_TRANSCRIPT_BYTES || alternative.transcript.contains(key.expose()) {
        return Err(malformed());
    }
    // Silence legitimately yields empty text. The API does not return a monetary receipt.
    Ok(Completed { outputs: vec![Output::Text(alternative.transcript.clone())], usage: Usage::default() })
}

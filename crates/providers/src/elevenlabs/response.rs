//! ElevenLabs replies: the MP3 on success, the `detail` error body otherwise.
use super::*;
use crate::{HttpResponse, Output, Usage};
use nodecraft_types::BillableUnits;
use serde_json::Value;

/// Error statuses ElevenLabs documents for a request it turned away before generating.
const REFUSALS: &[&str] = &["quota_exceeded", "voice_not_found", "too_many_concurrent_requests", "system_busy", "max_character_limit_exceeded"];

/// `detail.status` and `detail.message` of an error body, when it has that shape.
fn detail(body: &[u8]) -> Option<(String, String)> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let d = v.get("detail")?;
    let status = d.get("status")?.as_str()?.to_owned();
    let message = d.get("message").and_then(Value::as_str).unwrap_or_default().to_owned();
    Some((status, message))
}
/// The messages of a 422 validation body: a `detail` list of `{loc, msg, type}`.
fn validation(body: &[u8]) -> Option<Vec<String>> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let list = v.get("detail")?.as_array()?;
    if list.is_empty() {
        return None;
    }
    list.iter().take(8).map(|e| e.get("msg").and_then(Value::as_str).map(str::to_owned)).collect()
}

/// Whether the reply is a documented refusal of the request, sent before anything was generated
/// or billed: a rejected key, a validation failure, or a quota, voice or capacity refusal.
/// Anything else, a 5xx or an unparsed body included, stays possibly billed.
pub(super) fn refused(r: &HttpResponse) -> bool {
    match r.status {
        401 => detail(&r.body).is_some_and(|(s, _)| s == "invalid_api_key" || s == "quota_exceeded"),
        422 => validation(&r.body).is_some(),
        400 | 429 => detail(&r.body).is_some_and(|(s, _)| REFUSALS.contains(&s.as_str())),
        _ => false,
    }
}

/// ElevenLabs's own words, cleaned and shortened, unless they echo the key.
fn said(text: &str, key: &Secret) -> String {
    if text.contains(key.expose()) {
        return "(message withheld)".into();
    }
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(300).collect::<String>().trim().to_owned()
}
fn kind(status: &str, http: u16) -> ErrorKind {
    match status {
        "invalid_api_key" => ErrorKind::Auth,
        "quota_exceeded" => ErrorKind::Budget,
        "voice_not_found" | "max_character_limit_exceeded" => ErrorKind::UnsupportedInput,
        "too_many_concurrent_requests" => ErrorKind::RateLimited,
        "system_busy" => ErrorKind::Busy,
        _ => ProviderError::from_status(http, b"").kind,
    }
}
fn failure(r: &HttpResponse, key: &Secret) -> ProviderError {
    let secret = key.expose().as_bytes();
    let echoed = !secret.is_empty() && r.body.windows(secret.len()).any(|w| w == secret);
    let (kind, message) = if echoed {
        (ProviderError::from_status(r.status, b"").kind, format!("ElevenLabs failed (HTTP {}): (message withheld)", r.status))
    } else if let Some((status, message)) = detail(&r.body) {
        let status = said(&status, key);
        (kind(&status, r.status), format!("ElevenLabs refused the request (HTTP {}, {status}): {}", r.status, said(&message, key)))
    } else if let Some(messages) = validation(&r.body) {
        (ErrorKind::UnsupportedInput, format!("ElevenLabs refused the request (HTTP {}): {}", r.status, said(&messages.join("; "), key)))
    } else {
        let excerpt = crate::error::excerpt(&r.body);
        (ProviderError::from_status(r.status, b"").kind, format!("ElevenLabs failed (HTTP {}): {}", r.status, said(&excerpt, key)))
    };
    let mut error = ProviderError::new(kind, message);
    error.retry_after_ms = r
        .header("retry-after")
        .filter(|v| v.len() <= 16)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .and_then(|s| s.checked_mul(1000))
        .filter(|ms| *ms <= 3_600_000);
    error
}

/// The MP3 the request made, with the characters it billed when that's how it's priced.
pub(super) fn parse(r: HttpResponse, l: Limits, key: &Secret, characters: Option<u64>) -> Result<Completed, ProviderError> {
    if !r.is_success() {
        return Err(failure(&r, key));
    }
    if r.body.len() > l.output_bytes {
        return Err(malformed("ElevenLabs audio exceeds the size limit"));
    }
    if r.body.is_empty() || nodecraft_media::sniff(&r.body).map(|s| s.mime) != Some("audio/mpeg") {
        return Err(malformed("ElevenLabs reply isn't MP3 audio"));
    }
    let usage = Usage { billable_units: characters.and_then(|n| n.checked_mul(1_000_000)).and_then(BillableUnits::from_millionths), ..Usage::default() };
    Ok(Completed { outputs: vec![Output::Bytes { mime: "audio/mpeg".into(), bytes: Arc::from(r.body) }], usage })
}

/// One page of the voice list: its voices, the entries that didn't read as one, and the token of
/// the next page when there is one.
pub(super) struct VoicePage {
    pub(super) voices: Vec<Voice>,
    pub(super) skipped: usize,
    pub(super) next: Option<String>,
}

/// The most voices one page may hold: the first can carry the premade ones beyond its size.
const MAX_PAGE_VOICES: usize = 1000;
/// The longest page token passed back.
const MAX_TOKEN_BYTES: usize = 1024;
/// The longest voice name kept.
const MAX_NAME_CHARS: usize = 200;

/// Reads a `GET /v2/voices` page. A voice whose id isn't letters and digits, or that has no name
/// or echoes the key, is skipped rather than listed: the run would refuse its id anyway.
pub(super) fn voice_page(r: HttpResponse, key: &Secret) -> Result<VoicePage, ProviderError> {
    if !r.is_success() {
        return Err(failure(&r, key));
    }
    let v: Value = serde_json::from_slice(&r.body).map_err(|_| malformed("ElevenLabs's voice list isn't JSON NodeCraft reads"))?;
    let items = v.get("voices").and_then(Value::as_array).ok_or_else(|| malformed("ElevenLabs's voice list has no voices"))?;
    if items.len() > MAX_PAGE_VOICES {
        return Err(malformed("an ElevenLabs voice list page is longer than NodeCraft reads"));
    }
    let leaks = |s: &str| !key.expose().is_empty() && s.contains(key.expose());
    let mut voices = Vec::new();
    let mut skipped = 0usize;
    for item in items {
        let id = item.get("voice_id").and_then(Value::as_str).filter(|id| super::request::voice_id(id) && !leaks(id));
        let name =
            item.get("name").and_then(Value::as_str).map(|n| n.chars().map(|c| if c.is_control() { ' ' } else { c }).take(MAX_NAME_CHARS).collect::<String>());
        let name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty() && !leaks(n));
        match (id, name) {
            (Some(id), Some(name)) => voices.push(Voice { id: id.to_owned(), name }),
            _ => skipped = skipped.saturating_add(1),
        }
    }
    let more = v.get("has_more").and_then(Value::as_bool).unwrap_or(false);
    let next = match v.get("next_page_token").and_then(Value::as_str).filter(|t| !t.is_empty()) {
        Some(token) if more && token.len() <= MAX_TOKEN_BYTES => Some(token.to_owned()),
        Some(_) if more => return Err(malformed("ElevenLabs's next voice page token is longer than NodeCraft sends")),
        _ if more => return Err(malformed("ElevenLabs says there are more voices but gives no next page")),
        _ => None,
    };
    Ok(VoicePage { voices, skipped, next })
}

/// `text` percent-encoded for a query value: everything but unreserved characters escaped.
pub(super) fn query_escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

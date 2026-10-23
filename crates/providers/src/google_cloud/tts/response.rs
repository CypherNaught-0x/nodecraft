//! Cloud Text-to-Speech replies: the MP3 inline on success, Google's error body otherwise.
use super::*;
use crate::{HttpResponse, Output, Usage};
use base64::Engine as _;
use nodecraft_types::BillableUnits;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Synthesized {
    audio_content: String,
}
#[derive(Deserialize)]
struct Status {
    status: Option<String>,
    message: Option<String>,
}
#[derive(Deserialize)]
struct Failure {
    error: Status,
}

/// Google's `error.status` and `error.message`, when the body has that shape.
fn status(body: &[u8]) -> Option<(String, String)> {
    let failure: Failure = serde_json::from_slice(body).ok()?;
    Some((failure.error.status?, failure.error.message.unwrap_or_default()))
}

/// Whether the reply is one of Google's canonical refusals, which a synchronous request gets
/// before anything is synthesized: a malformed request, a rejected token or permission, an
/// unknown voice or a quota. Anything else, a 5xx or an unparsed body included, stays possibly
/// billed.
pub(super) fn refused(r: &HttpResponse) -> bool {
    let expected = match r.status {
        400 => "INVALID_ARGUMENT",
        401 => "UNAUTHENTICATED",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        429 => "RESOURCE_EXHAUSTED",
        _ => return false,
    };
    status(&r.body).is_some_and(|(s, _)| s == expected)
}

/// Google's own words, cleaned and shortened, unless they echo the token.
fn said(text: &str, bearer: &str) -> String {
    if !bearer.is_empty() && text.contains(bearer) {
        return "(message withheld)".into();
    }
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(300).collect::<String>().trim().to_owned()
}

fn failure(r: &HttpResponse, project: &str, bearer: &str) -> ProviderError {
    let kind = match r.status {
        300..=399 => ErrorKind::Malformed,
        400 | 413 | 415 | 422 => ErrorKind::UnsupportedInput,
        401 | 403 => ErrorKind::Auth,
        404 | 410 => ErrorKind::NotFound,
        408 | 504 => ErrorKind::Timeout,
        429 => ErrorKind::RateLimited,
        503 => ErrorKind::Busy,
        _ => ErrorKind::Server,
    };
    let message = match r.status {
        // A bad voice, language or SSML: Google's explanation is what the person needs.
        400 => match status(&r.body) {
            Some((_, message)) if !message.is_empty() => format!("Google TTS refused the request: {}", said(&message, bearer)),
            _ => "Google TTS refused the request".into(),
        },
        401 => "Google Cloud refused the service account's token".into(),
        403 => format!(
            "Google Cloud refused the service account: check that the Cloud Text-to-Speech API is enabled in project {project} (Gemini voices also need the Vertex AI User role)"
        ),
        404 => "Google TTS has no such voice or model".into(),
        429 => "Google Cloud's Text-to-Speech quota is used up for now; try again shortly".into(),
        _ => "the Google TTS request failed; it may have been billed".into(),
    };
    let mut error = ProviderError::new(kind, format!("{message} (HTTP {})", r.status));
    error.retry_after_ms = r
        .header("retry-after")
        .filter(|s| s.len() <= 16)
        .and_then(|s| s.trim().parse::<u64>().ok())
        .and_then(|n| n.checked_mul(1000))
        .filter(|n| *n <= 3_600_000);
    error
}

/// The MP3 the request made, with the characters it billed when that's how it's priced.
pub(super) fn parse(r: &HttpResponse, l: Limits, project: &str, bearer: &str, characters: Option<u64>) -> Result<Completed, ProviderError> {
    if r.body.len() > l.response_bytes {
        return Err(malformed("the Google TTS reply exceeds its byte limit; the speech may have been billed"));
    }
    if !r.is_success() {
        return Err(failure(r, project, bearer));
    }
    let reply: Synthesized = serde_json::from_slice(&r.body).map_err(|_| malformed("the Google TTS reply can't be read; the speech may have been billed"))?;
    // Base64 is four characters for every three bytes.
    if reply.audio_content.len() / 4 * 3 > l.output_bytes {
        return Err(malformed("Google TTS audio exceeds the size limit"));
    }
    let bytes =
        base64::engine::general_purpose::STANDARD.decode(reply.audio_content.as_bytes()).map_err(|_| malformed("Google TTS audio isn't valid base64"))?;
    if bytes.is_empty() || nodecraft_media::sniff(&bytes).map(|s| s.mime) != Some(MP3) {
        return Err(malformed("Google TTS reply isn't MP3 audio"));
    }
    let usage = Usage { billable_units: characters.and_then(|n| n.checked_mul(1_000_000)).and_then(BillableUnits::from_millionths), ..Usage::default() };
    Ok(Completed { outputs: vec![Output::Bytes { mime: MP3.into(), bytes: Arc::from(bytes) }], usage })
}

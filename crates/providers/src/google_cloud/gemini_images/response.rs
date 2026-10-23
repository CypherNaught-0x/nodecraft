//! `generateContent` replies: the images inline in the first candidate on success, a block
//! reason or a finish reason when there are none, Google's error body otherwise.
use super::*;
use crate::{HttpResponse, Output, Usage};
use base64::Engine as _;
use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Generated {
    candidates: Vec<Candidate>,
    prompt_feedback: Option<Feedback>,
    usage_metadata: Option<Metadata>,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Candidate {
    content: Option<Content>,
    finish_reason: Option<String>,
    finish_message: Option<String>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Content {
    parts: Vec<Part>,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Part {
    text: Option<String>,
    thought: bool,
    inline_data: Option<Inline>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Inline {
    mime_type: String,
    data: String,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Feedback {
    block_reason: Option<String>,
    block_reason_message: Option<String>,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Metadata {
    prompt_token_count: Option<u64>,
    candidates_token_count: Option<u64>,
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
/// before anything is generated (and Google bills only replies with a 200): a malformed request,
/// a rejected token or permission, an unknown model or a quota. Anything else, a 5xx or an
/// unparsed body included, stays possibly billed.
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
        // A bad image or setting: Google's explanation is what the person needs.
        400 => match status(&r.body) {
            Some((_, message)) if !message.is_empty() => format!("Google Cloud refused the Gemini image request: {}", said(&message, bearer)),
            _ => "Google Cloud refused the Gemini image request".into(),
        },
        401 => "Google Cloud refused the service account's token".into(),
        403 => format!(
            "Google Cloud refused the service account: check that the Vertex AI API is enabled in project {project} and the account has the Vertex AI User role"
        ),
        404 => "Google Cloud has no such Gemini image model for this project".into(),
        429 => "Google Cloud's Gemini image quota is used up for now; try again shortly".into(),
        _ => "the Gemini image request failed; it may have been billed".into(),
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

/// A finish reason that means a filter stopped the image.
fn filtered(reason: &str) -> bool {
    matches!(
        reason,
        "SAFETY"
            | "IMAGE_SAFETY"
            | "PROHIBITED_CONTENT"
            | "IMAGE_PROHIBITED_CONTENT"
            | "BLOCKLIST"
            | "SPII"
            | "RECITATION"
            | "IMAGE_RECITATION"
            | "MODEL_ARMOR"
    )
}
/// A reason code as Google spells it, or nothing for anything else.
fn code(reason: &str) -> Option<&str> {
    Some(reason).filter(|r| !r.is_empty() && r.len() <= 40 && r.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
}
fn explained(base: String, detail: Option<&str>, bearer: &str) -> String {
    match detail.map(|d| said(d, bearer)).filter(|d| !d.is_empty()) {
        Some(detail) => format!("{base}: {detail}"),
        None => base,
    }
}

/// The images the request made, with the tokens Google counted.
pub(super) fn parse(r: &HttpResponse, l: Limits, project: &str, bearer: &str) -> Result<Completed, ProviderError> {
    if r.body.len() > l.response_bytes {
        return Err(malformed("the Gemini image reply exceeds its byte limit; the images may have been billed"));
    }
    if !r.is_success() {
        return Err(failure(r, project, bearer));
    }
    let reply: Generated = serde_json::from_slice(&r.body).map_err(|_| malformed("the Gemini image reply can't be read; the images may have been billed"))?;
    if let Some(feedback) = &reply.prompt_feedback
        && let Some(reason) = feedback.block_reason.as_deref()
    {
        let base = format!("Google blocked the prompt ({})", code(reason).unwrap_or("unknown reason"));
        return Err(ProviderError::new(ErrorKind::BlockedContent, explained(base, feedback.block_reason_message.as_deref(), bearer)));
    }
    let candidate = reply.candidates.first().ok_or_else(|| malformed("the Gemini image reply has no candidate; it may have been billed"))?;
    let parts = candidate.content.as_ref().map(|c| c.parts.as_slice()).unwrap_or_default();
    let mut outputs = Vec::new();
    let mut total = 0usize;
    for inline in parts.iter().filter(|p| !p.thought).filter_map(|p| p.inline_data.as_ref()) {
        // Base64 is four characters for every three bytes.
        let size = inline.data.len() / 4 * 3;
        if size > l.output_bytes {
            return Err(malformed("a Gemini image exceeds the size limit"));
        }
        total = total.checked_add(size).filter(|n| *n <= l.total_output_bytes).ok_or_else(|| malformed("the Gemini images exceed the size limit together"))?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(inline.data.as_bytes()).map_err(|_| malformed("a Gemini image isn't valid base64"))?;
        let sniffed = nodecraft_media::sniff(&bytes).map(|s| s.mime);
        let mime = match sniffed {
            Some(m @ ("image/png" | "image/jpeg" | "image/webp")) if m == inline.mime_type => m,
            _ => return Err(malformed("the Gemini reply's image isn't the PNG, JPEG or WebP it says it is")),
        };
        outputs.push(Output::Bytes { mime: mime.into(), bytes: Arc::from(bytes) });
    }
    if outputs.is_empty() {
        let reason = candidate.finish_reason.as_deref().unwrap_or_default();
        if filtered(reason) {
            let base = format!("Google's filters stopped the image ({}); try another prompt or reference", code(reason).unwrap_or("filtered"));
            return Err(ProviderError::new(ErrorKind::BlockedContent, explained(base, candidate.finish_message.as_deref(), bearer)));
        }
        // The model may answer in words instead (a question back, say); those words explain it.
        let words = parts.iter().filter(|p| !p.thought).find_map(|p| p.text.as_deref()).or(candidate.finish_message.as_deref());
        let base = format!("the Gemini model answered without an image ({})", code(reason).unwrap_or("no reason given"));
        return Err(ProviderError::new(ErrorKind::JobFailed, explained(base, words, bearer)));
    }
    let metadata = reply.usage_metadata.unwrap_or_default();
    let usage = Usage { input_tokens: metadata.prompt_token_count, output_tokens: metadata.candidates_token_count, ..Usage::default() };
    Ok(Completed { outputs, usage })
}

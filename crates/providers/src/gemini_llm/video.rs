//! Video to text (#84). Google's OpenAI-compatible endpoint, which the text route uses, documents
//! text, image and audio parts only, so a job that carries a video goes as one stateless
//! Interactions request instead (`POST /v1beta/interactions`, `store`, `background` and `stream`
//! false), each video inline as base64. Everything here is checked before the request is built,
//! and nothing touches the network.
//!
//! Contract checked 2026-11-03: <https://ai.google.dev/gemini-api/docs/video-understanding>
//! (inline video under 20 MB total request size, at most ten videos a request, frames sampled at
//! one a second by default) and <https://ai.google.dev/api/interactions-api> (the `video` input's
//! `mime_type` enum, `generation_config.max_output_tokens`, the `Interaction` status, steps and
//! usage).
use super::{MAX_TEXT_BYTES, MAX_TOKENS, MODEL};
use crate::{Completed, ErrorKind, HttpResponse, JobInput, Output, ProviderError, ProviderJob, Usage};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::value::RawValue;
use std::borrow::Cow;

pub const URL: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";
/// The role a wired video arrives on, as the catalog's `video-to-text` mode declares it.
pub const ROLE: &str = "video";
/// Videos one request may carry: the guide's limit for Gemini 2.5 and later.
pub const MAX_VIDEOS: usize = 10;
/// The whole request, video base64 included. The guide says inline data suits requests under
/// 20 MB and to use the Files API above that (its table says 100 MB; the lower figure is kept).
pub const MAX_REQUEST_BYTES: usize = 20_000_000;
/// The reply. An answer is text, but the reply's step list may echo the input.
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// How long the one synchronous request may take; a long video takes the model a while.
pub const REQUEST_TIMEOUT_MS: u64 = 600_000;
/// The `mime_type` values the Interactions reference lists for a video input.
pub const MIMES: [&str; 9] = ["video/mp4", "video/mpeg", "video/mpg", "video/mov", "video/avi", "video/x-flv", "video/webm", "video/wmv", "video/3gpp"];

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, message)
}
fn malformed(message: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, message)
}

/// The `mime_type` a stored video is sent as. NodeCraft names QuickTime files `video/quicktime`;
/// Google lists that container as `video/mov`. Anything not listed (Matroska, for one) is refused.
pub fn wire_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "video/quicktime" => Some("video/mov"),
        other => MIMES.iter().copied().find(|listed| *listed == other),
    }
}

/// Whether `job` is a video-to-text job, so it takes this route.
pub fn carries_video(job: &ProviderJob) -> bool {
    job.inputs.iter().any(|input| matches!(input, JobInput::Media { .. }))
}

fn base64_len(bytes: usize) -> Option<usize> {
    bytes.div_ceil(3).checked_mul(4)
}

/// The request body for one prompt and one to ten videos, sized before it is built: a request
/// over [`MAX_REQUEST_BYTES`] or a video of an unlisted type is refused before any copy is made.
pub fn body(job: &ProviderJob, max_tokens: u64) -> Result<Vec<u8>, ProviderError> {
    if job.model != MODEL || !(1..=MAX_TOKENS).contains(&max_tokens) {
        return Err(invalid("Gemini video to text needs Gemini 3.8 Flash and max_tokens from 1 to 65536"));
    }
    let mut prompt = None;
    let mut videos = Vec::new();
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" && prompt.is_none() => {
                if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
                    return Err(invalid("a Gemini video prompt is 1 byte to 1 MiB of text"));
                }
                prompt = Some(text.as_str());
            }
            JobInput::Media { role, mime, bytes } if role == ROLE => {
                if videos.len() >= MAX_VIDEOS {
                    return Err(invalid(format!("Gemini reads at most {MAX_VIDEOS} videos a request")));
                }
                let Some(wire) = wire_mime(mime) else {
                    return Err(invalid(format!("Gemini doesn't read {} video; use MP4, MOV, WebM, MPEG, AVI, FLV, WMV or 3GPP", excerpt(mime))));
                };
                if bytes.is_empty() {
                    return Err(invalid("a wired video is empty"));
                }
                videos.push((wire, bytes.as_ref()));
            }
            _ => return Err(invalid("Gemini video to text takes one prompt and one to ten videos")),
        }
    }
    let prompt = prompt.ok_or_else(|| invalid("Gemini video to text needs a prompt"))?;
    if videos.is_empty() {
        return Err(invalid("Gemini video to text needs a video"));
    }
    let head = format!(
        "{{\"model\":\"{MODEL}\",\"store\":false,\"background\":false,\"stream\":false,\"generation_config\":{{\"max_output_tokens\":{max_tokens},\"thinking_summaries\":\"none\"}},\"input\":[{{\"type\":\"text\",\"text\":"
    );
    let text = serde_json::to_vec(prompt).map_err(|_| invalid("the Gemini prompt can't be encoded"))?;
    let mut length = head.len().checked_add(text.len()).and_then(|n| n.checked_add(1));
    for (mime, bytes) in &videos {
        // ,{"type":"video","mime_type":"<mime>","data":"<base64>"}
        let part = base64_len(bytes.len()).and_then(|n| n.checked_add(42 + mime.len()));
        length = length.zip(part).and_then(|(a, b)| a.checked_add(b));
    }
    let length = length.and_then(|n| n.checked_add(2)).filter(|n| *n <= MAX_REQUEST_BYTES).ok_or_else(|| {
        invalid(format!(
            "the wired videos are too large to send inline: Gemini takes requests under {} MB; use shorter or smaller videos",
            MAX_REQUEST_BYTES / 1_000_000
        ))
    })?;
    let mut out = Vec::new();
    out.try_reserve_exact(length).map_err(|_| invalid("not enough memory for the Gemini request"))?;
    out.extend_from_slice(head.as_bytes());
    out.extend_from_slice(&text);
    out.push(b'}');
    for (mime, bytes) in &videos {
        out.extend_from_slice(b",{\"type\":\"video\",\"mime_type\":\"");
        out.extend_from_slice(mime.as_bytes());
        out.extend_from_slice(b"\",\"data\":\"");
        let start = out.len();
        let encoded = base64_len(bytes.len()).ok_or_else(|| invalid("a wired video is too large"))?;
        out.resize(start.saturating_add(encoded), 0);
        let target = out.get_mut(start..).ok_or_else(|| invalid("the Gemini request extent changed"))?;
        let n = base64::engine::general_purpose::STANDARD.encode_slice(bytes, target).map_err(|_| invalid("a wired video can't be encoded"))?;
        if n != encoded {
            return Err(invalid("the Gemini request extent changed"));
        }
        out.extend_from_slice(b"\"}");
    }
    out.extend_from_slice(b"]}");
    if out.len() != length {
        return Err(invalid("the Gemini request extent changed"));
    }
    Ok(out)
}

/// At most 64 printable bytes of something a caller supplied, for a message.
fn excerpt(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(64).collect()
}

#[derive(Deserialize)]
struct Content<'a> {
    #[serde(borrow, rename = "type")]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
}
#[derive(Deserialize)]
struct Step<'a> {
    #[serde(borrow, rename = "type")]
    kind: Cow<'a, str>,
    // Left unparsed: only a model output's content is read, so an echoed input is never copied.
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}
#[derive(Deserialize)]
struct Tokens {
    total_input_tokens: Option<u64>,
    total_output_tokens: Option<u64>,
}
#[derive(Deserialize)]
struct Reply<'a> {
    #[serde(borrow)]
    status: Cow<'a, str>,
    #[serde(borrow, default)]
    steps: Vec<Step<'a>>,
    #[serde(borrow)]
    usage: Option<&'a RawValue>,
}

/// Most steps and output parts a reply may carry.
const MAX_STEPS: usize = 256;

/// A refused request's error. Every refusal after sending may have been billed.
pub fn error(reply: &HttpResponse) -> ProviderError {
    let kind = match reply.status {
        300..=399 => ErrorKind::Malformed,
        400 | 413 | 415 | 422 => ErrorKind::UnsupportedInput,
        401 | 403 => ErrorKind::Auth,
        402 => ErrorKind::Budget,
        404 | 410 => ErrorKind::NotFound,
        408 | 504 => ErrorKind::Timeout,
        429 => ErrorKind::RateLimited,
        503 => ErrorKind::Busy,
        _ => ErrorKind::Server,
    };
    let mut e = ProviderError::new(kind, format!("the Gemini video request failed (HTTP {}); it may have been billed", reply.status));
    e.retry_after_ms = reply
        .header("retry-after")
        .filter(|s| s.len() <= 16)
        .and_then(|s| s.trim().parse::<u64>().ok())
        .and_then(|n| n.checked_mul(1000))
        .filter(|n| *n <= 3_600_000);
    e
}

/// The answer in a completed reply: every model output's text, in order. Any other output kind,
/// a step this adapter doesn't know, or a reply that isn't complete refuses the whole reply.
pub fn parse(reply: &HttpResponse) -> Result<Completed, ProviderError> {
    if reply.body.len() > MAX_RESPONSE_BYTES {
        return Err(malformed("the Gemini reply exceeds its byte limit"));
    }
    if !reply.is_success() {
        return Err(error(reply));
    }
    let parsed: Reply<'_> = serde_json::from_slice(&reply.body).map_err(|_| malformed("the Gemini reply lacks its required fields"))?;
    match parsed.status.as_ref() {
        "completed" => {}
        "cancelled" => return Err(ProviderError::new(ErrorKind::Cancelled, "Gemini cancelled the request; it may have been billed")),
        "failed" => return Err(ProviderError::new(ErrorKind::JobFailed, "Gemini couldn't answer about the video")),
        "incomplete" => return Err(malformed("Gemini returned an incomplete answer; no partial answer is kept")),
        _ => return Err(malformed("Gemini returned an unfinished request")),
    }
    if parsed.steps.len() > MAX_STEPS {
        return Err(malformed("too many Gemini reply steps"));
    }
    let mut answer = String::new();
    let mut parts = 0usize;
    for step in &parsed.steps {
        match step.kind.as_ref() {
            "thought" | "user_input" => {}
            "model_output" => {
                let raw = step.content.ok_or_else(|| malformed("a Gemini output has no content"))?;
                let content: Vec<Content<'_>> = serde_json::from_str(raw.get()).map_err(|_| malformed("a Gemini output's content is invalid"))?;
                for c in content {
                    parts = parts.saturating_add(1);
                    if parts > MAX_STEPS || c.kind != "text" {
                        return Err(malformed("Gemini answered with something other than text"));
                    }
                    let text = c.text.ok_or_else(|| malformed("a Gemini text output has no text"))?;
                    if answer.len().saturating_add(text.len()) > MAX_TEXT_BYTES * 8 {
                        return Err(malformed("the Gemini answer exceeds its byte limit"));
                    }
                    answer.push_str(&text);
                }
            }
            _ => return Err(malformed("Gemini returned a step this adapter doesn't know")),
        }
    }
    if answer.is_empty() {
        return Err(malformed("Gemini returned no answer about the video"));
    }
    let usage = parsed.usage.and_then(|raw| serde_json::from_str::<Tokens>(raw.get()).ok());
    let usage = usage.map_or_else(Usage::default, |t| Usage { input_tokens: t.total_input_tokens, output_tokens: t.total_output_tokens, ..Usage::default() });
    Ok(Completed { outputs: vec![Output::Text(answer)], usage })
}

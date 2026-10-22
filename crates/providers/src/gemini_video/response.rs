//! Gemini's replies read into what the adapter acts on. Nothing here touches the network.
use super::*;
use base64::Engine as _;
use serde::Deserialize;

/// What the generate request produced.
pub(super) enum Created {
    /// The video came inline.
    Inline(Arc<[u8]>, Usage),
    /// The video is a file to poll and download, named by its id.
    File(String, Usage),
}
pub(super) enum FileState {
    Processing,
    Active,
    Failed,
}

#[derive(Deserialize)]
struct Content {
    #[serde(rename = "type")]
    kind: String,
    data: Option<String>,
    uri: Option<String>,
    mime_type: Option<String>,
}
#[derive(Deserialize)]
struct Step {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    content: Vec<Content>,
}
#[derive(Deserialize)]
struct Interaction {
    status: String,
    #[serde(default)]
    steps: Vec<Step>,
    output_video: Option<Content>,
    /// Read leniently: a counter in an unexpected shape costs the token counts, never the video.
    #[serde(default)]
    usage: serde_json::Value,
}
#[derive(Deserialize)]
struct File {
    state: Option<String>,
}

/// A reply that isn't a success becomes the error a person can act on.
pub(super) fn check(reply: &HttpResponse, l: Limits, paid: bool) -> Result<(), ProviderError> {
    if reply.body.len() > l.response_bytes {
        return Err(malformed("the Gemini reply exceeds its byte limit"));
    }
    if reply.is_success() {
        return Ok(());
    }
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
    let message = match (paid, reply.status) {
        (true, _) => "the Gemini video request failed; it may have been billed",
        (false, 404 | 410) => "the Gemini video file is gone; Google keeps files for two days",
        (false, _) => "the Gemini video file check failed",
    };
    let mut error = ProviderError::new(kind, format!("{message} (HTTP {})", reply.status));
    error.retry_after_ms = reply
        .header("retry-after")
        .filter(|s| s.len() <= 16)
        .and_then(|s| s.trim().parse::<u64>().ok())
        .and_then(|n| n.checked_mul(1000))
        .filter(|n| *n <= 3_600_000);
    Err(error)
}
/// The file id in a file reference: `files/<id>`, or any path on the API's origin ending in
/// `files/<id>` (a versioned resource name or a `…:download?alt=media` link).
fn file_ref(uri: &str, key: &Secret) -> Result<String, ProviderError> {
    let refused = || malformed("the Gemini video file reference isn't a Gemini file");
    let path = match uri.strip_prefix(ORIGIN) {
        Some(rest) => rest.strip_prefix('/').ok_or_else(refused)?,
        None if uri.contains("://") => return Err(refused()),
        None => uri,
    };
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    let path = path.strip_suffix(":download").unwrap_or(path);
    let (parent, id) = path.rsplit_once('/').ok_or_else(refused)?;
    if parent != "files" && !parent.ends_with("/files") {
        return Err(refused());
    }
    file_id(id, key)?;
    Ok(id.to_owned())
}
fn usage(raw: &serde_json::Value) -> Usage {
    let count = |name: &str| raw.get(name).and_then(serde_json::Value::as_u64);
    Usage { input_tokens: count("total_input_tokens"), output_tokens: count("total_output_tokens"), ..Usage::default() }
}
pub(super) fn created(body: &[u8], l: Limits, key: &Secret) -> Result<Created, ProviderError> {
    let reply: Interaction = serde_json::from_slice(body).map_err(|_| malformed("the Gemini video reply can't be read"))?;
    match reply.status.as_str() {
        "completed" => {}
        "cancelled" => return Err(ProviderError::new(ErrorKind::Cancelled, "Gemini cancelled the video; billing may be unresolved")),
        "failed" => {
            return Err(ProviderError::new(ErrorKind::JobFailed, "Gemini couldn't make the video; check the prompt and images against its safety rules"));
        }
        "incomplete" => return Err(ProviderError::new(ErrorKind::JobFailed, "Gemini returned an incomplete video; nothing was kept")),
        _ => return Err(malformed("the Gemini video reply has an unknown status")),
    }
    let usage = usage(&reply.usage);
    let mut videos = reply.steps.into_iter().filter(|s| s.kind == "model_output").flat_map(|s| s.content).filter(|c| c.kind == "video");
    let output = match (videos.next(), videos.next()) {
        (Some(video), None) => video,
        (None, None) => reply
            .output_video
            .filter(|c| c.kind == "video")
            .ok_or_else(|| ProviderError::new(ErrorKind::BlockedContent, "Gemini returned no video; revise the prompt or check its safety rules"))?,
        _ => return Err(malformed("Gemini returned more than one video")),
    };
    if output.mime_type.as_deref().is_some_and(|m| m != MP4) {
        return Err(malformed("Gemini returned a video that isn't MP4"));
    }
    match (output.uri, output.data) {
        (Some(uri), None) => Ok(Created::File(file_ref(&uri, key)?, usage)),
        (None, Some(data)) => {
            // Base64 is four characters for every three bytes: refuse an oversize video unread.
            if data.len() / 4 * 3 > l.output_bytes {
                return Err(malformed("the Gemini video exceeds the download limit"));
            }
            let bytes = base64::engine::general_purpose::STANDARD.decode(data.as_bytes()).map_err(|_| malformed("the Gemini video data isn't base64"))?;
            Ok(Created::Inline(video(bytes, l)?, usage))
        }
        _ => Err(malformed("the Gemini video has neither a file reference nor data")),
    }
}
pub(super) fn file_state(body: &[u8]) -> Result<FileState, ProviderError> {
    let file: File = serde_json::from_slice(body).map_err(|_| malformed("the Gemini file record can't be read"))?;
    match file.state.as_deref() {
        Some("ACTIVE") => Ok(FileState::Active),
        Some("FAILED") => Ok(FileState::Failed),
        Some("PROCESSING" | "STATE_UNSPECIFIED") | None => Ok(FileState::Processing),
        Some(_) => Err(malformed("the Gemini file has an unknown state")),
    }
}
/// A downloaded video: within the limit, and an MP4 by its bytes.
pub(super) fn video(bytes: Vec<u8>, l: Limits) -> Result<Arc<[u8]>, ProviderError> {
    if bytes.len() > l.output_bytes {
        return Err(malformed("the Gemini video exceeds the download limit"));
    }
    if nodecraft_media::sniff(&bytes).map(|s| s.mime) != Some(MP4) {
        return Err(malformed("the Gemini video isn't an MP4 file"));
    }
    Ok(Arc::from(bytes))
}

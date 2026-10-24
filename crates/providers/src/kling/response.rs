//! Kling's replies: the `{code, message, data}` envelope, whose code says what went wrong, the
//! task receipt, task status and the result link.
use super::*;
use serde_json::Value;

/// Kling's own message, cleaned and shortened, unless it echoes the key or a signed token (whose
/// header always encodes to `eyJ`).
fn said(message: &str, key: &auth::Key) -> String {
    if key.appears_in(message) || message.contains("eyJ") {
        return "(message withheld)".into();
    }
    message.chars().filter(|c| !c.is_control()).take(300).collect()
}
/// What a Kling business code means. Codes Kling doesn't document fall back to the HTTP status.
fn kind(code: u64, status: u16) -> ErrorKind {
    match code {
        1000..=1004 => ErrorKind::Auth,
        1100 | 1103 => ErrorKind::Auth,
        1101 | 1102 => ErrorKind::Budget,
        1200..=1202 => ErrorKind::UnsupportedInput,
        1203 => ErrorKind::NotFound,
        1300 | 1301 => ErrorKind::BlockedContent,
        1302 => ErrorKind::RateLimited,
        1303 => ErrorKind::Busy,
        1304 => ErrorKind::Auth,
        5000 => ErrorKind::Server,
        5001 => ErrorKind::Busy,
        5002 => ErrorKind::Timeout,
        _ => ProviderError::from_status(status, b"").kind,
    }
}
/// Whether a reply to the create is one of Kling's documented refusals, made before any task
/// exists: a key, balance, request, policy or rate problem in a parsed envelope. A server error,
/// an unreadable body, or a refusal naming an existing task (a repeated `external_task_id`, whose
/// first create may have been billed) stays possibly billed.
pub(super) fn refused_create(r: &HttpResponse, l: Limits) -> bool {
    if r.body.len() > l.response_bytes || r.status >= 500 {
        return false;
    }
    let Some(reply) = serde_json::from_slice::<Value>(&r.body).ok() else { return false };
    let message = reply.get("message").and_then(Value::as_str).unwrap_or_default().to_ascii_lowercase();
    if ["external", "exist", "duplicate", "repeat"].iter().any(|word| message.contains(word)) {
        return false;
    }
    matches!(reply.get("code").and_then(Value::as_u64), Some(1000..=1004 | 1100..=1103 | 1200..=1202 | 1300..=1304))
}
/// A reply that fits the limit and says it succeeded; anything else becomes the error a person
/// can act on.
pub(super) fn check(r: &HttpResponse, l: Limits, key: &auth::Key, paid: bool) -> Result<(), ProviderError> {
    if r.body.len() > l.response_bytes {
        return Err(malformed("Kling reply exceeds the size limit"));
    }
    let parsed: Option<Value> = serde_json::from_slice(&r.body).ok();
    let code = parsed.as_ref().and_then(|v| v.get("code")).and_then(Value::as_u64);
    if r.is_success() && code == Some(0) {
        return Ok(());
    }
    if r.is_success() && code.is_none() {
        return Err(malformed("Kling reply isn't the expected JSON"));
    }
    let message = parsed.as_ref().and_then(|v| v.get("message")).and_then(Value::as_str).unwrap_or_default();
    let code = code.unwrap_or_default();
    let kind = kind(code, if r.is_success() { 500 } else { r.status });
    let billed = if paid && !matches!(kind, ErrorKind::Auth | ErrorKind::Budget | ErrorKind::UnsupportedInput | ErrorKind::BlockedContent) {
        "; the task may have been created"
    } else {
        ""
    };
    let mut error = ProviderError::new(kind, format!("Kling refused the request (HTTP {}, code {code}): {}{billed}", r.status, said(message, key)));
    error.retry_after_ms = r
        .header("retry-after")
        .filter(|v| v.len() <= 16)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .and_then(|s| s.checked_mul(1000))
        .filter(|ms| *ms <= 3_600_000);
    Err(error)
}
fn data(body: &[u8]) -> Result<Value, ProviderError> {
    let mut reply: Value = serde_json::from_slice(body).map_err(|_| malformed("Kling reply isn't the expected JSON"))?;
    reply.get_mut("data").map(Value::take).filter(Value::is_object).ok_or_else(|| malformed("Kling reply has no task"))
}
pub(super) fn created(body: &[u8], key: &auth::Key) -> Result<String, ProviderError> {
    let id = data(body)?.get("task_id").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| malformed("Kling created no task id"))?;
    task_id(&id, key)?;
    Ok(id)
}
pub(super) enum Status {
    Progress(JobProgress),
    /// Finished: the link to download.
    Done(String),
}
pub(super) fn status(body: &[u8], id: &str, image: bool, key: &auth::Key) -> Result<Status, ProviderError> {
    let task = data(body)?;
    if task.get("task_id").and_then(Value::as_str) != Some(id) {
        return Err(malformed("Kling status is for another task"));
    }
    match task.get("task_status").and_then(Value::as_str).ok_or_else(|| malformed("Kling status missing"))? {
        "submitted" => Ok(Status::Progress(JobProgress::Queued { position: None })),
        "processing" => Ok(Status::Progress(JobProgress::Running { fraction: None, message: None, partial: None })),
        "succeed" | "succeeded" => {
            let list = if image { "images" } else { "videos" };
            let url = task
                .get("task_result")
                .and_then(|r| r.get(list))
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("url"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| malformed("Kling finished without a result"))?;
            if key.appears_in(url) {
                return Err(malformed("Kling result link refused"));
            }
            Ok(Status::Done(crate::model_file::public_link(url, "Kling")?))
        }
        "failed" => {
            let detail = task.get("task_status_msg").and_then(Value::as_str).map(|m| said(m, key)).unwrap_or_default();
            let message = if detail.is_empty() { "Kling couldn't make the result".to_owned() } else { format!("Kling couldn't make the result: {detail}") };
            Ok(Status::Progress(JobProgress::Failed(ProviderError::new(ErrorKind::JobFailed, message))))
        }
        _ => Err(malformed("Kling reported an unknown status")),
    }
}
/// A downloaded result: within the limit, and an MP4 video or a PNG, JPEG or WebP image by its
/// bytes.
pub(super) fn result(bytes: Vec<u8>, image: bool, l: Limits) -> Result<(String, Arc<[u8]>), ProviderError> {
    if bytes.len() > l.output_bytes {
        return Err(malformed("the Kling result exceeds the download limit"));
    }
    let mime = nodecraft_media::sniff(&bytes).map(|s| s.mime);
    let mime = match (image, mime) {
        (false, Some(MP4)) => MP4,
        (true, Some(mime @ ("image/png" | "image/jpeg" | "image/webp"))) => mime,
        (false, _) => return Err(malformed("the Kling video isn't an MP4 file")),
        (true, _) => return Err(malformed("the Kling image isn't a PNG, JPEG or WebP file")),
    };
    Ok((mime.to_owned(), Arc::from(bytes)))
}

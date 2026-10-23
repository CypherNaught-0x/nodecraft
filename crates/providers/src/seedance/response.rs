//! ModelArk's replies: its `{error: {code, message}}` body, the task receipt and status, and the
//! finished video.
use super::*;
use serde_json::Value;

fn json(body: &[u8]) -> Result<Value, ProviderError> {
    serde_json::from_slice(body).map_err(|_| malformed("Seedance reply isn't the expected JSON"))
}
/// The error's code and BytePlus's own message, cleaned and shortened, unless it echoes the key.
fn error(body: &[u8], key: &Secret) -> (String, String) {
    let parsed = json(body).ok();
    let error = parsed.as_ref().and_then(|v| v.get("error"));
    let text = |name: &str| error.and_then(|e| e.get(name)).and_then(Value::as_str).unwrap_or_default();
    let clean = |s: &str, n: usize| -> String { s.chars().filter(|c| !c.is_control()).take(n).collect() };
    let (code, message) = (clean(text("code"), 96), clean(text("message"), 300));
    if code.contains(key.expose()) || message.contains(key.expose()) {
        return (String::new(), "(message withheld)".into());
    }
    (code, message)
}
fn kind(code: &str, status: u16) -> ErrorKind {
    if code.contains("SensitiveContentDetected") {
        return ErrorKind::BlockedContent;
    }
    match code {
        "AuthenticationError" | "InvalidAccountStatus" | "AccessDenied" | "OperationDenied.PermissionDenied" => ErrorKind::Auth,
        "AccountOverdueError" | "OperationDenied.ServiceOverdue" | "SetLimitExceeded" => ErrorKind::Budget,
        "ModelNotOpen" | "OperationDenied.ServiceNotOpen" | "InvalidEndpointOrModel.NotFound" => ErrorKind::Auth,
        "ServerOverloaded" => ErrorKind::Busy,
        _ if status == 429 => ErrorKind::RateLimited,
        _ if status == 400 => ErrorKind::UnsupportedInput,
        _ => ProviderError::from_status(status, b"").kind,
    }
}
/// Whether a task create was refused before any task existed: a code from ModelArk's error
/// table at the status it documents for it. A 5xx, a lost reply, an unreadable body or a code the
/// table doesn't list may still hide a created task.
pub(super) fn refused_create(r: &HttpResponse) -> bool {
    let Some(code) = json(&r.body).ok().and_then(|v| v.get("error")?.get("code")?.as_str().map(str::to_owned)) else { return false };
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| code.starts_with(p));
    match r.status {
        400 => {
            code.contains("SensitiveContentDetected")
                || starts(&["InvalidParameter", "MissingParameter", "InvalidArgumentError", "InvalidImageURL", "InvalidPayload", "InvalidEndpoint."])
        }
        401 => matches!(code.as_str(), "AuthenticationError" | "InvalidAccountStatus" | "MissingHeader"),
        403 => matches!(code.as_str(), "AccessDenied" | "AccountOverdueError") || code.starts_with("OperationDenied"),
        404 => {
            matches!(code.as_str(), "ModelNotOpen" | "UnsupportedModel" | "NotFound." | "PathNotFound" | "ResourceNotFound")
                || code.starts_with("InvalidEndpointOrModel.")
        }
        429 => {
            code.contains("RateLimitExceeded")
                || matches!(code.as_str(), "QuotaExceeded" | "SetLimitExceeded" | "ServerOverloaded" | "RequestBurstTooFast" | "InflightBatchsizeExceeded")
        }
        _ => false,
    }
}
/// A reply that fits the limit, with a failing status turned into an error.
pub(super) fn check(r: &HttpResponse, l: Limits, key: &Secret, paid: bool) -> Result<(), ProviderError> {
    if r.body.len() > l.response_bytes {
        return Err(malformed("Seedance reply exceeds the size limit"));
    }
    if r.is_success() {
        return Ok(());
    }
    let (code, message) = error(&r.body, key);
    let billed = if paid && !refused_create(r) { "; the task may have been created and billed once it succeeds" } else { "" };
    let mut e = ProviderError::new(kind(&code, r.status), format!("Seedance refused the request (HTTP {}, {code}): {message}{billed}", r.status));
    e.retry_after_ms = r
        .header("retry-after")
        .filter(|v| v.len() <= 16)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .and_then(|s| s.checked_mul(1000))
        .filter(|ms| *ms <= 3_600_000);
    Err(e)
}
pub(super) fn created(body: &[u8], key: &Secret) -> Result<String, ProviderError> {
    let id = json(body)?.get("id").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| malformed("Seedance created no task id"))?;
    task_id(&id, key)?;
    Ok(id)
}
pub(super) enum Status {
    Progress(JobProgress),
    /// Succeeded: the link to download and the tokens billed.
    Done {
        url: String,
        tokens: Option<u64>,
    },
}
pub(super) fn status(body: &[u8], id: &str, key: &Secret) -> Result<Status, ProviderError> {
    let task = json(body)?;
    if task.get("id").and_then(Value::as_str) != Some(id) {
        return Err(malformed("Seedance status is for another task"));
    }
    let failed = |kind, message: String| Ok(Status::Progress(JobProgress::Failed(ProviderError::new(kind, message))));
    match task.get("status").and_then(Value::as_str).ok_or_else(|| malformed("Seedance status missing"))? {
        "queued" => Ok(Status::Progress(JobProgress::Queued { position: None })),
        "running" => Ok(Status::Progress(JobProgress::Running { fraction: None, message: Some("Seedance is making the video".into()), partial: None })),
        "succeeded" => {
            let url = task
                .pointer("/content/video_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| malformed("Seedance finished without a video"))?;
            if url.contains(key.expose()) {
                return Err(malformed("Seedance result link refused"));
            }
            let tokens = task.pointer("/usage/completion_tokens").and_then(Value::as_u64);
            Ok(Status::Done { url: crate::model_file::public_link(url, "Seedance")?, tokens })
        }
        "failed" => {
            let (code, message) = error(body, key);
            let kind = if code.contains("SensitiveContentDetected") { ErrorKind::BlockedContent } else { ErrorKind::JobFailed };
            failed(kind, format!("Seedance couldn't make the video ({code}): {message}; a failed task isn't billed"))
        }
        "cancelled" => failed(ErrorKind::Cancelled, "the Seedance task was cancelled".into()),
        "expired" => failed(ErrorKind::JobFailed, "the Seedance task expired before it finished; it isn't billed".into()),
        _ => Err(malformed("Seedance reported an unknown status")),
    }
}
pub(super) fn video(bytes: Vec<u8>, l: Limits) -> Result<Arc<[u8]>, ProviderError> {
    if bytes.len() > l.output_bytes {
        return Err(malformed("the Seedance video exceeds the download limit"));
    }
    if nodecraft_media::sniff(&bytes).map(|s| s.mime) != Some(MP4) {
        return Err(malformed("the Seedance video isn't an MP4 file"));
    }
    Ok(Arc::from(bytes))
}

//! Meshy's replies: HTTP status errors with a `message`, the `{result}` receipt, task status and
//! the GLB link.
use super::*;
use serde_json::Value;

/// Meshy's own message, cleaned and shortened, unless it echoes the key.
fn said(body: &[u8], key: &Secret) -> String {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let text = parsed.as_ref().and_then(|v| v.get("message")).and_then(Value::as_str).unwrap_or_default();
    if text.contains(key.expose()) {
        return "(message withheld)".into();
    }
    text.chars().filter(|c| !c.is_control()).take(300).collect()
}
/// A reply that fits the limit, with a failing status turned into an error.
pub(super) fn check(r: &HttpResponse, l: Limits, key: &Secret) -> Result<(), ProviderError> {
    if r.body.len() > l.response_bytes {
        return Err(malformed("Meshy reply exceeds the size limit"));
    }
    if r.is_success() {
        return Ok(());
    }
    let kind = ProviderError::from_status(r.status, b"").kind;
    let mut error = ProviderError::new(kind, format!("Meshy refused the request (HTTP {}): {}", r.status, said(&r.body, key)));
    error.retry_after_ms = r
        .header("retry-after")
        .filter(|v| v.len() <= 16)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .and_then(|s| s.checked_mul(1000))
        .filter(|ms| *ms <= 3_600_000);
    Err(error)
}
fn json(body: &[u8]) -> Result<Value, ProviderError> {
    serde_json::from_slice(body).map_err(|_| malformed("Meshy reply isn't the expected JSON"))
}
pub(super) fn created(body: &[u8], key: &Secret) -> Result<String, ProviderError> {
    let id = json(body)?.get("result").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| malformed("Meshy created no task id"))?;
    task_id(&id, key)?;
    Ok(id)
}
pub(super) enum Status {
    Progress(JobProgress),
    /// Finished: the link to download.
    Done(String),
}
pub(super) fn status(body: &[u8], id: &str, key: &Secret) -> Result<Status, ProviderError> {
    let task = json(body)?;
    if task.get("id").and_then(Value::as_str) != Some(id) {
        return Err(malformed("Meshy status is for another task"));
    }
    match task.get("status").and_then(Value::as_str).ok_or_else(|| malformed("Meshy status missing"))? {
        "PENDING" => {
            let position = task.get("preceding_tasks").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
            Ok(Status::Progress(JobProgress::Queued { position }))
        }
        "IN_PROGRESS" => {
            let fraction = task.get("progress").and_then(Value::as_f64).filter(|p| p.is_finite()).map(|p| (p.clamp(0.0, 100.0) / 100.0) as f32);
            Ok(Status::Progress(JobProgress::Running { fraction, message: None, partial: None }))
        }
        "SUCCEEDED" => {
            let url = task
                .get("model_urls")
                .and_then(|urls| urls.get("glb"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| malformed("Meshy finished without a GLB model"))?;
            if url.contains(key.expose()) {
                return Err(malformed("Meshy result link refused"));
            }
            Ok(Status::Done(crate::model_file::public_link(url, "Meshy")?))
        }
        "FAILED" => {
            let detail = task.get("task_error").and_then(|e| e.get("message")).and_then(Value::as_str).unwrap_or_default();
            let detail: String = if detail.contains(key.expose()) { String::new() } else { detail.chars().filter(|c| !c.is_control()).take(300).collect() };
            let message = if detail.is_empty() { "Meshy couldn't make the model".to_owned() } else { format!("Meshy couldn't make the model: {detail}") };
            Ok(Status::Progress(JobProgress::Failed(ProviderError::new(ErrorKind::JobFailed, message))))
        }
        "CANCELED" => Ok(Status::Progress(JobProgress::Failed(ProviderError::new(ErrorKind::Cancelled, "the Meshy task was cancelled")))),
        _ => Err(malformed("Meshy reported an unknown status")),
    }
}

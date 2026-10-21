//! Tripo's replies: the `{code, data}` envelope, its error codes, task status and the GLB result.
use super::*;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Envelope {
    code: i64,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    data: Option<Value>,
}
fn envelope(body: &[u8]) -> Result<Envelope, ProviderError> {
    serde_json::from_slice(body).map_err(|_| malformed("Tripo reply isn't the expected JSON"))
}
/// Tripo's own message, cleaned and shortened, unless it echoes the key.
fn said(message: Option<&str>, key: &Secret) -> String {
    let text = message.unwrap_or_default();
    if text.contains(key.expose()) {
        return "(message withheld)".into();
    }
    let clean: String = text.chars().filter(|c| !c.is_control()).take(300).collect();
    clean
}
fn kind(code: i64, status: u16) -> ErrorKind {
    match code {
        1002 | 1005 => ErrorKind::Auth,
        1007 | 2000 => ErrorKind::RateLimited,
        2001 | 2019 => ErrorKind::NotFound,
        2008 => ErrorKind::BlockedContent,
        2010 => ErrorKind::Budget,
        1000 | 1001 => ErrorKind::Server,
        1003 | 1004 | 2002..=2007 | 2009 | 2011..=2018 => ErrorKind::UnsupportedInput,
        _ => ProviderError::from_status(status, b"").kind,
    }
}
/// A reply that fits the limit, with a failing status or nonzero code turned into an error.
pub(super) fn check(r: &HttpResponse, l: Limits, key: &Secret) -> Result<(), ProviderError> {
    if r.body.len() > l.response_bytes {
        return Err(malformed("Tripo reply exceeds the size limit"));
    }
    let parsed = envelope(&r.body).ok();
    let code = parsed.as_ref().map_or(0, |e| e.code);
    if r.is_success() && code == 0 {
        return Ok(());
    }
    let message = said(parsed.as_ref().and_then(|e| e.message.as_deref()), key);
    let mut error = ProviderError::new(kind(code, r.status), format!("Tripo refused the request (HTTP {}, code {code}): {message}", r.status));
    error.retry_after_ms = r
        .header("retry-after")
        .filter(|v| v.len() <= 16)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .and_then(|s| s.checked_mul(1000))
        .filter(|ms| *ms <= 3_600_000);
    Err(error)
}
fn data(body: &[u8]) -> Result<Value, ProviderError> {
    envelope(body)?.data.ok_or_else(|| malformed("Tripo reply has no data"))
}
pub(super) fn upload_token(body: &[u8], key: &Secret) -> Result<String, ProviderError> {
    let token = data(body)?.get("image_token").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| malformed("Tripo upload gave no token"))?;
    if token.is_empty() || token.len() > 256 || !token.bytes().all(|b| b.is_ascii_graphic()) || token.contains(key.expose()) {
        return Err(malformed("Tripo upload token refused"));
    }
    Ok(token)
}
pub(super) fn created(body: &[u8], key: &Secret) -> Result<String, ProviderError> {
    let id = data(body)?.get("task_id").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| malformed("Tripo created no task id"))?;
    task_id(&id, key)?;
    Ok(id)
}
pub(super) enum Status {
    Progress(JobProgress),
    /// Finished: the link to download.
    Done(String),
}
pub(super) fn status(body: &[u8], id: &str, key: &Secret) -> Result<Status, ProviderError> {
    let data = data(body)?;
    if data.get("task_id").and_then(Value::as_str) != Some(id) {
        return Err(malformed("Tripo status is for another task"));
    }
    let failed = |kind, message: &'static str| Ok(Status::Progress(JobProgress::Failed(ProviderError::new(kind, message))));
    match data.get("status").and_then(Value::as_str).ok_or_else(|| malformed("Tripo status missing"))? {
        "queued" => {
            let position = data.get("queuing_num").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
            Ok(Status::Progress(JobProgress::Queued { position }))
        }
        "running" => {
            let fraction = data.get("progress").and_then(Value::as_f64).filter(|p| p.is_finite()).map(|p| (p.clamp(0.0, 100.0) / 100.0) as f32);
            Ok(Status::Progress(JobProgress::Running { fraction, message: None, partial: None }))
        }
        "success" => {
            let output = data.get("output").ok_or_else(|| malformed("Tripo finished without output"))?;
            // The PBR model when there is one, else the textured model, else the bare shape.
            let url = ["pbr_model", "model", "base_model"]
                .iter()
                .find_map(|name| output.get(name).and_then(Value::as_str).filter(|s| !s.is_empty()))
                .ok_or_else(|| malformed("Tripo finished without a model"))?;
            if url.contains(key.expose()) {
                return Err(malformed("Tripo result link refused"));
            }
            Ok(Status::Done(crate::model_file::public_link(url, "Tripo")?))
        }
        "banned" => failed(ErrorKind::BlockedContent, "Tripo refused the input under its content policy"),
        "cancelled" => failed(ErrorKind::Cancelled, "the Tripo task was cancelled"),
        "expired" => failed(ErrorKind::JobFailed, "the Tripo task expired before it finished"),
        "failed" | "unknown" => failed(ErrorKind::JobFailed, "Tripo couldn't make the model"),
        _ => Err(malformed("Tripo reported an unknown status")),
    }
}
pub(super) fn glb(bytes: &[u8]) -> Result<(), ProviderError> {
    crate::model_file::glb(bytes, "Tripo")
}

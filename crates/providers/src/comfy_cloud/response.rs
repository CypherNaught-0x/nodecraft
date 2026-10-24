//! Comfy Cloud's replies: the `{error: {code, message}}` envelope, assets, jobs and their outputs.
use super::*;
use crate::secrets::Secret;
use serde::Deserialize;
use serde_json::Value;

const MAX_ID: usize = 128;

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    error: Option<ErrorBody>,
    /// Some refusals (a workflow that isn't an object) carry a bare `{message}` instead.
    #[serde(default)]
    message: Option<String>,
}
#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Comfy's own message, cleaned and shortened, unless it echoes the key.
fn said(message: Option<&str>, key: &Secret) -> String {
    let text = message.unwrap_or_default();
    if text.contains(key.expose()) {
        return "(message withheld)".into();
    }
    text.chars().filter(|c| !c.is_control()).take(300).collect()
}

fn kind(code: &str, status: u16) -> ErrorKind {
    match code {
        "insufficient_credits" => ErrorKind::Budget,
        "queue_full" => ErrorKind::Busy,
        "rate_limited" => ErrorKind::RateLimited,
        "unauthorized" | "forbidden" | "sso_required" => ErrorKind::Auth,
        "not_found" | "blob_not_found" => ErrorKind::NotFound,
        "content_blocked" | "input_blocked" => ErrorKind::BlockedContent,
        "upload_timed_out" => ErrorKind::Timeout,
        // Not transient: the first request with this key was taken, and Comfy never runs it again.
        "idempotency_key_reuse" => ErrorKind::JobFailed,
        "invalid_workflow" | "workflow_format_ui" | "missing_asset" | "hash_mismatch" | "invalid_body" | "invalid_request" => ErrorKind::UnsupportedInput,
        // Unavailable for legal reasons: Comfy's moderation, whatever the body says.
        _ if status == 451 => ErrorKind::BlockedContent,
        _ => ProviderError::from_status(status, b"").kind,
    }
}

/// Whether a job create was answered with one of Comfy's documented refusals that create no job
/// and release the idempotency key (`POST /api/v2/jobs`, #631). Each code counts only with its
/// documented status, so a 5xx or an unparsed body stays possibly billed.
pub(super) fn refused_create(r: &HttpResponse) -> bool {
    let Ok(envelope) = serde_json::from_slice::<Envelope>(&r.body) else { return false };
    let code = envelope.error.as_ref().and_then(|e| e.code.as_deref());
    matches!((r.status, code), (429, Some("queue_full" | "rate_limited")) | (402, Some("insufficient_credits")) | (422, Some("invalid_workflow")))
}

/// A failing status becomes an error carrying Comfy's code, its message and any `Retry-After`.
pub(super) fn check(r: &HttpResponse, key: &Secret) -> Result<(), ProviderError> {
    if r.is_success() {
        return Ok(());
    }
    let envelope = serde_json::from_slice::<Envelope>(&r.body).ok();
    let error = envelope.as_ref().and_then(|e| e.error.as_ref());
    let code: String = error.and_then(|e| e.code.as_deref()).unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(64).collect();
    let message = said(error.and_then(|e| e.message.as_deref()).or_else(|| envelope.as_ref().and_then(|e| e.message.as_deref())), key);
    let mut out = if code == "idempotency_key_reuse" {
        ProviderError::new(
            ErrorKind::JobFailed,
            "Comfy Cloud already took a submission of this run, whose reply was lost; it may be running there. NodeCraft won't submit it twice: check your Comfy Cloud queue, or run the node again.",
        )
    } else {
        let label = if code.is_empty() { String::new() } else { format!(", {code}") };
        ProviderError::new(kind(&code, r.status), format!("Comfy Cloud refused the request (HTTP {}{label}): {message}", r.status))
    };
    out.retry_after_ms = r
        .header("retry-after")
        .filter(|v| v.len() <= 16)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .and_then(|s| s.checked_mul(1000))
        .filter(|ms| *ms <= 3_600_000);
    Err(out)
}

/// Comfy's ids are UUIDs; anything else is refused before it goes into a URL.
pub(super) fn check_id(id: &str) -> Result<(), ProviderError> {
    if id.is_empty() || id.len() > MAX_ID || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(malformed("Comfy Cloud id refused"));
    }
    Ok(())
}

#[derive(Deserialize)]
struct Asset {
    id: String,
}

/// The stored asset's id, from a 201 (stored) or 200 (already stored) upload reply.
pub(super) fn asset(body: &[u8]) -> Result<String, ProviderError> {
    let asset: Asset = serde_json::from_slice(body).map_err(|_| malformed("Comfy Cloud's upload reply isn't the expected JSON"))?;
    check_id(&asset.id)?;
    Ok(asset.id)
}

#[derive(Deserialize)]
pub(super) struct Job {
    pub id: String,
    status: String,
    #[serde(default)]
    queue_position: Option<Value>,
    #[serde(default)]
    progress: Option<Progress>,
    #[serde(default)]
    outputs: Option<Vec<JobOutput>>,
    #[serde(default)]
    error: Option<JobError>,
}

#[derive(Deserialize)]
struct Progress {
    #[serde(default)]
    value: Option<f64>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
struct JobError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    class_type: Option<String>,
}

/// One saved file of a job.
#[derive(Deserialize)]
pub(super) struct JobOutput {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    /// The file's size as Comfy reports it, so an oversized output isn't downloaded at all.
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

pub(super) enum State {
    Progress(JobProgress),
    Failed(ProviderError),
    Succeeded(Vec<JobOutput>),
}

fn clean(text: Option<&str>, max: usize) -> String {
    text.unwrap_or_default().chars().filter(|c| !c.is_control()).take(max).collect()
}

/// A job reply, with its id checked.
pub(super) fn job(body: &[u8]) -> Result<Job, ProviderError> {
    let job: Job = serde_json::from_slice(body).map_err(|_| malformed("Comfy Cloud's job reply isn't the expected JSON"))?;
    check_id(&job.id)?;
    Ok(job)
}

impl Job {
    pub(super) fn state(self) -> Result<State, ProviderError> {
        Ok(match self.status.as_str() {
            "queued" => {
                State::Progress(JobProgress::Queued { position: self.queue_position.as_ref().and_then(Value::as_u64).and_then(|p| u32::try_from(p).ok()) })
            }
            "running" | "canceling" => {
                let progress = self.progress.as_ref();
                let fraction = progress.and_then(|p| p.value).filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0) as f32);
                let message = progress.map(|p| clean(p.message.as_deref(), 200)).filter(|m| !m.is_empty());
                State::Progress(JobProgress::Running { fraction, message, partial: None })
            }
            "succeeded" => State::Succeeded(self.outputs.unwrap_or_default()),
            "canceled" => State::Failed(ProviderError::new(ErrorKind::Cancelled, "the job was canceled on Comfy Cloud")),
            "expired" => State::Failed(ProviderError::new(ErrorKind::Expired, "the job expired on Comfy Cloud before it finished")),
            "failed" => {
                let e = self.error.as_ref();
                let at = match (e.and_then(|e| e.node_id.as_deref()), e.and_then(|e| e.class_type.as_deref())) {
                    (Some(node), Some(class)) => format!(" at node {} ({})", clean(Some(node), 32), clean(Some(class), 80)),
                    (Some(node), None) => format!(" at node {}", clean(Some(node), 32)),
                    _ => String::new(),
                };
                let message = clean(e.and_then(|e| e.message.as_deref()), 300);
                let kind = match e.and_then(|e| e.code.as_deref()) {
                    Some("insufficient_credits") => ErrorKind::Budget,
                    Some("content_blocked") => ErrorKind::BlockedContent,
                    _ => ErrorKind::JobFailed,
                };
                State::Failed(ProviderError::new(kind, format!("the workflow failed on Comfy Cloud{at}: {message}")))
            }
            other => return Err(malformed(format!("Comfy Cloud reported an unknown job status {:?}", clean(Some(other), 32)))),
        })
    }
}

/// The signed storage URL an asset's content redirects to: https only, and never one carrying the key.
pub(super) fn signed_location(r: &HttpResponse, key: &Secret) -> Result<String, ProviderError> {
    let location = r.header("location").ok_or_else(|| malformed("Comfy Cloud's download redirect has no location"))?;
    if !location.starts_with("https://") || location.len() > 8192 || location.contains(key.expose()) {
        return Err(malformed("Comfy Cloud's download redirect was refused"));
    }
    Ok(location.to_owned())
}

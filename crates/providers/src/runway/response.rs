use super::*;
use crate::Usage;
use serde::Deserialize;

#[derive(Deserialize)]
struct Receipt {
    id: String,
}
pub(super) fn created(bytes: &[u8], key: &Secret) -> Result<String, ProviderError> {
    let receipt: Receipt =
        serde_json::from_slice(bytes).map_err(|_| malformed("Runway create reply is unreadable; a task may have been created and billed"))?;
    task_id(&receipt.id, key)?;
    Ok(receipt.id)
}
pub(super) fn check(reply: &HttpResponse, limits: Limits) -> Result<(), ProviderError> {
    if reply.body.len() > limits.response_bytes {
        return Err(malformed("Runway reply exceeds its size limit"));
    }
    if reply.is_success() {
        return Ok(());
    }
    let mut error = ProviderError::from_status(reply.status, b"");
    error.message = format!("Runway request failed (HTTP {}); an accepted task may still run and be billed", reply.status);
    error.retry_after_ms =
        reply.header("retry-after").filter(|s| s.len() <= 10).and_then(|s| s.parse::<u64>().ok()).and_then(|v| v.checked_mul(1000)).filter(|v| *v <= 3_600_000);
    Err(error)
}
#[derive(Deserialize)]
struct Cost {
    credits: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Task {
    id: String,
    status: String,
    #[serde(default)]
    progress: Option<f64>,
    #[serde(default)]
    output: Option<Vec<String>>,
    #[serde(default)]
    failure_code: Option<String>,
    #[serde(default)]
    cost: Option<Cost>,
}
pub(super) enum Status {
    Progress(JobProgress),
    Done { url: String, usage: Usage },
}
fn link(raw: &str, key: &Secret) -> Result<String, ProviderError> {
    if raw.len() > 8192 || raw.contains(key.expose()) {
        return Err(malformed("Runway result link refused"));
    }
    let url = url::Url::parse(raw).map_err(|_| malformed("Runway result link refused"))?;
    let allowed = url.host_str().is_some_and(|h| h.ends_with(".cloudfront.net") && h.len() > ".cloudfront.net".len());
    if url.scheme() != "https" || !allowed || !url.username().is_empty() || url.password().is_some() || url.port().is_some() || url.fragment().is_some() {
        return Err(malformed("Runway output must be a public CloudFront HTTPS link without credentials or redirects"));
    }
    Ok(raw.into())
}
pub(super) fn status(bytes: &[u8], expected: &str, key: &Secret) -> Result<Status, ProviderError> {
    let task: Task = serde_json::from_slice(bytes).map_err(|_| malformed("Runway task reply is unreadable"))?;
    if task.id != expected {
        return Err(malformed("Runway reply belongs to another task"));
    }
    match task.status.as_str() {
        "PENDING" | "THROTTLED" => Ok(Status::Progress(JobProgress::Queued { position: None })),
        "RUNNING" => {
            let progress =
                task.progress.filter(|n| n.is_finite() && (0.0..=1.0).contains(n)).ok_or_else(|| malformed("Runway progress must be between zero and one"))?;
            Ok(Status::Progress(JobProgress::Running { fraction: Some(progress as f32), message: Some("Runway is making the video".into()), partial: None }))
        }
        "SUCCEEDED" => {
            let urls = task.output.filter(|v| v.len() == 1).ok_or_else(|| malformed("Runway must return exactly one MP4 output"))?;
            let raw = urls.first().ok_or_else(|| malformed("Runway returned no video"))?;
            // The API permits signed credit adjustments. Money cannot represent a refund or
            // an overflowing conversion; keep usage unknown without discarding a paid output.
            let cost = task
                .cost
                .and_then(|c| u64::try_from(c.credits).ok())
                .and_then(|n| n.checked_mul(10_000))
                .map(|micros| nodecraft_types::Money::new(nodecraft_types::Currency::USD, micros));
            Ok(Status::Done { url: link(raw, key)?, usage: Usage { cost, ..Usage::default() } })
        }
        "FAILED" => {
            let code = task.failure_code.as_deref().unwrap_or_default();
            let kind = if code.starts_with("SAFETY.") || code == "INPUT_PREPROCESSING.SAFETY.TEXT" {
                ErrorKind::BlockedContent
            } else if code == "ASSET.INVALID" {
                ErrorKind::UnsupportedInput
            } else {
                ErrorKind::JobFailed
            };
            Ok(Status::Progress(JobProgress::Failed(ProviderError::new(
                kind,
                "Runway could not generate the video; input safety refusals may still be billed",
            ))))
        }
        "CANCELLED" => Ok(Status::Progress(JobProgress::Failed(ProviderError::new(ErrorKind::Cancelled, "Runway task was cancelled")))),
        _ => Err(malformed("Runway reported an unknown task state")),
    }
}
pub(super) fn video(bytes: Vec<u8>, limits: Limits) -> Result<Arc<[u8]>, ProviderError> {
    if bytes.len() > limits.output_bytes || nodecraft_media::sniff(&bytes).map(|s| s.mime) != Some("video/mp4") {
        return Err(malformed("Runway output is not a bounded MP4 video"));
    }
    let meta = nodecraft_media::probe_movie(&bytes).map_err(|_| malformed("Runway MP4 headers are damaged"))?;
    if !meta.width.is_some_and(|n| n > 0 && n <= 4096)
        || !meta.height.is_some_and(|n| n > 0 && n <= 4096)
        || !meta.duration_ms.is_some_and(|n| n > 0 && n <= 60_000)
    {
        return Err(malformed("Runway MP4 geometry or duration exceeds its limit"));
    }
    Ok(Arc::from(bytes))
}

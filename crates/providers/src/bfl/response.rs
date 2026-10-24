use super::*;
use serde::Deserialize;
use url::Url;

#[derive(Deserialize)]
pub(super) struct Receipt {
    pub id: String,
    pub polling_url: String,
}
#[derive(Deserialize)]
struct StatusReply {
    id: String,
    status: String,
    result: Option<Box<serde_json::value::RawValue>>,
}
#[derive(Deserialize)]
struct ImageResult {
    sample: String,
}
pub(super) enum Status {
    Progress(JobProgress),
    Ready(String),
}

pub(super) fn task_id(id: &str, key: &Secret) -> Result<(), ProviderError> {
    if id.is_empty() || id.len() > 128 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') || id.contains(key.expose()) {
        return Err(malformed("Black Forest Labs task identifier refused"));
    }
    Ok(())
}
fn url(value: &str, key: &Secret) -> Result<Url, ProviderError> {
    if value.len() > 4096 || value.contains(key.expose()) || value.bytes().any(|b| b.is_ascii_control() || b == b'\\') {
        return Err(malformed("Black Forest Labs URL refused"));
    }
    let url = Url::parse(value).map_err(|_| malformed("Black Forest Labs URL is invalid"))?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || url.port().is_some() || url.fragment().is_some() {
        return Err(malformed("Black Forest Labs URL origin refused"));
    }
    Ok(url)
}
// BFL documents distributed cluster endpoints. A single DNS label may vary; the
// exact BFL-owned suffix and service prefix remain mandatory, with redirects disabled.
fn cluster(host: &str, service: &str) -> bool {
    host.strip_prefix(service).and_then(|s| s.strip_prefix('.')).and_then(|s| s.strip_suffix(".bfl.ai")).is_some_and(|region| {
        !region.is_empty()
            && region.len() <= 63
            && !region.starts_with('-')
            && !region.ends_with('-')
            && region.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}
pub(super) fn poll_url(value: &str, id: &str, key: &Secret) -> Result<(), ProviderError> {
    let parsed = url(value, key)?;
    let host = parsed.host_str().unwrap_or_default();
    let mut query = parsed.query_pairs();
    if !(host == "api.bfl.ai" || cluster(host, "api"))
        || parsed.path() != "/v1/get_result"
        || !query.next().is_some_and(|(name, value)| name == "id" && value == id)
        || query.next().is_some()
    {
        return Err(malformed("Black Forest Labs polling URL does not identify this task on a trusted API cluster"));
    }
    Ok(())
}
fn delivery_url(value: &str, key: &Secret) -> Result<(), ProviderError> {
    let parsed = url(value, key)?;
    if !cluster(parsed.host_str().unwrap_or_default(), "delivery") || parsed.path() == "/" {
        return Err(malformed("Black Forest Labs image URL is outside its delivery service"));
    }
    Ok(())
}
pub(super) fn check(reply: &HttpResponse) -> Result<(), ProviderError> {
    if reply.body.len() > METADATA_BYTES {
        return Err(malformed("Black Forest Labs metadata exceeds 64 KiB"));
    }
    if reply.status == 200 {
        return Ok(());
    }
    let mut error = ProviderError::new(
        ProviderError::from_status(reply.status, b"").kind,
        "Black Forest Labs refused the request; inspect the API key, credits and input bounds",
    );
    error.retry_after_ms = reply
        .header("retry-after")
        .filter(|s| s.len() <= 10)
        .and_then(|s| s.parse::<u64>().ok())
        .and_then(|s| s.checked_mul(1000))
        .filter(|ms| *ms <= 3_600_000);
    Err(error)
}
pub(super) fn status(body: &[u8], id: &str, key: &Secret) -> Result<Status, ProviderError> {
    let reply: StatusReply = serde_json::from_slice(body).map_err(|_| malformed("Black Forest Labs task status is invalid"))?;
    if reply.id != id {
        return Err(malformed("Black Forest Labs returned another task's status"));
    }
    let failed = |kind, message| Ok(Status::Progress(JobProgress::Failed(ProviderError::new(kind, message))));
    match reply.status.as_str() {
        "Pending" => Ok(Status::Progress(JobProgress::Queued { position: None })),
        "Reasoning" | "Generating" => Ok(Status::Progress(JobProgress::Running { fraction: None, message: None, partial: None })),
        "Ready" => {
            let result = reply.result.ok_or_else(|| malformed("Black Forest Labs finished without an image"))?;
            let result: ImageResult = serde_json::from_str(result.get()).map_err(|_| malformed("Black Forest Labs image result is invalid"))?;
            delivery_url(&result.sample, key)?;
            Ok(Status::Ready(result.sample))
        }
        "Request Moderated" | "Content Moderated" => failed(ErrorKind::BlockedContent, "Black Forest Labs declined the generation under its content policy"),
        "Task not found" => failed(ErrorKind::NotFound, "Black Forest Labs no longer has this task"),
        "No Card Generated" | "Error" | "Failed" => failed(ErrorKind::JobFailed, "Black Forest Labs could not generate the image"),
        _ => Err(malformed("Black Forest Labs task status is unknown")),
    }
}

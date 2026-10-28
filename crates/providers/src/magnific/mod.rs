//! Magnific Precision V2 (#38), using the current native API and shared task driver.
//! No documented submit deduplication or remote cancellation; ambiguous paid creates never replay.
mod limits;
mod media;
mod request;
mod response;
#[cfg(test)]
mod tests;
use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpMethod, HttpRequest, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission,
    SubmitContext,
};
pub use limits::Limits;
use std::sync::Arc;

/// One photo-flavor image enhancement; no invented prompt, seed or output-format fields.
pub const PRECISION_V2: &str = "image-upscaler-precision-v2";
const ROOT: &str = "https://api.magnific.com/v1/ai/image-upscaler-precision-v2";
fn invalid(message: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, message)
}
fn malformed(message: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, message)
}
fn transport(error: ProviderError) -> ProviderError {
    ProviderError {
        kind: error.kind,
        message: "Magnific transport failed; an accepted request may still be running or billed".into(),
        retry_after_ms: error.retry_after_ms.filter(|n| *n <= 3_600_000),
    }
}
fn identity(id: &str) -> Result<(), ProviderError> {
    if id.len() != 36 || id.bytes().enumerate().any(|(i, byte)| if matches!(i, 8 | 13 | 18 | 23) { byte != b'-' } else { !byte.is_ascii_hexdigit() }) {
        return Err(malformed("Magnific task identity must be a UUID"));
    }
    Ok(())
}
#[derive(Clone, Copy)]
struct Contract {
    width: u32,
    height: u32,
    limits: Limits,
}
impl Contract {
    fn validate(self) -> Result<(), ProviderError> {
        let limits = self.limits.validate()?;
        let pixels = u64::from(self.width).checked_mul(u64::from(self.height)).ok_or_else(|| invalid("Magnific geometry overflow"))?;
        // PNG/JPEG/WebP can decode to RGBA16 (8 bytes/pixel). The media decoder
        // reserves one more full raster when EXIF orientation needs a transform.
        // Admit both before buying an output, including with lowered host limits.
        let decoded_bytes = pixels.checked_mul(16).ok_or_else(|| invalid("Magnific decoded output extent overflow"))?;
        if self.width == 0
            || self.height == 0
            || self.width > limits.output_width
            || self.height > limits.output_height
            || pixels > limits.total_pixels
            || limits.output_bytes == 0
            || limits.response_bytes == 0
            || decoded_bytes > limits.codec_bytes
        {
            return Err(invalid("Magnific output exceeds this host's image limits"));
        }
        Ok(())
    }
    fn handle(self, id: &str) -> JobHandle {
        JobHandle { remote_id: id.into(), data: serde_json::json!({"v":1,"model":PRECISION_V2,"width":self.width,"height":self.height,"limits":self.limits}) }
    }
    fn read(handle: &JobHandle, host: Limits) -> Result<Self, ProviderError> {
        identity(&handle.remote_id)?;
        let object = handle.data.as_object().filter(|object| object.len() == 5).ok_or_else(|| invalid("Magnific handle record refused"))?;
        if object.get("v").and_then(|value| value.as_u64()) != Some(1) || object.get("model").and_then(|value| value.as_str()) != Some(PRECISION_V2) {
            return Err(invalid("Magnific handle model or version refused"));
        }
        let dimension = |name| {
            object.get(name).and_then(|value| value.as_u64()).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| invalid("Magnific handle geometry refused"))
        };
        // Borrow the caller's value; never clone a potentially hostile nested handle.
        let recorded: Limits = serde::Deserialize::deserialize(object.get("limits").ok_or_else(|| invalid("Magnific handle limits missing"))?)
            .map_err(|_| invalid("Magnific handle limits refused"))?;
        recorded.validate()?;
        let contract = Self { width: dimension("width")?, height: dimension("height")?, limits: recorded.intersect(host) };
        contract.validate()?;
        Ok(contract)
    }
}

pub struct Magnific {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl Magnific {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::default() }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { key, http, limits: limits.validate()? })
    }
    fn authenticated(&self, request: HttpRequest) -> HttpRequest {
        request.secret_header("x-magnific-api-key", self.key.expose())
    }
    async fn create(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Submission, ProviderError> {
        let (contract, body) = request::prepare(job, self.limits)?;
        let request = self.authenticated(HttpRequest {
            method: HttpMethod::Post,
            body,
            ..HttpRequest::get(ROOT).header("Content-Type", "application/json").max_response_bytes(self.limits.response_bytes)
        });
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.http.send_no_redirect(request).await.map_err(transport)?;
        if response::refusal(&reply, self.limits)
            && let Some(context) = context
        {
            context.refused();
        }
        response::status(&reply, self.limits.response_bytes)?;
        let id = response::receipt(&reply.body, &self.key)?;
        // Result metadata is validated on a canonical poll, never discarded with a paid UUID.
        Ok(Submission::Queued(contract.handle(&id)))
    }
    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        if handle.remote_id.contains(self.key.expose()) {
            return Err(malformed("Magnific task identity refused or redacted"));
        }
        let contract = Contract::read(handle, self.limits)?;
        let reply = self
            .http
            .send_no_redirect(self.authenticated(HttpRequest::get(format!("{ROOT}/{}", handle.remote_id)).max_response_bytes(contract.limits.response_bytes)))
            .await
            .map_err(transport)?;
        response::status(&reply, contract.limits.response_bytes)?;
        let task = response::task(&reply.body, &self.key)?;
        if task.id != handle.remote_id {
            return Err(malformed("Magnific polled task identity changed"));
        }
        match task.state {
            response::State::Created => Ok(JobProgress::Queued { position: None }),
            response::State::Running => Ok(JobProgress::Running { fraction: None, message: None, partial: None }),
            response::State::Failed => {
                Ok(JobProgress::Failed(ProviderError::new(ErrorKind::JobFailed, "Magnific reported failure; the accepted request may have been billed")))
            }
            response::State::Complete => {
                let url = task.url.ok_or_else(|| malformed("Magnific complete task has no image"))?;
                // The URL is validated before this key-free request. No response-supplied task
                // routes, redirects, or authenticated CDN downloads are supported.
                let image =
                    self.http.send_no_redirect(HttpRequest::get(url.as_ref()).max_response_bytes(contract.limits.output_bytes)).await.map_err(transport)?;
                if matches!(image.status, 403 | 404 | 410) {
                    response::bounds(&image, contract.limits.output_bytes)?;
                    return Err(ProviderError::new(
                        ErrorKind::Expired,
                        "Magnific image is unavailable or its 30-minute download link expired; the accepted task may have been billed",
                    ));
                }
                response::status(&image, contract.limits.output_bytes)?;
                let mime = media::validate(&image.body, contract)?;
                Ok(JobProgress::Done(Completed::new(vec![Output::Bytes { mime: mime.into(), bytes: image.body.into() }])))
            }
        }
    }
}
impl Provider for Magnific {
    fn id(&self) -> &str {
        "magnific"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move { self.create(job, None).await })
    }
    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.arm();
        Box::pin(async move { self.create(job, Some(context)).await })
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async move { self.progress(handle).await })
    }
    fn cancel<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            Contract::read(handle, self.limits)?;
            Err(invalid("Magnific does not document remote cancellation; this accepted task may continue running and billing"))
        })
    }
}

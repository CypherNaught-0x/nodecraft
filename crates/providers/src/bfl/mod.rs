//! Black Forest Labs' direct FLUX queue, with resumable cluster polling.
mod request;
mod response;
#[cfg(test)]
mod tests;

use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission,
};
use std::sync::Arc;

pub const MODEL: &str = "flux-2-klein-4b";
const API: &str = "https://api.bfl.ai/v1/flux-2-klein-4b";
const METADATA_BYTES: usize = 64 * 1024;
const OUTPUT_BYTES: usize = 32 * 1024 * 1024;
const PIXELS: u64 = 4_000_000;

fn invalid(message: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, message)
}
fn malformed(message: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, message)
}
fn transport(error: ProviderError) -> ProviderError {
    ProviderError::new(error.kind, "Black Forest Labs request failed; a submitted generation may still run and be billed")
}

/// Fixed model and origin. Credentials never enter a saved polling handle or output request.
pub struct Bfl {
    key: Secret,
    http: Arc<dyn Http>,
}
impl Bfl {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http }
    }
    fn auth(&self, request: HttpRequest) -> Result<HttpRequest, ProviderError> {
        if self.key.expose().len() > 256 {
            return Err(invalid("Black Forest Labs API key exceeds 256 bytes"));
        }
        Ok(request.secret_header("x-key", self.key.expose()).header("Accept", "application/json").max_response_bytes(METADATA_BYTES).timeout_ms(60_000))
    }
    async fn create(&self, job: &ProviderJob, context: Option<&crate::SubmitContext>) -> Result<Submission, ProviderError> {
        let (body, width, height) = request::body(job)?;
        let request = self.auth(HttpRequest::post_json(API, &body)?)?;
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.http.send_no_redirect(request).await.map_err(transport)?;
        response::check(&reply)?;
        let receipt: response::Receipt = serde_json::from_slice(&reply.body).map_err(|_| malformed("Black Forest Labs returned an invalid task receipt"))?;
        response::task_id(&receipt.id, &self.key)?;
        response::poll_url(&receipt.polling_url, &receipt.id, &self.key)?;
        Ok(Submission::Queued(JobHandle {
            remote_id: receipt.id,
            data: serde_json::json!({"v":1,"polling_url":receipt.polling_url,"width":width,"height":height}),
        }))
    }
    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        response::task_id(&handle.remote_id, &self.key)?;
        let data = handle.data.as_object().filter(|data| data.len() == 4).ok_or_else(|| invalid("Black Forest Labs handle is invalid"))?;
        let url = data.get("polling_url").and_then(serde_json::Value::as_str).ok_or_else(|| invalid("Black Forest Labs handle has no polling URL"))?;
        let width = data.get("width").and_then(serde_json::Value::as_u64).ok_or_else(|| invalid("Black Forest Labs handle has no width"))?;
        let height = data.get("height").and_then(serde_json::Value::as_u64).ok_or_else(|| invalid("Black Forest Labs handle has no height"))?;
        if data.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
            return Err(invalid("Black Forest Labs handle version is unsupported"));
        }
        request::geometry(width, height)?;
        response::poll_url(url, &handle.remote_id, &self.key)?;
        let reply = self.http.send_no_redirect(self.auth(HttpRequest::get(url))?).await.map_err(transport)?;
        response::check(&reply)?;
        match response::status(&reply.body, &handle.remote_id, &self.key)? {
            response::Status::Progress(progress) => Ok(progress),
            response::Status::Ready(url) => {
                drop(reply);
                // The short-lived signed delivery URL is downloaded immediately, without x-key.
                let reply = self.http.send_no_redirect(HttpRequest::get(url).max_response_bytes(OUTPUT_BYTES).timeout_ms(120_000)).await.map_err(transport)?;
                if !reply.is_success() {
                    return Err(ProviderError::new(ErrorKind::Server, "Black Forest Labs image download failed; polling again obtains a fresh delivery URL"));
                }
                if reply.body.len() > OUTPUT_BYTES {
                    return Err(malformed("Black Forest Labs image exceeds 32 MiB"));
                }
                let limits = nodecraft_media::Limits { max_input_bytes: OUTPUT_BYTES as u64, max_width: 2048, max_height: 2048, max_alloc: 64 * 1024 * 1024 };
                let meta = nodecraft_media::probe_image(&reply.body, &limits).map_err(|_| malformed("Black Forest Labs output is not a bounded PNG image"))?;
                if meta.mime != "image/png" || u64::from(meta.width) != width || u64::from(meta.height) != height {
                    return Err(malformed("Black Forest Labs output does not match the requested PNG dimensions"));
                }
                let _ = nodecraft_media::decode_image(&reply.body, &limits).map_err(|_| malformed("Black Forest Labs output image is corrupt"))?;
                Ok(JobProgress::Done(Completed::new(vec![Output::Bytes { mime: "image/png".into(), bytes: Arc::from(reply.body) }])))
            }
        }
    }
}
impl Provider for Bfl {
    fn id(&self) -> &str {
        "bfl"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(self.create(job, None))
    }
    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a crate::SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.arm();
        Box::pin(self.create(job, Some(context)))
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(self.progress(handle))
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("Black Forest Labs has no supported task cancellation endpoint; the generation may finish and be billed")) })
    }
    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        1_000
    }
}

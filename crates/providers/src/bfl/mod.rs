//! Black Forest Labs' direct FLUX queue, with resumable cluster polling: FLUX.2 text-to-image
//! (klein 4B and 9B, pro, max, flex) and FLUX.1 Kontext image editing (pro, max; #1060).
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
/// The one origin requests go to; each model's path is fixed in [`MODELS`].
const ORIGIN: &str = "https://api.bfl.ai";

/// A direct model: its catalog `provider_model`, its API path, and whether it edits one image.
pub(super) struct Spec {
    pub model: &'static str,
    path: &'static str,
    pub edit: bool,
}

/// Every direct model this adapter sends, from BFL's OpenAPI spec (checked 2026-11-06).
const MODELS: &[Spec] = &[
    Spec { model: MODEL, path: "/v1/flux-2-klein-4b", edit: false },
    Spec { model: "flux-2-klein-9b", path: "/v1/flux-2-klein-9b", edit: false },
    Spec { model: "flux-2-pro", path: "/v1/flux-2-pro", edit: false },
    Spec { model: "flux-2-max", path: "/v1/flux-2-max", edit: false },
    Spec { model: "flux-2-flex", path: "/v1/flux-2-flex", edit: false },
    Spec { model: "flux-kontext-pro", path: "/v1/flux-kontext-pro", edit: true },
    Spec { model: "flux-kontext-max", path: "/v1/flux-kontext-max", edit: true },
];

pub(super) fn spec(model: &str) -> Option<&'static Spec> {
    MODELS.iter().find(|spec| spec.model == model)
}
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
        let spec = spec(&job.model).ok_or_else(|| invalid("Black Forest Labs has no such direct model here"))?;
        let (body, width, height) = request::body(job, spec)?;
        let request = self.auth(HttpRequest::post_json(format!("{ORIGIN}{}", spec.path), &body)?)?;
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
            // An edit's handle is version 2, so a saved text-to-image handle (version 1) can't
            // claim the size-free check an edit gets.
            data: serde_json::json!({"v":if spec.edit { 2 } else { 1 },"polling_url":receipt.polling_url,"width":width,"height":height}),
        }))
    }
    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        response::task_id(&handle.remote_id, &self.key)?;
        let data = handle.data.as_object().filter(|data| data.len() == 4).ok_or_else(|| invalid("Black Forest Labs handle is invalid"))?;
        let url = data.get("polling_url").and_then(serde_json::Value::as_str).ok_or_else(|| invalid("Black Forest Labs handle has no polling URL"))?;
        let width = data.get("width").and_then(serde_json::Value::as_u64).ok_or_else(|| invalid("Black Forest Labs handle has no width"))?;
        let height = data.get("height").and_then(serde_json::Value::as_u64).ok_or_else(|| invalid("Black Forest Labs handle has no height"))?;
        // Version 1 fixes the output's size; version 2 is an edit, whose size follows its input
        // image, so it fixes none (0 × 0).
        let any_size = match data.get("v").and_then(serde_json::Value::as_u64) {
            Some(1) => {
                request::geometry(width, height)?;
                false
            }
            Some(2) if width == 0 && height == 0 => true,
            _ => return Err(invalid("Black Forest Labs handle version is unsupported")),
        };
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
                let fits = if any_size {
                    u64::from(meta.width).checked_mul(u64::from(meta.height)).is_some_and(|pixels| pixels > 0 && pixels <= PIXELS)
                } else {
                    u64::from(meta.width) == width && u64::from(meta.height) == height
                };
                if meta.mime != "image/png" || !fits {
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

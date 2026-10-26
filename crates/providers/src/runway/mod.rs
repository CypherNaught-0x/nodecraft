//! Runway Gen-4.5 and Gen-4 Turbo video tasks (#25). One paid create, then resumable polling.
//! No documented server submission deduplication: uncertain creates are never replayed.
mod request;
mod response;
#[cfg(test)]
mod tests;

use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob,
    Submission, SubmitContext,
};
use std::sync::Arc;

pub const GEN4_5: &str = "gen4.5";
pub const GEN4_TURBO: &str = "gen4_turbo";
const API: &str = "https://api.dev.runwayml.com/v1";
const VERSION: &str = "2024-11-06";
const MIB: usize = 1024 * 1024;

/// Lower-only limits for one operation, excluding caller-owned inputs and transport internals.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub image_bytes: usize,
    pub response_bytes: usize,
    pub output_bytes: usize,
}
impl Limits {
    pub const HARD: Self = Self { image_bytes: 3 * MIB, response_bytes: 64 * 1024, output_bytes: 256 * MIB };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.image_bytes > h.image_bytes || self.response_bytes > h.response_bytes || self.output_bytes > h.output_bytes {
            return Err(invalid("Runway limits may only be lowered"));
        }
        Ok(self)
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self::HARD
    }
}
fn invalid(message: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, message)
}
fn malformed(message: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, message)
}
fn model(id: &str) -> bool {
    matches!(id, GEN4_5 | GEN4_TURBO)
}
fn task_id(id: &str, key: &Secret) -> Result<(), ProviderError> {
    let valid = id.len() == 36
        && id.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else if i == 14 {
                c == b'4'
            } else if i == 19 {
                matches!(c, b'8' | b'9' | b'a' | b'b' | b'A' | b'B')
            } else {
                c.is_ascii_hexdigit()
            }
        });
    if !valid || id.contains(key.expose()) {
        return Err(malformed("Runway task identifier refused"));
    }
    Ok(())
}
fn transport(error: ProviderError) -> ProviderError {
    ProviderError {
        kind: error.kind,
        message: "Runway transport failed; an accepted task may still run and be billed".into(),
        retry_after_ms: error.retry_after_ms.filter(|v| *v <= 3_600_000),
    }
}

/// Fixed-origin adapter. Construction reads no keychain and sends nothing.
pub struct Runway {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl Runway {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { key, http, limits: limits.validate()? })
    }
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        let request = request
            .header("User-Agent", crate::USER_AGENT)
            .header("X-Runway-Version", VERSION)
            .secret_header("Authorization", format!("Bearer {}", self.key.expose()))
            .max_response_bytes(self.limits.response_bytes);
        self.http.send_no_redirect(request).await.map_err(transport)
    }
    async fn create(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Submission, ProviderError> {
        let prepared = request::prepare(job, self.limits)?;
        let body = serde_json::to_vec(&prepared.body).map_err(|_| invalid("Runway request encoding failed"))?;
        let request = HttpRequest { method: HttpMethod::Post, body, ..HttpRequest::get(format!("{API}/{}", prepared.endpoint)) }
            .header("Content-Type", "application/json")
            .timeout_ms(120_000);
        let duration = prepared.body.duration;
        drop(prepared);
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.send(request).await?;
        response::check(&reply, self.limits)?;
        let id = response::created(&reply.body, &self.key)?;
        Ok(Submission::Queued(JobHandle { remote_id: id, data: serde_json::json!({"v":1,"model":job.model,"duration":duration}) }))
    }
    fn handle(&self, handle: &JobHandle) -> Result<(), ProviderError> {
        task_id(&handle.remote_id, &self.key)?;
        let data = handle.data.as_object().filter(|d| d.len() == 3).ok_or_else(|| invalid("Runway handle record refused"))?;
        if data.get("v").and_then(|v| v.as_u64()) != Some(1)
            || !data.get("model").and_then(|v| v.as_str()).is_some_and(model)
            || !data.get("duration").and_then(|v| v.as_u64()).is_some_and(|v| (2..=10).contains(&v))
        {
            return Err(invalid("Runway handle record refused"));
        }
        Ok(())
    }
    async fn task(&self, handle: &JobHandle) -> Result<response::Status, ProviderError> {
        self.handle(handle)?;
        let reply = self.send(HttpRequest::get(format!("{API}/tasks/{}", handle.remote_id)).timeout_ms(30_000)).await?;
        response::check(&reply, self.limits)?;
        response::status(&reply.body, &handle.remote_id, &self.key)
    }
    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        match self.task(handle).await? {
            response::Status::Progress(progress) => Ok(progress),
            response::Status::Done { url, usage } => {
                // The API key and version header never go to the pre-signed storage URL.
                let request = HttpRequest::get(url).header("User-Agent", crate::USER_AGENT).max_response_bytes(self.limits.output_bytes).timeout_ms(300_000);
                let reply = self.http.send_no_redirect(request).await.map_err(transport)?;
                if !reply.is_success() {
                    return Err(ProviderError::new(ErrorKind::Server, "Runway video download failed; resume the task to fetch a fresh link"));
                }
                let bytes = response::video(reply.body, self.limits)?;
                Ok(JobProgress::Done(Completed { outputs: vec![Output::Bytes { mime: "video/mp4".into(), bytes }], usage }))
            }
        }
    }
    async fn stop(&self, handle: &JobHandle) -> Result<(), ProviderError> {
        // DELETE also erases completed output. Read first so an already completed video remains
        // retrievable; a completion racing this request is an unavoidable API limitation.
        let status = match self.task(handle).await {
            Err(error) if error.kind == ErrorKind::NotFound => return Ok(()),
            result => result?,
        };
        match status {
            response::Status::Done { .. } => return Err(invalid("Runway already completed this video; cancellation would delete its output")),
            response::Status::Progress(JobProgress::Failed(_)) => return Ok(()),
            response::Status::Progress(_) => {}
        }
        let request = HttpRequest { method: HttpMethod::Delete, ..HttpRequest::get(format!("{API}/tasks/{}", handle.remote_id)) }.timeout_ms(30_000);
        let reply = self.send(request).await?;
        if reply.body.len() <= self.limits.response_bytes && matches!(reply.status, 204 | 404) {
            return Ok(());
        }
        response::check(&reply, self.limits)?;
        Err(malformed("Runway did not confirm cancellation"))
    }
}
impl Provider for Runway {
    fn id(&self) -> &str {
        "runway"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(self.create(job, None))
    }
    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.arm();
        Box::pin(self.create(job, Some(context)))
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(self.progress(handle))
    }
    fn cancel<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(self.stop(handle))
    }
    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        5_000
    }
}

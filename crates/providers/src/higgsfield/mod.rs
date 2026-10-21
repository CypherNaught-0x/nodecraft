//! Higgsfield (#177): SOUL 2 text to image and Kling 2.5 Turbo Pro image to video on Higgsfield's
//! queue API. Inputs stage through Higgsfield's presigned uploads, which are free; the generation
//! submit is the paid step. It carries the job's idempotency key, and the exact body sent under a
//! key is kept for this process, so a retry after a lost reply replays it byte for byte and gets
//! the original request back instead of paying for a second one. Higgsfield keeps results for at
//! least seven days, so a finished request downloads at once.
mod request;
mod response;
#[cfg(test)]
mod tests;
use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob,
    Submission,
};
use nodecraft_media::ContentHash;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// SOUL 2 text to image. Higgsfield's SOUL 1 lives on another path; this is the v2 model.
pub const SOUL: &str = "higgsfield-ai/soul/v2/standard";
/// Kling 2.5 Turbo Pro: one image animated into a 5 or 10 second clip.
pub const KLING_I2V: &str = "kling-video/v2.5-turbo/pro/image-to-video";
const API: &str = "https://api.higgsfield.ai";
const MIB: usize = 1024 * 1024;
/// Bodies kept for exact replay; the oldest goes first.
const STAGED: usize = 32;

/// Lower-only payload limits for one job.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Higgsfield documents no prompt cap for these models; 10,000 characters is its largest
    /// documented one (Genjutsu).
    pub prompt_chars: usize,
    /// Each staged image. Higgsfield documents no upload cap.
    pub input_bytes: usize,
    /// JSON replies (receipts, status, upload grants, estimates).
    pub response_bytes: usize,
    /// Each downloaded result.
    pub output_bytes: usize,
    /// All results of one request together.
    pub total_output_bytes: usize,
}
impl Limits {
    pub const HARD: Self = Self { prompt_chars: 10_000, input_bytes: 20 * MIB, response_bytes: MIB, output_bytes: 256 * MIB, total_output_bytes: 512 * MIB };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.prompt_chars > h.prompt_chars
            || self.input_bytes > h.input_bytes
            || self.response_bytes > h.response_bytes
            || self.output_bytes > h.output_bytes
            || self.total_output_bytes > h.total_output_bytes
        {
            return Err(invalid("Higgsfield limits may only be lowered"));
        }
        Ok(self)
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self::HARD
    }
}
fn invalid(s: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, s)
}
fn malformed(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, s)
}
fn transport(e: ProviderError, paid: bool) -> ProviderError {
    let message = if paid { "Higgsfield transport failed; the request may have been accepted and billed" } else { "Higgsfield transport failed" };
    ProviderError { kind: e.kind, message: message.into(), retry_after_ms: e.retry_after_ms.filter(|n| *n <= 3_600_000) }
}
/// A request id as Higgsfield issues them (a UUID); anything else is refused before it reaches a URL.
fn request_id(s: &str) -> Result<(), ProviderError> {
    if s.is_empty() || s.len() > 64 || !s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err(malformed("Higgsfield request id refused"));
    }
    Ok(())
}

/// What Higgsfield says a request would cost this account, before it is sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Estimate {
    /// Higgsfield credits, as the decimal it returned.
    pub credits: String,
    pub usd: nodecraft_types::Money,
}

/// One staged body: what was sent, or is about to be sent, under an idempotency key.
struct Staged {
    key: String,
    /// The job it was staged for: a different job under the same key never reuses it.
    job: ContentHash,
    body: Arc<[u8]>,
}

/// Fixed-origin queue adapter. Constructing it reads no credential and sends nothing.
pub struct Higgsfield {
    /// The key ID and secret joined by a colon, as Higgsfield issues them.
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
    staged: Mutex<VecDeque<Staged>>,
}
impl Higgsfield {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD, staged: Mutex::new(VecDeque::new()) }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { limits: limits.validate()?, ..Self::new(key, http) })
    }
    /// Whether `s` holds the credential or either half of it.
    fn leaks(&self, s: &str) -> bool {
        let key = self.key.expose();
        s.contains(key) || key.split(':').any(|part| part.len() >= 8 && s.contains(part))
    }
    fn auth(&self, r: HttpRequest) -> Result<HttpRequest, ProviderError> {
        let key = self.key.expose();
        let well_formed = key
            .split_once(':')
            .is_some_and(|(id, secret)| !id.is_empty() && !secret.is_empty() && !secret.contains(':') && key.bytes().all(|b| b.is_ascii_graphic()));
        if !well_formed {
            return Err(ProviderError::new(ErrorKind::Auth, "a Higgsfield key is the key ID and the secret joined by a colon"));
        }
        Ok(r.secret_header("Authorization", format!("Key {key}")).max_response_bytes(self.limits.response_bytes))
    }
    async fn send(&self, request: HttpRequest, paid: bool, refusal: Option<&crate::SubmitContext>) -> Result<HttpResponse, ProviderError> {
        let reply = self.http.send_no_redirect(self.auth(request)?).await.map_err(|e| transport(e, paid))?;
        if let Some(context) = refusal
            && reply.body.len() <= self.limits.response_bytes
            && response::refused_submit(&reply)
        {
            context.refused();
        }
        response::check(&reply, self.limits, |s| self.leaks(s))?;
        Ok(reply)
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Staged>> {
        self.staged.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// The exact body for this job: the one already staged under its key, or a new one, with its
    /// images uploaded. Uploads are free, so a failure here proves nothing was billed.
    async fn stage(&self, job: &ProviderJob, prepared: &request::Prepared<'_>) -> Result<Arc<[u8]>, ProviderError> {
        let fingerprint = request::fingerprint(job);
        if let Some(staged) = self.lock().iter().find(|s| s.key == job.idempotency_key && s.job == fingerprint) {
            return Ok(Arc::clone(&staged.body));
        }
        let mut urls = Vec::new();
        urls.try_reserve_exact(prepared.images.len()).map_err(|_| malformed("Higgsfield bounded allocation failed"))?;
        for image in &prepared.images {
            urls.push(self.upload(image.mime, image.bytes).await?);
        }
        let body: Arc<[u8]> = serde_json::to_vec(&prepared.body(&urls)?).map_err(|_| malformed("Higgsfield request couldn't be encoded"))?.into();
        let mut staged = self.lock();
        // Another submit under this key may have staged first; keep the body it may have sent.
        if let Some(existing) = staged.iter().find(|s| s.key == job.idempotency_key && s.job == fingerprint) {
            return Ok(Arc::clone(&existing.body));
        }
        staged.retain(|s| s.key != job.idempotency_key);
        if staged.len() >= STAGED {
            staged.pop_front();
        }
        staged.push_back(Staged { key: job.idempotency_key.clone(), job: fingerprint, body: Arc::clone(&body) });
        Ok(body)
    }
    /// Uploads one image through a presigned grant; the bytes go to storage without the key.
    async fn upload(&self, mime: &'static str, bytes: &[u8]) -> Result<String, ProviderError> {
        let grant = HttpRequest::post_json(format!("{API}/files/generate-upload-url"), &serde_json::json!({"content_type": mime}))?.timeout_ms(30_000);
        let reply = self.send(grant, false, None).await?;
        let grant = response::upload_grant(&reply.body, mime, |s| self.leaks(s))?;
        let mut put = HttpRequest { method: HttpMethod::Put, body: bytes.to_vec(), ..HttpRequest::get(grant.upload_url) }.timeout_ms(120_000);
        for (name, value) in grant.headers {
            put = put.header(name, value);
        }
        let reply = self.http.send_no_redirect(put.max_response_bytes(64 * 1024)).await.map_err(|e| transport(e, false))?;
        if !reply.is_success() {
            let mut error = ProviderError::new(ErrorKind::Server, format!("the Higgsfield upload failed (HTTP {}); nothing was billed", reply.status));
            error.retry_after_ms = ProviderError::from_status(reply.status, b"").retry_after_ms;
            return Err(error);
        }
        Ok(grant.public_url)
    }
    async fn create(&self, job: &ProviderJob, context: Option<&crate::SubmitContext>) -> Result<Submission, ProviderError> {
        let prepared = request::prepare(job, self.limits)?;
        let key = request::idempotency_key(&job.idempotency_key);
        let body = self.stage(job, &prepared).await?;
        let mut submit = HttpRequest { method: HttpMethod::Post, body: body.to_vec(), ..HttpRequest::get(format!("{API}/{}", prepared.model)) }
            .header("Content-Type", "application/json")
            .header("Idempotency-Key", key)
            .timeout_ms(60_000);
        submit.max_response_bytes = self.limits.response_bytes;
        // Everything local is done; from here a lost reply may hide an accepted, billed request,
        // unless Higgsfield answers with a refusal it documents as made before acceptance.
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.send(submit, true, context).await?;
        let id = response::receipt(&reply.body)?;
        Ok(Submission::Queued(JobHandle { remote_id: id, data: serde_json::json!({"v": 1, "model": prepared.model, "count": prepared.count}) }))
    }
    /// What Higgsfield says this job would cost this account. Free: it stages the job's images,
    /// and a submit of the same job afterwards sends exactly the body that was quoted.
    pub async fn estimate(&self, job: &ProviderJob) -> Result<Estimate, ProviderError> {
        let prepared = request::prepare(job, self.limits)?;
        let body = self.stage(job, &prepared).await?;
        let mut quote = HttpRequest { method: HttpMethod::Post, body: body.to_vec(), ..HttpRequest::get(format!("{API}/estimate/{}", prepared.model)) }
            .header("Content-Type", "application/json")
            .timeout_ms(30_000);
        quote.max_response_bytes = self.limits.response_bytes;
        let reply = self.send(quote, false, None).await?;
        response::estimate(&reply.body)
    }
    fn handle(handle: &JobHandle) -> Result<(&'static str, u64), ProviderError> {
        request_id(&handle.remote_id)?;
        let model = match handle.data.get("model").and_then(serde_json::Value::as_str) {
            Some(SOUL) => SOUL,
            Some(KLING_I2V) => KLING_I2V,
            _ => return Err(invalid("Higgsfield handle record refused")),
        };
        let count = handle.data.get("count").and_then(serde_json::Value::as_u64).filter(|n| matches!(n, 1 | 4) && (model == SOUL || *n == 1));
        match (handle.data.get("v").and_then(serde_json::Value::as_u64), count) {
            (Some(1), Some(count)) => Ok((model, count)),
            _ => Err(invalid("Higgsfield handle record refused")),
        }
    }
    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        let (model, count) = Self::handle(handle)?;
        let status = HttpRequest::get(format!("{API}/requests/{}/status", handle.remote_id)).timeout_ms(30_000);
        let reply = self.send(status, false, None).await?;
        match response::status(&reply.body, &handle.remote_id, model, count, |s| self.leaks(s))? {
            response::Status::Done(urls) => {
                drop(reply);
                let mut outputs = Vec::new();
                outputs.try_reserve_exact(urls.len()).map_err(|_| malformed("Higgsfield bounded allocation failed"))?;
                let mut total = 0usize;
                for url in &urls {
                    let (mime, bytes) = self.download(url, model).await?;
                    total = total
                        .checked_add(bytes.len())
                        .filter(|n| *n <= self.limits.total_output_bytes)
                        .ok_or_else(|| malformed("Higgsfield results exceed the download limit"))?;
                    outputs.push(Output::Bytes { mime: mime.into(), bytes });
                }
                Ok(JobProgress::Done(Completed::new(outputs)))
            }
            response::Status::Progress(progress) => Ok(progress),
        }
    }
    /// A result on Higgsfield's storage: fetched without the key, and checked to be the kind of
    /// media the model makes.
    async fn download(&self, url: &str, model: &str) -> Result<(&'static str, Arc<[u8]>), ProviderError> {
        let request = HttpRequest::get(url).max_response_bytes(self.limits.output_bytes).timeout_ms(300_000);
        let reply = self.http.send_no_redirect(request).await.map_err(|e| transport(e, false))?;
        if !reply.is_success() {
            // Retryable whatever the status: the next poll reads the request again, so a billed
            // result is never given up on one failed fetch.
            let mut error =
                ProviderError::new(ErrorKind::Server, format!("the Higgsfield result download failed (HTTP {}); the request was billed", reply.status));
            error.retry_after_ms = ProviderError::from_status(reply.status, b"").retry_after_ms;
            return Err(error);
        }
        if reply.body.is_empty() || reply.body.len() > self.limits.output_bytes {
            return Err(malformed("the Higgsfield result is empty or exceeds the download limit"));
        }
        let mime = response::media(&reply.body, model)?;
        Ok((mime, Arc::from(reply.body)))
    }
    async fn stop(&self, handle: &JobHandle) -> Result<(), ProviderError> {
        Self::handle(handle)?;
        let cancel = HttpRequest { method: HttpMethod::Post, ..HttpRequest::get(format!("{API}/requests/{}/cancel", handle.remote_id)) }.timeout_ms(30_000);
        let reply = self.http.send_no_redirect(self.auth(cancel)?).await.map_err(|e| transport(e, false))?;
        // Higgsfield answers 400 with its reason once a request has left the queue; a 400 that
        // says it is about concurrency, or has no readable reason, goes the ordinary way.
        let too_late = reply.status == 400 && response::detail(&reply.body).is_some_and(|d| !d.to_ascii_lowercase().contains("concurrent"));
        if too_late {
            return Err(invalid("Higgsfield has already started this request, so it can't be cancelled; it runs to the end and is billed"));
        }
        response::check(&reply, self.limits, |s| self.leaks(s))
    }
}
impl Provider for Higgsfield {
    fn id(&self) -> &str {
        "higgsfield"
    }
    /// Higgsfield deduplicates submits by `Idempotency-Key` when the body is identical, and the
    /// adapter replays the staged body; a body it no longer has is refused by Higgsfield, not
    /// charged again.
    fn retry_ambiguous_submit(&self) -> bool {
        true
    }
    fn submit<'a>(&'a self, j: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(self.create(j, None))
    }
    fn submit_with_delivery<'a>(&'a self, j: &'a ProviderJob, context: &'a crate::SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        // This constructor is pure; create marks the paid request before it sends it.
        context.arm();
        Box::pin(self.create(j, Some(context)))
    }
    fn poll<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(self.progress(h))
    }
    fn cancel<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(self.stop(h))
    }
    fn poll_interval_ms(&self, progress: &JobProgress) -> u64 {
        // Higgsfield suggests starting at two seconds and backing off toward ten.
        match progress {
            JobProgress::Queued { .. } => 5_000,
            _ => 3_000,
        }
    }
}

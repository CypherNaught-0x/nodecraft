//! Comfy Cloud (#16): ComfyUI hosted by Comfy, run through its versioned v2 API. Jobs have the same
//! shape as the local ComfyUI adapter's (the Workflow node's): `params.workflow` (API format),
//! `params.values` overrides and media inputs whose role is `<node>.<input>`.
//!
//! - **Auth:** one Comfy API key (`comfyui-…`), sent as `Authorization: Bearer` to the v2 API and as
//!   `X-API-Key` to `/api/object_info` (the v1 API Comfy documents for it). Both are secret headers
//!   and every request is sent without following redirects, so the key never leaves
//!   `cloud.comfy.org`.
//! - **Checked before anything is paid for:** the workflow is checked against the cloud's
//!   `/api/object_info` (missing nodes, values it doesn't offer) before any upload.
//! - **Inputs** upload as assets (`POST /api/v2/assets`, deduplicated by content) and go into the
//!   workflow as `core/ASSET` references.
//! - **Submitting once:** `POST /api/v2/jobs` carries an `Idempotency-Key` derived from the job's.
//!   Comfy refuses a second request with that key (`idempotency_key_reuse`) and never runs it again,
//!   so the same run can't be billed twice. Comfy Cloud can't look a job up by key, so a lost reply
//!   can't be recovered: it is reported as possibly billed, never retried automatically.
//! - **Outputs** are read from the finished job and downloaded with the key from
//!   `/api/v2/assets/{id}/content`, whose redirect to a signed storage URL is followed without it.
//! - **Not sent:** the key in `extra_data`. Partner (API) nodes bill extra credits that no quote
//!   here covers, so a workflow using one is refused before anything is uploaded.
mod request;
mod response;
#[cfg(test)]
mod tests;

use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission,
};
use nodecraft_comfy::NodeCatalog;
use std::sync::{Arc, Mutex, PoisonError};

/// The adapter id, the catalog's `provider`, and the keychain account of the key.
pub const ID: &str = "comfy-cloud";
const ORIGIN: &str = "https://cloud.comfy.org";
const MIB: usize = 1024 * 1024;
/// How long a fetched `/api/object_info` is reused.
const CATALOG_TTL_MS: u64 = 5 * 60 * 1000;

/// Lower-only limits for one job.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Each uploaded input (Comfy expects single uploads of about 100 MB at most).
    pub input_bytes: usize,
    /// JSON replies (assets, jobs).
    pub response_bytes: usize,
    /// One downloaded output.
    pub output_bytes: usize,
    /// All outputs of one job together.
    pub total_output_bytes: usize,
    /// Output files fetched for one job.
    pub output_files: usize,
}

impl Limits {
    pub const HARD: Self = Self { input_bytes: 100 * MIB, response_bytes: 8 * MIB, output_bytes: 512 * MIB, total_output_bytes: 2048 * MIB, output_files: 256 };

    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.input_bytes > h.input_bytes
            || self.response_bytes > h.response_bytes
            || self.output_bytes > h.output_bytes
            || self.total_output_bytes > h.total_output_bytes
            || self.output_files > h.output_files
        {
            return Err(invalid("Comfy Cloud limits may only be lowered"));
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

fn malformed(s: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, s)
}

/// A transport failure, saying when it may hide a created (billable) job.
fn transport(e: ProviderError, paid: bool) -> ProviderError {
    let message = if paid { "Comfy Cloud transport failed; the job may have been created" } else { "Comfy Cloud transport failed" };
    ProviderError { kind: e.kind, message: message.into(), retry_after_ms: e.retry_after_ms.filter(|ms| *ms <= 3_600_000) }
}

/// The Comfy Cloud adapter. Constructing it sends nothing.
pub struct ComfyCloud {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
    now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    catalog: Mutex<Option<(u64, Arc<NodeCatalog>)>>,
}

impl std::fmt::Debug for ComfyCloud {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComfyCloud").finish_non_exhaustive()
    }
}

impl ComfyCloud {
    /// `now_ms` is a monotonic clock (for the node catalog's cache).
    pub fn new(key: Secret, http: Arc<dyn Http>, now_ms: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        Self { key, http, limits: Limits::HARD, now_ms, catalog: Mutex::new(None) }
    }

    pub fn with_limits(mut self, limits: Limits) -> Result<Self, ProviderError> {
        self.limits = limits.validate()?;
        Ok(self)
    }

    fn bearer(&self, r: HttpRequest) -> HttpRequest {
        r.secret_header("Authorization", format!("Bearer {}", self.key.expose()))
    }

    /// One request to Comfy, without following redirects; a failing status becomes an error.
    async fn send(&self, request: HttpRequest, max_bytes: usize, paid: bool) -> Result<HttpResponse, ProviderError> {
        let reply = self.http.send_no_redirect(request.max_response_bytes(max_bytes)).await.map_err(|e| transport(e, paid))?;
        if reply.body.len() > max_bytes {
            return Err(malformed("the Comfy Cloud reply exceeds the size limit"));
        }
        response::check(&reply, &self.key)?;
        Ok(reply)
    }

    /// The cloud's node catalog, cached for a few minutes.
    pub async fn catalog(&self) -> Result<Arc<NodeCatalog>, ProviderError> {
        let now = (self.now_ms)();
        if let Some((at, c)) = self.catalog.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
            && now.saturating_sub(*at) < CATALOG_TTL_MS
        {
            return Ok(Arc::clone(c));
        }
        let request = HttpRequest::get(format!("{ORIGIN}/api/object_info")).secret_header("X-API-Key", self.key.expose()).timeout_ms(120_000);
        let reply = self.send(request, nodecraft_comfy::MAX_OBJECT_INFO_BYTES, false).await?;
        let (catalog, _skipped) = NodeCatalog::parse(&reply.body).map_err(|e| malformed(format!("Comfy Cloud's node list can't be read: {e}")))?;
        let catalog = Arc::new(catalog);
        *self.catalog.lock().unwrap_or_else(PoisonError::into_inner) = Some((now, Arc::clone(&catalog)));
        Ok(catalog)
    }

    /// Forgets the cached catalog.
    pub fn invalidate_catalog(&self) {
        *self.catalog.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    async fn create(&self, job: &ProviderJob, context: Option<&crate::SubmitContext>) -> Result<Submission, ProviderError> {
        let catalog = self.catalog().await?;
        let mut prepared = request::prepare(job, &catalog, self.limits)?;
        // Uploads are free and deduplicated by content: a failure here proves no job exists.
        for upload in std::mem::take(&mut prepared.uploads) {
            let request = HttpRequest::post_multipart(
                format!("{ORIGIN}/api/v2/assets"),
                &[("content_type", upload.mime), ("file_path", &upload.file_name), ("tags", "input")],
                &[("file", &upload.file_name, upload.mime, &upload.bytes)],
            )?
            .timeout_ms(600_000);
            let reply = self.send(self.bearer(request), self.limits.response_bytes, false).await?;
            let asset = response::asset(&reply.body)?;
            prepared.set_asset(&upload, &asset)?;
        }
        let body = prepared.body()?;
        let submit = self
            .bearer(HttpRequest::post_json(format!("{ORIGIN}/api/v2/jobs"), &body)?)
            .header("Idempotency-Key", request::idempotency_key(&job.idempotency_key))
            .timeout_ms(120_000);
        // Everything local is done; from here a lost reply may hide a created job.
        if let Some(context) = context {
            context.unknown();
        }
        let reply = self.send(submit, self.limits.response_bytes, true).await?;
        let created = response::job(&reply.body)?;
        Ok(Submission::Queued(JobHandle { remote_id: created.id, data: serde_json::json!({"v": 1}) }))
    }

    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        response::check_id(&handle.remote_id)?;
        if handle.data.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
            return Err(invalid("Comfy Cloud handle record refused"));
        }
        let request = self.bearer(HttpRequest::get(format!("{ORIGIN}/api/v2/jobs/{}", handle.remote_id))).timeout_ms(30_000);
        let reply = self.send(request, self.limits.response_bytes, false).await?;
        let job = response::job(&reply.body)?;
        if job.id != handle.remote_id {
            return Err(malformed("Comfy Cloud answered about another job"));
        }
        match job.state()? {
            response::State::Progress(progress) => Ok(progress),
            response::State::Failed(error) => Ok(JobProgress::Failed(error)),
            response::State::Succeeded(outputs) => Ok(JobProgress::Done(Completed::new(self.download(&outputs).await?))),
        }
    }

    /// Downloads a finished job's files within the caps. Files NodeCraft can't show (latents) are
    /// skipped; a job with nothing usable fails.
    async fn download(&self, outputs: &[response::JobOutput]) -> Result<Vec<Output>, ProviderError> {
        if outputs.len() > self.limits.output_files {
            return Err(invalid(format!("the job saved {} files; at most {} are fetched", outputs.len(), self.limits.output_files)));
        }
        let mut results = Vec::new();
        let mut skipped = Vec::new();
        let mut total = 0usize;
        for output in outputs {
            if output.kind == "latent" {
                skipped.push(output.name.clone());
                continue;
            }
            let cap = self.limits.output_bytes.min(self.limits.total_output_bytes.saturating_sub(total));
            if output.size_bytes.is_some_and(|size| usize::try_from(size).map_or(true, |size| size > cap)) {
                return Err(malformed("a Comfy Cloud output exceeds the download limit"));
            }
            let bytes = self.content(&output.id, cap).await?;
            total = total.saturating_add(bytes.len());
            match nodecraft_media::sniff(&bytes) {
                Some(s) if nodecraft_media::is_undecoded_image(s.mime) => skipped.push(output.name.clone()),
                Some(s) if s.kind != nodecraft_media::MediaKind::Text => results.push(Output::Bytes { mime: s.mime.to_owned(), bytes: Arc::from(bytes) }),
                _ if output.kind == "text" => results.push(Output::Text(String::from_utf8_lossy(&bytes).into_owned())),
                _ => skipped.push(output.name.clone()),
            }
        }
        if results.is_empty() {
            let names: Vec<String> = skipped.iter().take(5).cloned().collect();
            return Err(ProviderError::new(ErrorKind::JobFailed, format!("the workflow saved no files NodeCraft can use (skipped: {})", names.join(", "))));
        }
        Ok(results)
    }

    /// An asset's bytes. Comfy answers with a redirect to a signed storage URL, fetched without the
    /// key; a self-hosted surface may answer with the bytes at once.
    async fn content(&self, asset: &str, cap: usize) -> Result<Vec<u8>, ProviderError> {
        response::check_id(asset)?;
        let request = self.bearer(HttpRequest::get(format!("{ORIGIN}/api/v2/assets/{asset}/content"))).max_response_bytes(cap).timeout_ms(600_000);
        let reply = self.http.send_no_redirect(request).await.map_err(|e| transport(e, false))?;
        let reply = match reply.status {
            301 | 302 | 303 | 307 | 308 => {
                let location = response::signed_location(&reply, &self.key)?;
                drop(reply);
                let signed = HttpRequest::get(location).max_response_bytes(cap).timeout_ms(600_000);
                let signed = self.http.send_no_redirect(signed).await.map_err(|e| transport(e, false))?;
                if !signed.is_success() {
                    // Signed links expire; the next poll mints fresh ones, so a paid result isn't given up.
                    let mut error =
                        ProviderError::new(ErrorKind::Server, format!("a Comfy Cloud output download failed (HTTP {}); the job has finished", signed.status));
                    error.retry_after_ms = ProviderError::from_status(signed.status, b"").retry_after_ms;
                    return Err(error);
                }
                signed
            }
            // Comfy's own answer: a revoked key, an expired asset or blocked content says so.
            _ => {
                response::check(&reply, &self.key)?;
                reply
            }
        };
        if reply.body.len() > cap {
            return Err(malformed("a Comfy Cloud output exceeds the download limit"));
        }
        if let Some(announced) = reply.header("content-length").and_then(|v| v.trim().parse::<usize>().ok())
            && reply.header("content-encoding").is_none()
            && announced != reply.body.len()
        {
            return Err(ProviderError::new(ErrorKind::Network, "a Comfy Cloud output download was cut short"));
        }
        Ok(reply.body)
    }

    async fn stop(&self, handle: &JobHandle) -> Result<(), ProviderError> {
        response::check_id(&handle.remote_id)?;
        let request = self
            .bearer(HttpRequest { method: crate::HttpMethod::Post, ..HttpRequest::get(format!("{ORIGIN}/api/v2/jobs/{}/cancel", handle.remote_id)) })
            .timeout_ms(30_000);
        // Idempotent: canceling a finished job answers with its final state.
        self.send(request, self.limits.response_bytes, false).await.map(|_| ())
    }
}

impl Provider for ComfyCloud {
    fn id(&self) -> &str {
        ID
    }

    /// No: a retry under the same key is refused rather than answered with the job, so it could
    /// only fail. A lost reply is reported as possibly billed instead.
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }

    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(self.create(job, None))
    }

    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a crate::SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        // Construction sends nothing; `create` marks uncertainty before the paid request.
        context.arm();
        Box::pin(self.create(job, Some(context)))
    }

    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(self.progress(handle))
    }

    fn cancel<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(self.stop(handle))
    }

    fn poll_interval_ms(&self, progress: &JobProgress) -> u64 {
        match progress {
            JobProgress::Queued { .. } => 3_000,
            _ => 1_500,
        }
    }
}

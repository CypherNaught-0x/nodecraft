//! Higgsfield (#177): SOUL 2 text to image, Kling 2.5 Turbo Pro image to video and the Genjutsu
//! source video edits (#179: motion transfer, object swap, restyle) on Higgsfield's queue API.
//! SOUL 2 can draw a Soul ID (#180), an identity trained on the account: the adapter lists the
//! account's ready ones, and confirms the one a request names is ready before paying. It also
//! trains new ones from wired images (#680, [`training`]), never sending one training twice. Inputs stage through Higgsfield's presigned uploads, which are free; the generation
//! submit is the paid step. It carries the job's idempotency key, and the exact body sent under a
//! key is kept for this process, so a retry after a lost reply replays it byte for byte and gets
//! the original request back instead of paying for a second one. Higgsfield keeps results for at
//! least seven days, so a finished request downloads at once.
mod request;
mod response;
#[cfg(test)]
mod tests;
mod training;
use crate::secrets::Secret;
use crate::{
    BoxFuture, Clock, Completed, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob,
    StagedSubmit, Submission,
};
use nodecraft_media::ContentHash;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// SOUL 2 text to image. Higgsfield's SOUL 1 lives on another path; this is the v2 model.
pub const SOUL: &str = "higgsfield-ai/soul/v2/standard";
/// Kling 2.5 Turbo Pro: one image animated into a 5 or 10 second clip.
pub const KLING_I2V: &str = "kling-video/v2.5-turbo/pro/image-to-video";
/// Genjutsu motion transfer: the source video's motion, performed by the reference images' subjects.
pub const MOTION_TRANSFER: &str = "higgsfield/genjutsu/motion-transfer/v1.0";
/// Genjutsu object swap: an object in the source video replaced by the reference images' one.
pub const OBJECT_SWAP: &str = "higgsfield/genjutsu/object-swap/v1.0";
/// Genjutsu restyle: the source video redrawn in one of Higgsfield's preset styles.
pub const RESTYLE: &str = "higgsfield/genjutsu/restyle/v1.0";
/// Soul ID training for SOUL 2 (#680): `POST /v1/custom-references`. Not a queue model; the id
/// names the trainer in handles and style descriptors.
pub const SOUL_TRAINER: &str = "higgsfield/soul-id/v2";
/// The style family a trained Soul ID is for, as the trainer's catalog entry names it.
pub const SOUL_FAMILY: &str = "soul-2";
/// The restyle presets Higgsfield lists, as the `preset_id` option list.
pub const RESTYLE_PRESETS: &str = "higgsfield.restyle_presets";
/// The account's ready SOUL 2 Soul IDs, as the `custom_reference_id` option list.
pub const SOUL_IDS: &str = "higgsfield.soul_ids";
/// Soul IDs asked for per list page, and the pages read: an account with more is refused
/// rather than listed in part.
const SOUL_PAGE: usize = 50;
const SOUL_PAGES: usize = 20;
const API: &str = "https://api.higgsfield.ai";
const MIB: usize = 1024 * 1024;
/// Bodies kept for exact replay, and uploads kept from estimates; the oldest goes first.
const STAGED: usize = 32;
/// Presigned upload retention is unspecified; reuse an estimate's link for at most five minutes.
const UPLOAD_TTL_MS: u64 = 5 * 60 * 1_000;

/// Default clock for callers using the existing two-argument constructor.
#[cfg(not(target_arch = "wasm32"))]
struct UploadClock(std::time::Instant);
#[cfg(target_arch = "wasm32")]
struct UploadClock;
impl Clock for UploadClock {
    fn sleep(&self, _: u64) -> BoxFuture<'static, ()> {
        // Only now_ms is used for upload retention; the adapter never schedules a sleep.
        Box::pin(futures::future::pending())
    }
    fn now_ms(&self) -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
        }
        #[cfg(target_arch = "wasm32")]
        {
            // std::time::Instant::now traps on wasm32-unknown-unknown. The browser run host
            // uses performance.now() too; Date is only a fallback when Performance is absent.
            // Backward clock changes invalidate cached links via fresh()'s checked subtraction.
            let ms = web_sys::window().and_then(|window| window.performance()).map_or_else(js_sys::Date::now, |performance| performance.now());
            if ms.is_finite() && ms > 0.0 { ms as u64 } else { 0 }
        }
    }
}

/// Lower-only payload limits for one job.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Higgsfield documents no prompt cap for these models; 10,000 characters is its largest
    /// documented one (Genjutsu).
    pub prompt_chars: usize,
    /// Each staged image. Higgsfield documents no upload cap.
    pub input_bytes: usize,
    /// A staged source video. Higgsfield documents 200 MiB for Genjutsu sources.
    pub video_bytes: usize,
    /// JSON replies (receipts, status, upload grants, estimates).
    pub response_bytes: usize,
    /// Each downloaded result.
    pub output_bytes: usize,
    /// All results of one request together.
    pub total_output_bytes: usize,
}
impl Limits {
    pub const HARD: Self = Self {
        prompt_chars: 10_000,
        input_bytes: 20 * MIB,
        video_bytes: 200 * MIB,
        response_bytes: MIB,
        output_bytes: 256 * MIB,
        total_output_bytes: 512 * MIB,
    };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.prompt_chars > h.prompt_chars
            || self.input_bytes > h.input_bytes
            || self.video_bytes > h.video_bytes
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

/// One restyle preset: the id a request names, and the name a person reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preset {
    pub id: String,
    pub name: String,
}

/// The presets Higgsfield lists, in its order, and how many unusable entries were left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Presets {
    pub presets: Vec<Preset>,
    pub skipped: usize,
}

/// A Soul ID trained for SOUL 2 and ready to use: the id a request names, and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoulId {
    pub id: String,
    pub name: String,
}

/// The account's ready SOUL 2 Soul IDs, in Higgsfield's order, and how many entries didn't read
/// as a reference. Ones still training, failed or trained for another SOUL aren't listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SoulIds {
    pub ids: Vec<SoulId>,
    pub skipped: usize,
}

/// Where a Soul ID's training stands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Training {
    Pending,
    Completed,
    /// With Higgsfield's reason, when it gave one.
    Failed(Option<String>),
    /// A state Higgsfield doesn't document, as it said it.
    Unknown(String),
}

/// A Soul ID as `GET /v1/custom-references/{id}` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reference {
    /// Trained for SOUL 2 (`model_version: "v2"`).
    pub v2: bool,
    pub training: Training,
}

/// One staged body: what was sent, or is about to be sent, under an idempotency key.
struct Staged {
    key: String,
    /// The job it was staged for: a different job under the same key never reuses it.
    job: ContentHash,
    body: Arc<[u8]>,
    /// Known timestamps for first-send freshness; replay must never replace this body.
    uploads: Vec<Uploaded>,
    dispatched: bool,
}

/// A file an estimate uploaded, kept for the submit that follows it.
#[derive(Clone)]
struct Uploaded {
    mime: &'static str,
    bytes: ContentHash,
    url: String,
    uploaded_at_ms: u64,
}
fn fresh(uploaded: &Uploaded, now: u64) -> bool {
    now.checked_sub(uploaded.uploaded_at_ms).is_some_and(|age| age < UPLOAD_TTL_MS)
}

struct Assembled {
    body: Arc<[u8]>,
    uploads: Vec<Uploaded>,
}

/// Whether an upload is for an estimate, which keeps it, or a submit, which takes it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    Estimate,
    Submit,
}

/// Fixed-origin queue adapter. Constructing it reads no credential and sends nothing.
pub struct Higgsfield {
    /// The key ID and secret joined by a colon, as Higgsfield issues them.
    key: Secret,
    http: Arc<dyn Http>,
    clock: Arc<dyn Clock>,
    limits: Limits,
    staged: Mutex<VecDeque<Staged>>,
    /// Uploads made for estimates (#663). The job a person runs after its estimate sends the
    /// same files again, but its seeds are drawn and its body built only then; the files are
    /// what can be reused, each by one submit.
    uploaded: Mutex<VecDeque<Uploaded>>,
    /// Keys of the Soul ID trainings this process has sent, newest last: one whose reply was
    /// lost is looked for, never sent again.
    posted: Mutex<VecDeque<String>>,
}
impl Higgsfield {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let clock = UploadClock(std::time::Instant::now());
        #[cfg(target_arch = "wasm32")]
        let clock = UploadClock; // No Instant construction on this cfg path: browser-safe even at runtime.
        Self::with_clock(key, http, Arc::new(clock))
    }
    /// Inject a monotonic clock, including a deterministic fixture clock for upload expiry.
    pub fn with_clock(key: Secret, http: Arc<dyn Http>, clock: Arc<dyn Clock>) -> Self {
        Self {
            key,
            http,
            clock,
            limits: Limits::HARD,
            staged: Mutex::new(VecDeque::new()),
            uploaded: Mutex::new(VecDeque::new()),
            posted: Mutex::new(VecDeque::new()),
        }
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
        self.send_checked(request, paid, refusal, None, None).await
    }
    async fn send_checked(
        &self,
        request: HttpRequest,
        paid: bool,
        refusal: Option<&crate::SubmitContext>,
        uploads: Option<&[Uploaded]>,
        staged_key: Option<&str>,
    ) -> Result<HttpResponse, ProviderError> {
        let request = self.auth(request)?;
        // Last synchronous step before giving the request to the transport. In particular this
        // occurs AFTER potentially blocking persistence. A stale staged first submit stays
        // unchanged for reconciliation; it is never silently rebuilt under the paid key.
        let now = self.clock.now_ms();
        if uploads.is_some_and(|uploads| !uploads.iter().all(|upload| fresh(upload, now))) {
            return Err(invalid("Higgsfield uploads expired before dispatch; the request was not sent. Check any saved staged request before retrying"));
        }
        if let Some(key) = staged_key
            && let Some(staged) = self.lock().iter_mut().find(|staged| staged.key == key)
        {
            staged.dispatched = true;
        }
        if paid && let Some(context) = refusal {
            context.paid();
        }
        let reply = self.http.send_no_redirect(request).await.map_err(|e| transport(e, paid))?;
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
    /// source video and images uploaded. Uploads are free, so a failure here proves nothing was
    /// billed.
    async fn stage(&self, job: &ProviderJob, prepared: &request::Prepared<'_>) -> Result<(Arc<[u8]>, Option<Vec<Uploaded>>), ProviderError> {
        let fingerprint = request::fingerprint(job);
        if let Some(staged) = self.lock().iter().find(|s| s.key == job.idempotency_key && s.job == fingerprint) {
            // An ambiguous paid submit must replay precisely its original body and key, even if
            // its upload links have expired. New links under the same key could bill twice.
            return Ok((Arc::clone(&staged.body), (!staged.dispatched).then(|| staged.uploads.clone())));
        }
        // A Soul ID is checked once, when the body is first staged; a replay sends the body
        // that was checked.
        if let Some(id) = &prepared.reference {
            self.ready(id).await?;
        }
        let assembled = self.body(prepared, Use::Submit).await?;
        let mut staged = self.lock();
        // Another submit under this key may have staged first; keep the body it may have sent.
        if let Some(existing) = staged.iter().find(|s| s.key == job.idempotency_key && s.job == fingerprint) {
            return Ok((Arc::clone(&existing.body), (!existing.dispatched).then(|| existing.uploads.clone())));
        }
        staged.retain(|s| s.key != job.idempotency_key);
        if staged.len() >= STAGED {
            staged.pop_front();
        }
        staged.push_back(Staged {
            key: job.idempotency_key.clone(),
            job: fingerprint,
            body: Arc::clone(&assembled.body),
            uploads: assembled.uploads.clone(),
            dispatched: false,
        });
        Ok((assembled.body, Some(assembled.uploads)))
    }
    /// The body for `prepared`, its source video and images uploaded, or reused from an estimate.
    async fn body(&self, prepared: &request::Prepared<'_>, using: Use) -> Result<Assembled, ProviderError> {
        let mut video = match &prepared.video {
            Some(video) => Some(self.file(video.mime, video.bytes, using).await?),
            None => None,
        };
        let mut images = Vec::new();
        images.try_reserve_exact(prepared.images.len()).map_err(|_| malformed("Higgsfield bounded allocation failed"))?;
        for image in &prepared.images {
            images.push(self.file(image.mime, image.bytes, using).await?);
        }
        // An earlier URL can expire while later inputs upload. Check the entire assembled set
        // immediately before building a new estimate/first-submit body, not just at cache lookup.
        // Bound refreshes: a clock that moves faster than uploads must refuse rather than send
        // an expired link (and must not loop forever).
        for _ in 0..2 {
            let now = self.clock.now_ms();
            if video.as_ref().is_some_and(|u| !fresh(u, now))
                && let Some(source) = &prepared.video
            {
                self.forget(source.mime, source.bytes);
                video = Some(self.file(source.mime, source.bytes, using).await?);
            }
            for (source, resolved) in prepared.images.iter().zip(images.iter_mut()) {
                if !fresh(resolved, now) {
                    self.forget(source.mime, source.bytes);
                    *resolved = self.file(source.mime, source.bytes, using).await?;
                }
            }
            let dispatch_time = self.clock.now_ms();
            if video.as_ref().is_none_or(|u| fresh(u, dispatch_time)) && images.iter().all(|u| fresh(u, dispatch_time)) {
                let mut urls = Vec::new();
                urls.try_reserve_exact(images.len()).map_err(|_| malformed("Higgsfield bounded allocation failed"))?;
                urls.extend(images.iter().map(|u| u.url.clone()));
                let body = serde_json::to_vec(&prepared.body(video.as_ref().map(|u| u.url.as_str()), &urls)?)
                    .map_err(|_| malformed("Higgsfield request couldn't be encoded"))?;
                let now = self.clock.now_ms();
                if video.as_ref().is_none_or(|u| fresh(u, now)) && images.iter().all(|u| fresh(u, now)) {
                    let mut uploads = images.clone();
                    if let Some(video) = video {
                        uploads.push(video);
                    }
                    return Ok(Assembled { body: body.into(), uploads });
                }
            }
        }
        Err(invalid("Higgsfield uploads expired while preparing the request; nothing was billed, try again"))
    }
    fn uploads(&self) -> std::sync::MutexGuard<'_, VecDeque<Uploaded>> {
        self.uploaded.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn forget(&self, mime: &'static str, bytes: &[u8]) {
        let hash = ContentHash::of(bytes);
        self.uploads().retain(|u| u.mime != mime || u.bytes != hash);
    }
    /// The URL of `bytes` on Higgsfield's storage: one an estimate uploaded, else a new upload.
    /// An estimate keeps what it uploads for later estimates and the next submit; a submit takes
    /// it, so an upload is reused by one submit only within the short retention window.
    async fn file(&self, mime: &'static str, bytes: &[u8], using: Use) -> Result<Uploaded, ProviderError> {
        let hash = ContentHash::of(bytes);
        {
            let now = self.clock.now_ms();
            let mut uploads = self.uploads();
            uploads.retain(|u| fresh(u, now));
            if let Some(at) = uploads.iter().position(|u| u.mime == mime && u.bytes == hash) {
                let uploaded = match using {
                    Use::Estimate => uploads.get(at).cloned(),
                    Use::Submit => uploads.remove(at),
                };
                if let Some(uploaded) = uploaded {
                    return Ok(uploaded);
                }
            }
        }
        let url = self.upload(mime, bytes).await?;
        let now = self.clock.now_ms();
        let uploaded = Uploaded { mime, bytes: hash, url, uploaded_at_ms: now };
        if using == Use::Estimate {
            let mut uploads = self.uploads();
            uploads.retain(|u| fresh(u, now));
            if !uploads.iter().any(|u| u.mime == mime && u.bytes == hash) {
                if uploads.len() >= STAGED {
                    uploads.pop_front();
                }
                uploads.push_back(uploaded.clone());
            }
        }
        Ok(uploaded)
    }
    /// Uploads one image or video through a presigned grant; the bytes go to storage without the key.
    async fn upload(&self, mime: &'static str, bytes: &[u8]) -> Result<String, ProviderError> {
        let grant = HttpRequest::post_json(format!("{API}/files/generate-upload-url"), &serde_json::json!({"content_type": mime}))?.timeout_ms(30_000);
        let reply = self.send(grant, false, None).await?;
        let grant = response::upload_grant(&reply.body, mime, |s| self.leaks(s))?;
        let mut put =
            HttpRequest { method: HttpMethod::Put, body: bytes.to_vec(), ..HttpRequest::get(grant.upload_url) }.timeout_ms(upload_timeout_ms(bytes.len()));
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
        if job.model == SOUL_TRAINER {
            return self.train(job, context).await;
        }
        let prepared = request::prepare(job, self.limits)?;
        let key = request::idempotency_key(&job.idempotency_key);
        let (body, uploads) = self.stage(job, &prepared).await?;
        let mut submit = HttpRequest { method: HttpMethod::Post, body: body.to_vec(), ..HttpRequest::get(format!("{API}/{}", prepared.model)) }
            .header("Content-Type", "application/json")
            .header("Idempotency-Key", key)
            .timeout_ms(60_000);
        submit.max_response_bytes = self.limits.response_bytes;
        let data = serde_json::json!({"v": 1, "model": prepared.model, "count": prepared.count});
        // Everything local is done; from here a lost reply may hide an accepted, billed request,
        // unless Higgsfield answers with a refusal it documents as made before acceptance. Saved
        // first, so a restart can send these same bytes again (#664).
        if let Some(context) = context {
            let staged = String::from_utf8(body.to_vec()).map_err(|_| malformed("a Higgsfield request body wasn't text"))?;
            context.stage(&StagedSubmit { body: staged, data: data.clone() })?;
        }
        let reply = self.send_checked(submit, true, context, uploads.as_deref(), Some(&job.idempotency_key)).await?;
        let id = response::receipt(&reply.body)?;
        Ok(Submission::Queued(JobHandle { remote_id: id, data }))
    }
    /// Sends a request staged before a restart again, byte for byte under its key (#664). Its
    /// uploads are never staged again: Higgsfield would take new links for different parameters.
    /// If it refuses the request (its uploads may have expired), that is reported, not resent.
    async fn replay_staged(&self, job: &ProviderJob, staged: &StagedSubmit, context: &crate::SubmitContext) -> Result<Submission, ProviderError> {
        let damaged = || {
            invalid(
                "the Higgsfield request saved before NodeCraft closed is damaged, so it wasn't sent again; it may have been billed, so check Higgsfield before running it again",
            )
        };
        let model = request::model(&job.model).ok_or_else(damaged)?;
        if staged.body.len() > crate::MAX_STAGED_BODY || job.idempotency_key.is_empty() {
            return Err(damaged());
        }
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&staged.body).map_err(|_| damaged())?;
        // The handle it will be followed by must be one polling accepts, or it would be paid for
        // and never found.
        if Self::record(&staged.data).ok().map(|(saved, _)| saved) != Some(model) {
            return Err(damaged());
        }
        let key = request::idempotency_key(&job.idempotency_key);
        let mut submit = HttpRequest { method: HttpMethod::Post, body: staged.body.as_bytes().to_vec(), ..HttpRequest::get(format!("{API}/{model}")) }
            .header("Content-Type", "application/json")
            .header("Idempotency-Key", key)
            .timeout_ms(60_000);
        submit.max_response_bytes = self.limits.response_bytes;
        context.arm();
        context.paid();
        let reply = match self.send(submit, true, Some(context)).await {
            Ok(reply) => reply,
            Err(e) if e.is_transient() => return Err(e),
            Err(e) => {
                let message = format!(
                    "Higgsfield refused the request saved before NodeCraft closed, sent again with the files uploaded then (they may have expired): {}. It wasn't uploaded and sent anew under the same key; check Higgsfield, then run it again",
                    e.message
                );
                return Err(ProviderError { message, ..e });
            }
        };
        let id = response::receipt(&reply.body)?;
        Ok(Submission::Queued(JobHandle { remote_id: id, data: staged.data.clone() }))
    }
    /// What Higgsfield says this job would cost this account. Free, and it uses no idempotency
    /// key: it uploads the job's files, and the next submit sending the same files reuses them.
    pub async fn estimate(&self, job: &ProviderJob) -> Result<Estimate, ProviderError> {
        if job.model == SOUL_TRAINER {
            return Err(invalid("Higgsfield gives no estimate for Soul ID training; the catalog prices it"));
        }
        let prepared = request::prepare(job, self.limits)?;
        let assembled = self.body(&prepared, Use::Estimate).await?;
        let mut quote =
            HttpRequest { method: HttpMethod::Post, body: assembled.body.to_vec(), ..HttpRequest::get(format!("{API}/estimate/{}", prepared.model)) }
                .header("Content-Type", "application/json")
                .timeout_ms(30_000);
        quote.max_response_bytes = self.limits.response_bytes;
        let reply = self.send_checked(quote, false, None, Some(&assembled.uploads), None).await?;
        response::estimate(&reply.body)
    }
    /// Refuses a Soul ID this account doesn't have, or whose training isn't done or was for
    /// another SOUL, so a request never pays for an image made without its identity. Free.
    async fn ready(&self, id: &str) -> Result<(), ProviderError> {
        if !request::uuid(id) {
            return Err(invalid("a Higgsfield Soul ID is the UUID Higgsfield issued"));
        }
        let status = HttpRequest::get(format!("{API}/v1/custom-references/{id}")).timeout_ms(30_000);
        let reply = self.send(status, false, None).await.map_err(|e| {
            if e.kind == ErrorKind::NotFound {
                invalid("this Higgsfield account has no such Soul ID; Soul IDs belong to the account that trained them. Nothing was billed")
            } else {
                e
            }
        })?;
        let reference = response::reference(&reply.body, id, |s| self.leaks(s))?;
        match reference.training {
            _ if !reference.v2 => Err(invalid("this Soul ID was trained for another SOUL version, not SOUL 2. Nothing was billed")),
            Training::Completed => Ok(()),
            Training::Pending => Err(invalid("this Soul ID is still training; run again once Higgsfield finishes it. Nothing was billed")),
            Training::Failed(Some(reason)) => Err(invalid(format!("this Soul ID's training failed: {reason}. Nothing was billed"))),
            Training::Failed(None) => Err(invalid("this Soul ID's training failed. Nothing was billed")),
            Training::Unknown(status) => Err(invalid(format!("this Soul ID isn't ready: Higgsfield reports it as {status}. Nothing was billed"))),
        }
    }
    /// The account's ready SOUL 2 Soul IDs. Free, and sends nothing but the key.
    pub async fn soul_ids(&self) -> Result<SoulIds, ProviderError> {
        let mut out = SoulIds::default();
        let mut seen = std::collections::BTreeSet::new();
        // Only an empty page ends the list: Higgsfield doesn't document its page size, so a
        // short page may still have more after it. One page past the cap probes for more.
        for page in 1..=SOUL_PAGES.saturating_add(1) {
            let list = HttpRequest::get(format!("{API}/v1/custom-references/list?page={page}&page_size={SOUL_PAGE}")).timeout_ms(30_000);
            let reply = self.send(list, false, None).await?;
            let (ready, entries, skipped) = response::soul_page(&reply.body, SOUL_PAGE, |s| self.leaks(s))?;
            if entries == 0 {
                return Ok(out);
            }
            if page > SOUL_PAGES {
                break;
            }
            out.skipped = out.skipped.saturating_add(skipped);
            for soul in ready {
                if seen.insert(soul.id.clone()) {
                    out.ids.push(SoulId { name: training::shown(&soul.name).to_owned(), ..soul });
                }
            }
        }
        Err(malformed("this Higgsfield account has more Soul IDs than NodeCraft lists"))
    }
    /// The restyle presets Higgsfield lists today. Free, and sends nothing but the key.
    pub async fn restyle_presets(&self) -> Result<Presets, ProviderError> {
        let list = HttpRequest::get(format!("{API}/models/{RESTYLE}/presets")).timeout_ms(30_000);
        let reply = self.send(list, false, None).await?;
        response::presets(&reply.body, |s| self.leaks(s))
    }
    fn handle(handle: &JobHandle) -> Result<(&'static str, u64), ProviderError> {
        request_id(&handle.remote_id)?;
        Self::record(&handle.data)
    }
    /// A handle's saved record: its model and how many outputs it asked for.
    fn record(data: &serde_json::Value) -> Result<(&'static str, u64), ProviderError> {
        let model = match data.get("model").and_then(serde_json::Value::as_str) {
            Some(SOUL) => SOUL,
            Some(KLING_I2V) => KLING_I2V,
            Some(MOTION_TRANSFER) => MOTION_TRANSFER,
            Some(OBJECT_SWAP) => OBJECT_SWAP,
            Some(RESTYLE) => RESTYLE,
            Some(SOUL_TRAINER) => SOUL_TRAINER,
            _ => return Err(invalid("Higgsfield handle record refused")),
        };
        let count = data.get("count").and_then(serde_json::Value::as_u64).filter(|n| matches!(n, 1 | 4) && (model == SOUL || *n == 1));
        match (data.get("v").and_then(serde_json::Value::as_u64), count) {
            (Some(1), Some(count)) => Ok((model, count)),
            _ => Err(invalid("Higgsfield handle record refused")),
        }
    }
    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        let (model, count) = Self::handle(handle)?;
        if model == SOUL_TRAINER {
            return self.training(&handle.remote_id).await;
        }
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
        if Self::handle(handle)?.0 == SOUL_TRAINER {
            return Err(invalid("Higgsfield can't stop a Soul ID's training once it is sent; it runs to the end and is billed"));
        }
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
    /// The plan's price in US dollars, from [`Higgsfield::estimate`].
    fn quote<'a>(&'a self, j: &'a ProviderJob) -> BoxFuture<'a, Result<nodecraft_types::Money, ProviderError>> {
        Box::pin(async move { self.estimate(j).await.map(|estimate| estimate.usd) })
    }
    fn submit_with_delivery<'a>(&'a self, j: &'a ProviderJob, context: &'a crate::SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        // This constructor is pure; create marks the paid request before it sends it.
        context.arm();
        Box::pin(self.create(j, Some(context)))
    }
    fn replay<'a>(
        &'a self,
        j: &'a ProviderJob,
        staged: &'a StagedSubmit,
        context: &'a crate::SubmitContext,
    ) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(self.replay_staged(j, staged, context))
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

/// How long an upload may take: two minutes, plus enough for a slow 1 Mbit/s uplink (8 ms a
/// KiB), so a large source video isn't cut off every time.
fn upload_timeout_ms(bytes: usize) -> u64 {
    120_000u64.saturating_add(u64::try_from(bytes / 1024).unwrap_or(u64::MAX).saturating_mul(8))
}

//! Bounded fal queue adapter for five explicitly selected live model aliases.
//! No remote deduplication guarantee; accepted handles resume, ambiguous creates never replay.
mod json;
mod limits;
mod media;
#[cfg(all(test, feature = "http", not(target_arch = "wasm32")))]
mod native_tests;
mod request;
mod response;
#[cfg(test)]
mod tests;
use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob,
    Submission,
};
pub use limits::Limits;
use limits::reserve;
use nodecraft_types::BillableUnits;
use std::sync::Arc;
pub const FLUX: &str = "fal-ai/flux/schnell";
pub const WAN: &str = "fal-ai/wan/v2.2-a14b/text-to-video";
pub const KOKORO: &str = "fal-ai/kokoro/american-english";
pub const TRELLIS: &str = "fal-ai/trellis-2";
pub const ESRGAN: &str = "fal-ai/esrgan";
const ORIGIN: &str = "https://queue.fal.run";
fn invalid(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, s)
}
fn malformed(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, s)
}
fn transport(e: ProviderError) -> ProviderError {
    ProviderError {
        kind: e.kind,
        message: "fal transport failed; prior processing or billing may be uncertain".into(),
        retry_after_ms: e.retry_after_ms.filter(|n| *n <= 3_600_000),
    }
}
fn response_bounds(r: &HttpResponse, cap: usize) -> Result<(), ProviderError> {
    if r.body.len() > cap {
        return Err(malformed("fal response exceeds admitted body limit"));
    }
    if r.headers.len() > 128 {
        return Err(malformed("fal HTTP metadata exceeds limit"));
    }
    let mut content_length = None;
    let mut header_bytes = 0usize;
    for (name, value) in &r.headers {
        header_bytes = header_bytes
            .checked_add(name.len())
            .and_then(|n| n.checked_add(value.len()))
            .filter(|n| *n <= 256 * 1024)
            .ok_or_else(|| malformed("fal HTTP metadata exceeds limit"))?;
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() || value.len() > 20 || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(malformed("fal invalid Content-Length"));
            }
            content_length = Some(value.parse::<usize>().map_err(|_| malformed("fal Content-Length overflow"))?);
        }
    }
    if content_length.is_some_and(|n| n != r.body.len()) {
        return Err(malformed("fal Content-Length disagrees with received bytes"));
    }
    Ok(())
}
fn status(r: &HttpResponse, cap: usize) -> Result<(), ProviderError> {
    response_bounds(r, cap)?;
    if r.is_success() {
        return Ok(());
    }
    let kind = crate::ProviderError::from_status(r.status, b"").kind;
    let mut error = ProviderError::new(kind, "fal request failed; processing or billing may remain uncertain");
    error.retry_after_ms = r
        .header("retry-after")
        .filter(|value| value.len() <= 16)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .and_then(|seconds| seconds.checked_mul(1000))
        .filter(|millis| *millis <= 3_600_000);
    Err(error)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Model {
    Image,
    Video,
    Audio,
    Mesh,
    Upscale,
}
impl Model {
    fn read(s: &str) -> Result<Self, ProviderError> {
        match s {
            FLUX => Ok(Self::Image),
            WAN => Ok(Self::Video),
            KOKORO => Ok(Self::Audio),
            TRELLIS => Ok(Self::Mesh),
            ESRGAN => Ok(Self::Upscale),
            _ => Err(invalid("choose one of the five supported fal models")),
        }
    }
    fn wire(self) -> &'static str {
        match self {
            Self::Image => FLUX,
            Self::Video => WAN,
            Self::Audio => KOKORO,
            Self::Mesh => TRELLIS,
            Self::Upscale => ESRGAN,
        }
    }
    fn root(self) -> &'static str {
        match self {
            Self::Image => "fal-ai/flux",
            Self::Video => "fal-ai/wan",
            Self::Audio => "fal-ai/kokoro",
            Self::Mesh => TRELLIS,
            Self::Upscale => ESRGAN,
        }
    }
}
#[derive(Clone, Copy)]
struct Record {
    model: Model,
    requested: u8,
    width: u32,
    height: u32,
}
struct Routes<'a> {
    status: std::borrow::Cow<'a, str>,
    result: std::borrow::Cow<'a, str>,
    cancel: std::borrow::Cow<'a, str>,
}
fn id(s: &str, key: &Secret) -> Result<(), ProviderError> {
    if s.is_empty() || s.len() > 128 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) || s.contains(key.expose()) {
        return Err(malformed("fal request identity refused or redacted"));
    }
    Ok(())
}
fn routes(model: Model, id: &str, r: &Routes<'_>, key: &Secret) -> Result<(), ProviderError> {
    // Raw string equality with literal origins rejects :443, percent aliases, dot segments,
    // backslashes, credentials, queries and URL normalization BEFORE any URL parser can erase them.
    for value in [&r.status, &r.result, &r.cancel] {
        if value.len() > 4096 || value.contains(key.expose()) {
            return Err(malformed("fal route refused or redacted"));
        }
    }
    let root = format!("{ORIGIN}/{}/requests/{id}", model.root());
    let short = r.status == format!("{root}/status") && r.result == root && r.cancel == format!("{root}/cancel");
    let full = if model == Model::Image {
        let root = format!("{ORIGIN}/{FLUX}/requests/{id}");
        r.status == format!("{root}/status") && (r.result == root || r.result == format!("{root}/response")) && r.cancel == format!("{root}/cancel")
    } else {
        false
    };
    if !short && !full {
        return Err(malformed("fal receipt routes do not match the selected model and request"));
    }
    Ok(())
}
impl Record {
    fn handle(self, id: &str, r: &Routes<'_>) -> JobHandle {
        JobHandle {
            remote_id: id.into(),
            data: serde_json::json!({"v":1,"model":self.model.wire(),"requested":self.requested,"width":self.width,"height":self.height,"status_url":r.status,"response_url":r.result,"cancel_url":r.cancel}),
        }
    }
    fn read<'a>(h: &'a JobHandle, key: &Secret) -> Result<(Self, Routes<'a>), ProviderError> {
        id(&h.remote_id, key)?;
        let o = h.data.as_object().filter(|o| o.len() == 8).ok_or_else(|| invalid("fal invalid handle record"))?;
        let text = |k: &str| o.get(k).and_then(|v| v.as_str()).ok_or_else(|| invalid("fal invalid handle field"));
        if o.get("v").and_then(|v| v.as_u64()) != Some(1) {
            return Err(invalid("fal unsupported handle version"));
        }
        let model = Model::read(text("model")?)?;
        let num = |k: &str| o.get(k).and_then(|v| v.as_u64()).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| invalid("fal invalid handle dimensions/count"));
        let record = Self {
            model,
            requested: u8::try_from(num("requested")?).map_err(|_| invalid("fal invalid handle count"))?,
            width: num("width")?,
            height: num("height")?,
        };
        if !(1..=if model == Model::Image { 4 } else { 1 }).contains(&record.requested)
            || (model == Model::Upscale
                && (record.width == 0
                    || record.height == 0
                    || record.width > 2048
                    || record.height > 2048
                    || !record.width.is_multiple_of(2)
                    || !record.height.is_multiple_of(2)))
            || (model != Model::Upscale && (record.width != 0 || record.height != 0))
        {
            return Err(invalid("fal handle output contract refused"));
        }
        let r = Routes { status: text("status_url")?.into(), result: text("response_url")?.into(), cancel: text("cancel_url")?.into() };
        routes(model, &h.remote_id, &r, key)?;
        Ok((record, r))
    }
}
#[derive(Debug, PartialEq, Eq)]
enum CancelDisposition {
    Requested,
    AlreadyCompleted,
    NotFound,
}
impl CancelDisposition {
    fn label(&self) -> &'static str {
        match self {
            Self::Requested => "cancellation_requested",
            Self::AlreadyCompleted => "already_completed",
            Self::NotFound => "not_found",
        }
    }
}
/// Host-injected credential and trusted bounded HTTP implementation; constructor performs no I/O.
pub struct Fal {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl Fal {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { key, http, limits: limits.validate()? })
    }
    fn auth(&self, r: HttpRequest) -> HttpRequest {
        r.secret_header("Authorization", format!("Key {}", self.key.expose()))
    }
    async fn cancel_route(&self, url: &str) -> Result<CancelDisposition, ProviderError> {
        let r = self
            .http
            .send_no_redirect(
                self.auth(HttpRequest { method: HttpMethod::Put, ..HttpRequest::get(url).max_response_bytes(self.limits.response_bytes).timeout_ms(15_000) }),
            )
            .await
            .map_err(transport)?;
        response_bounds(&r, self.limits.response_bytes)?;
        if !matches!(r.status, 202 | 400 | 404) {
            status(&r, self.limits.response_bytes)?;
            return Err(malformed("fal unexpected cancellation HTTP status"));
        }
        let body = json::parse(&r.body, self.limits)?;
        if body.get("error").is_some_and(|value| value.get() != "null") {
            return Err(malformed("fal contradictory cancellation response"));
        }
        match (r.status, body.text("status")?.as_ref()) {
            (202, "CANCELLATION_REQUESTED") => Ok(CancelDisposition::Requested),
            (400, "ALREADY_COMPLETED") => Ok(CancelDisposition::AlreadyCompleted),
            (404, "NOT_FOUND") => Ok(CancelDisposition::NotFound),
            _ => Err(malformed("fal cancellation status disagrees with HTTP response")),
        }
    }
    async fn create(&self, j: &ProviderJob, context: Option<&crate::SubmitContext>) -> Result<Submission, ProviderError> {
        let (record, body) = request::prepare(j, self.limits)?;
        let request = self.auth(HttpRequest {
            method: HttpMethod::Post,
            body,
            ..HttpRequest::get(format!("{ORIGIN}/{}", record.model.wire()))
                .max_response_bytes(self.limits.response_bytes)
                .header("Content-Type", "application/json")
                .header("X-Fal-No-Retry", "1")
                .header("x-app-fal-disable-fallback", "true")
        });
        // All local preparation is complete. The transport constructor itself may have
        // effects, so uncertainty must precede invocation, not merely future polling.
        if let Some(context) = context {
            context.unknown();
        }
        let r = self.http.send_no_redirect(request).await.map_err(transport)?;
        status(&r, self.limits.response_bytes)?;
        // A second bounded receipt pass can recover cleanup authority after a LOWER application
        // metadata cap fails. It still validates complete JSON and the same hard representation cap.
        let receipt = json::parse(&r.body, Limits { metadata_bytes: Limits::HARD.metadata_bytes, ..self.limits }).and_then(|o| {
            let i = o.text("request_id")?;
            id(&i, &self.key)?;
            // Identity-only authority permits cleanup on a local canonical route, never on
            // a response-provided URL that may be precisely what admission refused.
            Ok(i)
        });
        let admitted = json::parse(&r.body, self.limits).and_then(|o| {
            let s = o.optional_text("status")?;
            if s.as_deref().is_some_and(|s| !matches!(s, "IN_QUEUE" | "IN_PROGRESS" | "COMPLETED")) {
                return Err(malformed("fal unknown accepted status"));
            }
            if let Some(n) = o.optional_u64("queue_position")?
                && u32::try_from(n).is_err()
            {
                return Err(malformed("fal invalid queue position"));
            }
            let i = o.text("request_id")?;
            id(&i, &self.key)?;
            let urls = Routes { status: o.text("status_url")?, result: o.text("response_url")?, cancel: o.text("cancel_url")? };
            routes(record.model, &i, &urls, &self.key)?;
            Ok(urls)
        });
        match (receipt, admitted) {
            (Ok(i), Ok(urls)) => Ok(Submission::Queued(record.handle(&i, &urls))),
            (Ok(i), Err(mut e)) => {
                let cancel = format!("{ORIGIN}/{}/requests/{i}/cancel", record.model.root());
                let outcome = match self.cancel_route(&cancel).await {
                    Ok(disposition) => disposition.label(),
                    Err(error) => match error.kind {
                        ErrorKind::Auth => "failed_auth",
                        ErrorKind::RateLimited | ErrorKind::Busy => "failed_busy",
                        ErrorKind::Network | ErrorKind::Timeout => "failed_transport",
                        ErrorKind::Malformed => "failed_response_validation",
                        _ => "failed_remote",
                    },
                };
                e.message.push_str(&format!("; accepted request {i}; cancel outcome {outcome}; billing remains uncertain"));
                Err(e)
            }
            (Err(e), _) => Err(e),
        }
    }
    async fn progress(&self, h: &JobHandle) -> Result<JobProgress, ProviderError> {
        let (record, urls) = Record::read(h, &self.key)?;
        let r = self
            .http
            .send_no_redirect(self.auth(HttpRequest::get(urls.status.as_ref()).max_response_bytes(self.limits.response_bytes)))
            .await
            .map_err(transport)?;
        status(&r, self.limits.response_bytes)?;
        let o = json::parse(&r.body, self.limits)?;
        if o.optional_text("request_id")?.is_some_and(|s| s != h.remote_id) {
            return Err(malformed("fal status identity mismatch"));
        }
        if o.optional_text("response_url")?.is_some_and(|s| s != urls.result) {
            return Err(malformed("fal result route changed"));
        }
        match o.text("status")?.as_ref() {
            "IN_QUEUE" => Ok(JobProgress::Queued {
                position: o.optional_u64("queue_position")?.map(u32::try_from).transpose().map_err(|_| malformed("fal invalid queue position"))?,
            }),
            "IN_PROGRESS" => Ok(JobProgress::Running { fraction: None, message: None, partial: None }),
            "COMPLETED" => {
                let error = o.optional_text("error")?;
                let kind = o.optional_text("error_type")?;
                if error.as_ref().is_some_and(|s| !s.is_empty()) || kind.as_ref().is_some_and(|s| !s.is_empty()) {
                    return Ok(JobProgress::Failed(ProviderError::new(
                        ErrorKind::JobFailed,
                        "fal reported terminal generation failure; prior billing remains uncertain",
                    )));
                }
                drop(o);
                drop(r);
                self.download(record, &urls.result).await.map(JobProgress::Done)
            }
            _ => Err(malformed("fal unknown queue status")),
        }
    }
    async fn download(&self, record: Record, url: &str) -> Result<Completed, ProviderError> {
        let r = self.http.send_no_redirect(self.auth(HttpRequest::get(url).max_response_bytes(self.limits.response_bytes))).await.map_err(transport)?;
        status(&r, self.limits.response_bytes)?;
        let files = response::outputs(&r.body, record, self.limits, &self.key)?;
        let billable_units = response::billable_units(&r);
        drop(r);
        let mut outputs = Vec::new();
        reserve(&mut outputs, files.len())?;
        let mut remaining = self.limits.total_output_bytes;
        let mut pixels = 0u64;
        for f in files {
            let cap = remaining.min(self.limits.output_bytes);
            if cap == 0 {
                return Err(malformed("fal aggregate output capacity exhausted"));
            }
            let reply = self.http.send_no_redirect(HttpRequest::get(&f.url).max_response_bytes(cap)).await.map_err(transport)?;
            if reply.body.len() > cap {
                return Err(malformed("fal output exceeds remaining byte capacity"));
            }
            status(&reply, self.limits.output_bytes).map_err(|mut error| {
                error.message = "fal output download failed; the accepted request may have been billed".into();
                error
            })?;
            if reply.body.is_empty() || f.size.is_some_and(|n| usize::try_from(n).ok() != Some(reply.body.len())) {
                return Err(malformed("fal output size disagrees with body"));
            }
            let mime = media::validate(&reply.body, record, self.limits, &mut pixels)?;
            if f.mime.as_deref().is_some_and(|m| m != mime) {
                return Err(malformed("fal output MIME disagrees with actual bytes"));
            }
            if matches!(record.model, Model::Image | Model::Upscale) && (f.width.is_some() || f.height.is_some()) {
                let meta =
                    nodecraft_media::probe_image(&reply.body, &self.limits.image(false)).map_err(|_| malformed("fal invalid declared image dimensions"))?;
                if f.width.is_some_and(|w| w != u64::from(meta.width)) || f.height.is_some_and(|h| h != u64::from(meta.height)) {
                    return Err(malformed("fal declared geometry disagrees with actual image"));
                }
            }
            remaining = remaining.checked_sub(reply.body.len()).ok_or_else(|| malformed("fal output byte overflow"))?;
            outputs.push(Output::Bytes { mime: mime.into(), bytes: Arc::from(reply.body) });
        }
        let mut done = Completed::new(outputs);
        done.usage.billable_units = billable_units;
        Ok(done)
    }
}
impl Provider for Fal {
    fn id(&self) -> &str {
        "fal"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, j: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(self.create(j, None))
    }
    fn submit_with_delivery<'a>(&'a self, j: &'a ProviderJob, context: &'a crate::SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        // This constructor is pure; create marks uncertainty before any transport call.
        context.arm();
        Box::pin(self.create(j, Some(context)))
    }
    fn poll<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(self.progress(h))
    }
    fn cancel<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let (_, r) = Record::read(h, &self.key)?;
            self.cancel_route(&r.cancel).await.map(|_| ())
        })
    }
    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        2_000
    }
}
fn observe(_stage: usize) {
    #[cfg(test)]
    tests::observe(_stage);
}

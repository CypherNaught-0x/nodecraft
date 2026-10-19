//! Version-pinned Replicate predictions. No remote deduplication or exact monetary quote.
//! Production requests never follow redirects; builtin browser transport refuses this operation.
mod receipt;
mod request;
mod response;
#[cfg(test)]
mod tests;

use crate::secrets::Secret;
use crate::{BoxFuture, Completed, ErrorKind, Http, HttpMethod, HttpRequest, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission};
use std::sync::Arc;

pub const SDXL: &str = "stability-ai/sdxl:7762fd07cf82c948538e41f63f77d685e02b063e37e496e96eefd46c929f9bdc";
pub const HELLO: &str = "replicate/hello-world:5c7d5dc6dd8bf75c1acaa8565735e7986bc5b66206b55cca93cb72c9bf15ccaa";
const API: &str = "https://api.replicate.com/v1";
const MIB: usize = 1024 * 1024;

/// Lower-only operation representation limits. Caller inputs, transport/allocator internals and
/// simultaneous host operations are additional; these are not an RSS or global quota contract.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub metadata_bytes: usize,
    pub input_bytes: usize,
    pub total_input_bytes: usize,
    pub request_bytes: usize,
    pub response_bytes: usize,
    pub text_bytes: usize,
    pub output_bytes: usize,
    pub total_output_bytes: usize,
    pub width: u32,
    pub height: u32,
    pub pixels: u64,
    pub total_pixels: u64,
    pub codec_bytes: u64,
}
impl Limits {
    pub const HARD: Self = Self {
        metadata_bytes: 256 * 1024,
        input_bytes: 256_000,
        total_input_bytes: 512_000,
        request_bytes: 4 * MIB,
        response_bytes: 2 * MIB,
        text_bytes: MIB,
        output_bytes: 16 * MIB,
        total_output_bytes: 48 * MIB,
        width: 4096,
        height: 4096,
        pixels: 16_777_216,
        total_pixels: 32_000_000,
        codec_bytes: 128 * MIB as u64,
    };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.metadata_bytes > h.metadata_bytes
            || self.input_bytes > h.input_bytes
            || self.total_input_bytes > h.total_input_bytes
            || self.request_bytes > h.request_bytes
            || self.response_bytes > h.response_bytes
            || self.text_bytes > h.text_bytes
            || self.output_bytes > h.output_bytes
            || self.total_output_bytes > h.total_output_bytes
            || self.width > h.width
            || self.height > h.height
            || self.pixels > h.pixels
            || self.total_pixels > h.total_pixels
            || self.codec_bytes > h.codec_bytes
        {
            return Err(invalid("Replicate limits may only be lowered"));
        }
        Ok(self)
    }
    fn image(self, input: bool) -> nodecraft_media::Limits {
        nodecraft_media::Limits {
            max_input_bytes: if input { self.input_bytes } else { self.output_bytes } as u64,
            max_width: self.width,
            max_height: self.height,
            max_alloc: self.codec_bytes,
        }
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self::HARD
    }
}
fn invalid(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, s)
}
fn malformed(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, s)
}
fn transport(error: ProviderError) -> ProviderError {
    ProviderError {
        kind: error.kind,
        message: "Replicate transport failed; processing or prior billing may be uncertain".into(),
        retry_after_ms: error.retry_after_ms.filter(|n| *n <= 3_600_000),
    }
}
fn reserve<T>(v: &mut Vec<T>, n: usize) -> Result<(), ProviderError> {
    v.try_reserve_exact(n).map_err(|_| malformed("Replicate buffer allocation refused"))
}
fn extent(meta: &nodecraft_media::ImageMeta, l: Limits) -> Result<u64, ProviderError> {
    let n = u64::from(meta.width).checked_mul(u64::from(meta.height)).ok_or_else(|| invalid("Replicate image dimensions overflow"))?;
    if meta.width == 0 || meta.height == 0 || meta.width > l.width || meta.height > l.height || n > l.pixels {
        return Err(invalid("Replicate image geometry exceeds limits"));
    }
    Ok(n)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Model {
    Image,
    Text,
}
impl Model {
    fn from_wire(s: &str) -> Result<Self, ProviderError> {
        match s {
            SDXL => Ok(Self::Image),
            HELLO => Ok(Self::Text),
            _ => Err(invalid("choose a supported pinned Replicate model version")),
        }
    }
    fn wire(self) -> &'static str {
        match self {
            Self::Image => SDXL,
            Self::Text => HELLO,
        }
    }
    fn parts(self) -> (&'static str, &'static str) {
        self.wire().split_once(':').unwrap_or_default()
    }
}
#[derive(Clone, Copy)]
struct Record {
    model: Model,
    requested: u8,
}
impl Record {
    fn handle(self, id: &str) -> JobHandle {
        JobHandle {
            remote_id: id.to_owned(),
            data: serde_json::json!({"v":1,"model":self.model.wire(),"shape":if self.model==Model::Image{"images"}else{"text"},"requested":self.requested}),
        }
    }
    fn read(h: &JobHandle) -> Result<Self, ProviderError> {
        if h.remote_id.is_empty() || h.remote_id.len() > 128 || !h.remote_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(invalid("invalid Replicate prediction id"));
        }
        let o = h.data.as_object().filter(|o| o.len() == 4).ok_or_else(|| invalid("invalid Replicate handle record"))?;
        if o.get("v").and_then(|v| v.as_u64()) != Some(1) {
            return Err(invalid("unsupported Replicate handle version"));
        }
        let model = Model::from_wire(o.get("model").and_then(|v| v.as_str()).ok_or_else(|| invalid("missing Replicate handle model"))?)?;
        let requested =
            o.get("requested").and_then(|v| v.as_u64()).and_then(|n| u8::try_from(n).ok()).ok_or_else(|| invalid("invalid Replicate handle cardinality"))?;
        let shape = o.get("shape").and_then(|v| v.as_str());
        if (model == Model::Image && (!(1..=4).contains(&requested) || shape != Some("images")))
            || (model == Model::Text && (requested != 1 || shape != Some("text")))
        {
            return Err(invalid("invalid Replicate handle output shape"));
        }
        Ok(Self { model, requested })
    }
}

/// Receives a key from the host; does not read environment, keychain or project credentials.
pub struct Replicate {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl Replicate {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { key, http, limits: limits.validate()? })
    }
    // IDs become URL paths and diagnostics, neither of which has secret_header redaction.
    fn check_id(&self, id: &str) -> Result<(), ProviderError> {
        if id.contains(self.key.expose()) {
            return Err(malformed("Replicate identifier contains configured credentials and was redacted"));
        }
        Ok(())
    }
    async fn reject_accepted(&self, id: &str, mut error: ProviderError) -> ProviderError {
        if self.check_id(id).is_err() {
            error.message.push_str("; accepted identifier redacted; cancellation could not safely be requested; billing remains uncertain");
            return error;
        }
        let req = self.authenticate(HttpRequest {
            method: HttpMethod::Post,
            ..HttpRequest::get(self.endpoint(&format!("predictions/{id}/cancel"))).max_response_bytes(self.limits.response_bytes).timeout_ms(15_000)
        });
        let confirmed = match self.http.send_no_redirect(req).await {
            Ok(reply) if reply.is_success() => receipt::parse(&reply.body, self.limits.response_bytes).is_some_and(|r| r.id() == Some(id) && r.terminal),
            _ => false,
        };
        let disposition = if confirmed { "terminal status observed" } else { "cancellation unconfirmed" };
        error.message = format!("{}; accepted prediction {id}: {disposition}; prior billing remains uncertain", error.message);
        error
    }
    fn endpoint(&self, path: &str) -> String {
        format!("{API}/{path}")
    }
    fn authenticate(&self, request: HttpRequest) -> HttpRequest {
        request.secret_header("Authorization", format!("Bearer {}", self.key.expose()))
    }
    async fn create(&self, job: &ProviderJob) -> Result<Submission, ProviderError> {
        let prepared = request::prepare(job, self.limits)?;
        let record = prepared.record;
        let body = prepared.encode(self.limits)?;
        let req = self.authenticate(HttpRequest {
            method: HttpMethod::Post,
            body,
            ..HttpRequest::get(self.endpoint("predictions")).header("Content-Type", "application/json").max_response_bytes(self.limits.response_bytes)
        });
        let reply = self.http.send_no_redirect(req).await.map_err(transport)?;
        let receipt = reply.is_success().then(|| receipt::parse(&reply.body, self.limits.response_bytes)).flatten();
        let admitted = response::parse(&reply, record, self.limits).and_then(|parsed| {
            parsed.identity(None, record)?;
            let accepted = receipt.as_ref().ok_or_else(|| malformed("invalid complete Replicate create receipt"))?;
            let id = accepted.id().ok_or_else(|| malformed("invalid decoded Replicate identifier"))?;
            if id != parsed.id {
                return Err(malformed("Replicate create receipt does not match"));
            }
            self.check_id(id)?;
            Ok(parsed)
        });
        match admitted {
            Ok(parsed) => Ok(Submission::Queued(record.handle(&parsed.id))),
            Err(error) => match receipt {
                Some(receipt) => match receipt.id() {
                    Some(id) => Err(self.reject_accepted(id, error).await),
                    None => Err(error),
                },
                None => Err(error),
            },
        }
    }
    async fn progress(&self, h: &JobHandle) -> Result<JobProgress, ProviderError> {
        let record = Record::read(h)?;
        self.check_id(&h.remote_id)?;
        let req = self.authenticate(HttpRequest::get(self.endpoint(&format!("predictions/{}", h.remote_id))).max_response_bytes(self.limits.response_bytes));
        let reply = self.http.send_no_redirect(req).await.map_err(transport)?;
        let parsed = response::parse(&reply, record, self.limits)?;
        parsed.identity(Some(&h.remote_id), record)?;
        match parsed.status.as_ref() {
            "starting" => Ok(JobProgress::Queued { position: None }),
            "processing" => Ok(JobProgress::Running { fraction: None, message: Some("Replicate is processing the prediction".into()), partial: None }),
            "succeeded" => {
                if parsed.data_removed {
                    return Err(ProviderError::new(
                        ErrorKind::Expired,
                        "Replicate prediction outputs have expired; the accepted prediction may have been billed",
                    ));
                }
                let outputs = response::outputs(parsed.output, record, self.limits)?;
                let done = self.materialize(outputs, self.limits).await?;
                observe(6);
                Ok(JobProgress::Done(Completed::new(done)))
            }
            "failed" => Ok(JobProgress::Failed(ProviderError::new(
                ErrorKind::JobFailed,
                "Replicate prediction failed; review its inputs or model settings before a new paid attempt",
            ))),
            "canceled" | "aborted" => {
                Ok(JobProgress::Failed(ProviderError::new(ErrorKind::Cancelled, "Replicate prediction stopped; this does not establish a refund")))
            }
            _ => Err(malformed("unknown Replicate prediction status")),
        }
    }
    async fn materialize(&self, output: response::AdmittedOutput<'_>, l: Limits) -> Result<Vec<Output>, ProviderError> {
        match output {
            response::AdmittedOutput::Text(text) => {
                let mut out = Vec::new();
                reserve(&mut out, 1)?;
                out.push(Output::Text(text.into_owned()));
                Ok(out)
            }
            response::AdmittedOutput::Images(urls) => {
                // Admit every URL before any credential-free delivery request.
                for u in &urls {
                    response::delivery_url(u)?;
                }
                let mut outputs = Vec::new();
                reserve(&mut outputs, urls.len())?;
                let mut retained = 0usize;
                let mut total_pixels = 0u64;
                for u in urls {
                    let remaining = l.total_output_bytes.checked_sub(retained).ok_or_else(|| malformed("Replicate output extent overflow"))?;
                    let cap = l.output_bytes.min(remaining);
                    if cap == 0 {
                        return Err(malformed("Replicate aggregate output capacity exhausted before download"));
                    }
                    let req = HttpRequest::get(u.as_ref()).max_response_bytes(cap);
                    observe(4);
                    let reply = self.http.send_no_redirect(req).await.map_err(transport)?;
                    response::download_status(&reply, l, cap)?;
                    if reply.body.len() > cap {
                        return Err(malformed("Replicate download exceeds remaining capacity"));
                    }
                    let meta = nodecraft_media::probe_image(&reply.body, &l.image(false))
                        .map_err(|_| malformed("Replicate output is not a supported bounded image"))?;
                    if !matches!(meta.mime, "image/png" | "image/jpeg" | "image/webp") {
                        return Err(malformed("Replicate returned an unsupported image format"));
                    }
                    let n = extent(&meta, l).map_err(|_| malformed("Replicate output geometry exceeds limits"))?;
                    total_pixels = total_pixels.checked_add(n).ok_or_else(|| malformed("Replicate pixel extent overflow"))?;
                    if total_pixels > l.total_pixels {
                        return Err(malformed("Replicate aggregate output geometry exceeds limits"));
                    }
                    observe(1);
                    let _ = nodecraft_media::decode_image(&reply.body, &l.image(false))
                        .map_err(|_| malformed("Replicate output is corrupt or exceeds codec limits"))?;
                    retained = retained.checked_add(reply.body.len()).ok_or_else(|| malformed("Replicate output extent overflow"))?;
                    outputs.push(Output::Bytes { mime: meta.mime.into(), bytes: Arc::from(reply.body) });
                }
                Ok(outputs)
            }
        }
    }
}
impl Provider for Replicate {
    fn id(&self) -> &str {
        "replicate"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn poll_interval_ms(&self, _progress: &JobProgress) -> u64 {
        2_000
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(self.create(job))
    }
    fn poll<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(self.progress(h))
    }
    fn cancel<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let record = Record::read(h)?;
            self.check_id(&h.remote_id)?;
            let req = self.authenticate(HttpRequest {
                method: HttpMethod::Post,
                ..HttpRequest::get(self.endpoint(&format!("predictions/{}/cancel", h.remote_id))).max_response_bytes(self.limits.response_bytes)
            });
            let reply = self.http.send_no_redirect(req).await.map_err(transport)?;
            let parsed = response::parse(&reply, record, self.limits)?;
            parsed.identity(Some(&h.remote_id), record)?;
            Ok(())
        })
    }
}
#[cfg(test)]
thread_local! {static OBS:std::cell::RefCell<[usize;7]>=const{std::cell::RefCell::new([0;7])};}
fn observe(n: usize) {
    #[cfg(test)]
    OBS.with(|m| {
        if let Some(v) = m.borrow_mut().get_mut(n) {
            *v = v.saturating_add(1);
        }
    });
    #[cfg(not(test))]
    let _ = n;
}

#[cfg(all(test, feature = "http", not(target_arch = "wasm32")))]
mod native_tests;

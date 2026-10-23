//! The broker's [`Provider`] (#174): sends what [`super::plan`] allows, and nothing else.
//!
//! Every request goes out without following redirects, so neither the credential nor a job leaves
//! the template's origins on the service's word. The credential goes only to the API origin
//! ([`authorize`]); outputs are fetched from the output origins without it. The service is
//! unknown to the host, so no reply proves a submit created nothing: once the paid request is
//! sent, every failure may hide a billed job, and an ambiguous submit is retried automatically
//! only when the template names an idempotency header for the key.

use std::sync::Arc;

use nodecraft_types::Money;

use super::plan::{authorize, plan_cancel, plan_poll, plan_submit, read_poll, read_submit};
use super::template::Template;
use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission,
    SubmitContext,
};

/// The most bytes one output download may hold.
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
/// The most bytes all of one job's downloads may hold together.
pub const MAX_JOB_BYTES: usize = 512 * 1024 * 1024;
const DOWNLOAD_TIMEOUT_MS: u64 = 300_000;
const REDACTED: &str = "[redacted]";

fn unsupported(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, message)
}

fn malformed(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, message)
}

/// A plugin's provider, run by the host from its template. Constructing it sends nothing.
pub struct Broker {
    id: String,
    template: Template,
    key: Option<Secret>,
    http: Arc<dyn Http>,
    /// Whether the grant it runs under still stands; asked right before a paid request.
    granted: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl Broker {
    /// A broker for `template` under the provider id `id`. A template that places a credential
    /// needs `key`; one that doesn't is never given one. In the browser this is refused: `fetch`
    /// follows redirects itself, so the host can't keep a credential on its origin there.
    pub fn new(id: impl Into<String>, template: Template, key: Option<Secret>, http: Arc<dyn Http>) -> Result<Self, ProviderError> {
        if cfg!(target_arch = "wasm32") {
            return Err(unsupported("plugin providers need the desktop app; the browser can't keep their credential on its origin"));
        }
        let key = match (template.credential, key) {
            (Some(_), Some(key)) => Some(key),
            (Some(_), None) => return Err(ProviderError::new(ErrorKind::Auth, "this plugin provider needs an API key; add it in Settings")),
            (None, _) => None,
        };
        Ok(Self { id: id.into(), template, key, http, granted: Arc::new(|| true) })
    }

    /// This broker sending a paid request only while `granted` says its grant stands.
    pub fn granted(self, granted: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self { granted, ..self }
    }

    fn key(&self) -> &str {
        self.key.as_ref().map(Secret::expose).unwrap_or_default()
    }

    /// Whether `text` holds the credential.
    fn leaks(&self, text: &str) -> bool {
        self.key.as_ref().is_some_and(|k| text.contains(k.expose()))
    }

    /// `error` with the credential taken out of its message.
    fn scrub(&self, mut error: ProviderError) -> ProviderError {
        if let Some(key) = &self.key
            && error.message.contains(key.expose())
        {
            error.message = error.message.replace(key.expose(), REDACTED);
        }
        error
    }

    /// Whether `body` holds the credential, as sent or JSON-escaped.
    fn echoes(&self, body: &[u8]) -> bool {
        let Some(key) = &self.key else { return false };
        let escaped = serde_json::to_string(key.expose()).unwrap_or_default();
        let escaped = escaped.get(1..escaped.len().saturating_sub(1)).unwrap_or_default();
        [key.expose(), escaped].iter().any(|k| !k.is_empty() && body.windows(k.len()).any(|w| w == k.as_bytes()))
    }

    /// An error for a non-2xx reply. A reply that echoes the credential is not quoted at all, so
    /// not even the part of it an excerpt would cut keeps.
    fn status_error(&self, reply: &HttpResponse) -> ProviderError {
        let mut error = ProviderError::from_status(reply.status, if self.echoes(&reply.body) { b"" } else { &reply.body });
        if (300..400).contains(&reply.status) {
            error = ProviderError::new(
                ErrorKind::Malformed,
                format!("the plugin provider answered with a redirect (HTTP {}), which the host doesn't follow", reply.status),
            );
        }
        error.retry_after_ms = error.retry_after_ms.or_else(|| retry_after(reply)).filter(|n| *n <= 3_600_000);
        self.scrub(error)
    }

    /// Sends `request` to the API origin with the credential.
    async fn api(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        let request = authorize(&self.template, request, self.key())?;
        self.http.send_no_redirect(request).await.map_err(|e| self.scrub(e))
    }

    async fn create(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Submission, ProviderError> {
        let request = authorize(&self.template, plan_submit(&self.template, job)?, self.key())?;
        // The last moment the job can still be refused without being sent.
        if !(self.granted)() {
            return Err(ProviderError::new(ErrorKind::Auth, "this plugin's grant was withdrawn; nothing was sent"));
        }
        // Everything local is done. This service has no documented list of replies that create
        // no job, so from here every failure may hide one it bills.
        if let Some(context) = context {
            context.paid();
        }
        let billed = |e: ProviderError| {
            let mut e = self.scrub(e);
            e.message = format!("{}; the job may have been accepted and billed", e.message);
            e
        };
        let reply = self.http.send_no_redirect(request).await.map_err(billed)?;
        if !reply.is_success() {
            return Err(billed(self.status_error(&reply)));
        }
        let submission = read_submit(&self.template, &job.model, &reply.body).map_err(billed)?;
        drop(reply);
        match submission {
            Submission::Queued(handle) if self.leaks(&handle.remote_id) => Err(billed(malformed("the provider's job id carries the credential"))),
            Submission::Queued(handle) => Ok(Submission::Queued(handle)),
            Submission::Finished(done) => {
                // The downloads are further requests: a refusal of one says nothing of the job.
                if let Some(context) = context {
                    context.unknown();
                }
                let billed = |e: ProviderError| {
                    let mut e = self.scrub(e);
                    e.message = format!("{}; the job was billed", e.message);
                    e
                };
                Ok(Submission::Finished(self.fetch(done).await.map_err(billed)?))
            }
        }
    }

    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        if self.leaks(&handle.remote_id) {
            return Err(malformed("the job's id carries the credential"));
        }
        let reply = self.api(plan_poll(&self.template, handle)?).await?;
        if !reply.is_success() {
            return Err(self.status_error(&reply));
        }
        match read_poll(&self.template, &reply.body, self.key()).map_err(|e| self.scrub(e))? {
            JobProgress::Done(done) => {
                drop(reply);
                // The job is billed: a download that may work next time (the next poll may hand
                // out a fresh link) is retried; one whose output is wrong is not.
                self.fetch(done).await.map(JobProgress::Done).map_err(|e| {
                    let mut e = self.scrub(e);
                    if e.kind.is_transient() {
                        e.kind = ErrorKind::Server;
                    }
                    e.message = format!("{}; the job was billed", e.message);
                    e
                })
            }
            JobProgress::Failed(e) => Ok(JobProgress::Failed(self.scrub(e))),
            other => Ok(other),
        }
    }

    /// `done` with every link fetched into bytes; no output may carry the credential.
    async fn fetch(&self, done: Completed) -> Result<Completed, ProviderError> {
        let mut outputs = Vec::with_capacity(done.outputs.len());
        let mut budget = MAX_JOB_BYTES;
        for output in done.outputs {
            outputs.push(match output {
                Output::Text(text) if self.leaks(&text) => return Err(malformed("an output in the provider's reply carries the credential")),
                Output::Url { url, .. } if self.leaks(&url) => return Err(malformed("an output link in the provider's reply carries the credential")),
                Output::Url { url, .. } => self.download(&url, &mut budget).await?,
                other => other,
            });
        }
        Ok(Completed { outputs, ..done })
    }

    /// An output from one of the template's origins, fetched without the credential. Its type is
    /// what its bytes are, not what the service or the template says.
    /// `budget` is what the job's outputs may still take, and loses what this one does.
    async fn download(&self, url: &str, budget: &mut usize) -> Result<Output, ProviderError> {
        let parsed = url::Url::parse(url).map_err(|_| malformed("an output link in the provider's reply isn't a URL"))?;
        if !self.template.origins().any(|o| o.holds(&parsed)) || !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(malformed("an output link in the provider's reply is outside its template's origins"));
        }
        let request = HttpRequest::get(parsed.as_str()).max_response_bytes(MAX_OUTPUT_BYTES.min(*budget)).timeout_ms(DOWNLOAD_TIMEOUT_MS);
        let reply = self.http.send_no_redirect(request).await.map_err(|e| self.scrub(e))?;
        if !reply.is_success() {
            let mut error = ProviderError::new(ErrorKind::Server, format!("the plugin provider's output download failed (HTTP {})", reply.status));
            error.retry_after_ms = retry_after(&reply).filter(|n| *n <= 3_600_000);
            return Err(error);
        }
        if reply.body.len() > MAX_OUTPUT_BYTES {
            return Err(malformed("the plugin provider's output is larger than 256 MiB"));
        }
        *budget = budget.checked_sub(reply.body.len()).ok_or_else(|| malformed("the plugin provider's outputs for one job are larger than 512 MiB"))?;
        // A link on the API origin may serve what the credential let it see.
        if self.echoes(&reply.body) {
            return Err(malformed("an output the plugin provider served carries the credential"));
        }
        // Not quoted when it isn't media: the body is the service's, and may echo anything.
        let sniffed = nodecraft_media::sniff(&reply.body).ok_or_else(|| malformed("the plugin provider's output is not a known media type"))?;
        Ok(Output::Bytes { mime: sniffed.mime.to_owned(), bytes: Arc::from(&reply.body[..]) })
    }
}

fn retry_after(reply: &HttpResponse) -> Option<u64> {
    reply.header("retry-after").and_then(|v| v.trim().parse::<u64>().ok()).and_then(|s| s.checked_mul(1000))
}

impl Provider for Broker {
    fn id(&self) -> &str {
        &self.id
    }

    /// Only a template that sends the job's idempotency key lets an ambiguous submit be sent
    /// again: the service then answers with the job it may already have.
    fn retry_ambiguous_submit(&self) -> bool {
        self.template.idempotency.is_some()
    }

    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(self.create(job, None))
    }

    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        // Planning is pure; create marks the paid request before it sends it.
        context.arm();
        Box::pin(self.create(job, Some(context)))
    }

    /// The template's price for the model: what one job costs at least, asked of no one.
    fn quote<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Money, ProviderError>> {
        let price = self.template.model(&job.model).map(|m| m.price).ok_or_else(|| unsupported("this plugin provider doesn't serve that model"));
        Box::pin(async move { price })
    }

    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(self.progress(handle))
    }

    fn cancel<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let Some(request) = plan_cancel(&self.template, handle)? else {
                return Err(unsupported("this plugin provider can't cancel a job; it runs to the end and is billed"));
            };
            let reply = self.api(request).await?;
            if !reply.is_success() {
                return Err(self.status_error(&reply));
            }
            // A cancel the service accepts is no promise of a refund.
            Ok(())
        })
    }

    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        self.template.poll.as_ref().map_or(2_000, |p| p.interval_ms)
    }
}

//! Direct Google text generation through its documented OpenAI-compatible endpoint (#44).
//! Reuses the bounded chat/stream implementation; no new wire dialect or gateway. A job that
//! carries a video goes through the Interactions API instead ([`video`], #84): the compatible
//! endpoint documents no video input.
use crate::llm::{ApiKey, Endpoint, LlmClient};
use crate::secrets::Secret;
use crate::{
    BoxFuture, ErrorKind, Http, HttpMethod, HttpRequest, JobHandle, JobInput, JobProgress, Provider, ProviderError, ProviderJob, Submission, SubmitContext,
};
use std::sync::Arc;

pub mod video;

pub const MODEL: &str = "gemini-3.8-flash";
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_TOKENS: u64 = 65_536;
pub const DEFAULT_TOKENS: u64 = 4096;

pub struct GeminiLlm {
    chat: LlmClient,
    key: Secret,
    http: Arc<dyn Http>,
}

fn invalid() -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, "Gemini text needs Gemini 3.8 Flash, at most 64 text inputs totaling 1 MiB, and max_tokens from 1 to 65536")
}

fn key_fits(job: &ProviderJob) -> Result<(), ProviderError> {
    if job.idempotency_key.len() > 256 {
        return Err(ProviderError::new(ErrorKind::UnsupportedInput, "Gemini text request keys must be at most 256 bytes"));
    }
    Ok(())
}

/// The requested answer limit, or the default.
fn max_tokens(job: &ProviderJob) -> Result<u64, ProviderError> {
    match job.params.get("max_tokens") {
        None => Ok(DEFAULT_TOKENS),
        Some(value) => value.as_u64().filter(|n| (1..=MAX_TOKENS).contains(n)).ok_or_else(invalid),
    }
}

fn prepare(job: &ProviderJob) -> Result<ProviderJob, ProviderError> {
    key_fits(job)?;
    if job.model != MODEL || job.inputs.is_empty() || job.inputs.len() > 64 || job.params.keys().any(|key| key != "max_tokens") {
        return Err(invalid());
    }
    let mut bytes = 0usize;
    for input in &job.inputs {
        let JobInput::Text { role, text } = input else { return Err(invalid()) };
        if !matches!(role.as_str(), "prompt" | "user" | "system" | "assistant") {
            return Err(invalid());
        }
        bytes = bytes.checked_add(text.len()).filter(|n| *n <= MAX_TEXT_BYTES).ok_or_else(invalid)?;
    }
    let cap = max_tokens(job)?;
    let mut checked = job.clone();
    checked.params.insert("max_tokens".into(), cap.into());
    Ok(checked)
}

/// A video job's one Interactions request, checked and built; nothing is sent.
fn video_request(job: &ProviderJob, key: &Secret) -> Result<HttpRequest, ProviderError> {
    key_fits(job)?;
    if job.params.keys().any(|key| key != "max_tokens") {
        return Err(invalid());
    }
    let body = video::body(job, max_tokens(job)?)?;
    Ok(HttpRequest { method: HttpMethod::Post, body, ..HttpRequest::get(video::URL) }
        .header("Content-Type", "application/json")
        .header("User-Agent", crate::USER_AGENT)
        .secret_header("x-goog-api-key", key.expose())
        .timeout_ms(video::REQUEST_TIMEOUT_MS)
        .max_response_bytes(video::MAX_RESPONSE_BYTES))
}

impl GeminiLlm {
    pub fn new(key: &Secret, http: Arc<dyn Http>) -> Self {
        Self {
            chat: LlmClient::new(
                Endpoint::openai_compatible("gemini-llm", "https://generativelanguage.googleapis.com/v1beta/openai", Some(ApiKey::from(key))),
                http.clone(),
            ),
            key: key.clone(),
            http,
        }
    }

    /// Sends a video job's one request. From the moment it is sent a lost reply may hide a
    /// billed answer, so it is never replayed.
    async fn video(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Submission, ProviderError> {
        let request = video_request(job, &self.key)?;
        if let Some(context) = context {
            context.unknown();
        }
        let reply = self.http.send_no_redirect(request).await.map_err(|e| ProviderError {
            kind: e.kind,
            message: "the Gemini video request failed in transport; it may have been billed".into(),
            retry_after_ms: e.retry_after_ms.filter(|n| *n <= 3_600_000),
        })?;
        Ok(Submission::Finished(video::parse(&reply)?))
    }
}

impl Provider for GeminiLlm {
    fn id(&self) -> &str {
        "gemini-llm"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        if video::carries_video(job) {
            return Box::pin(self.video(job, None));
        }
        Box::pin(async move { self.chat.submit(&prepare(job)?).await })
    }
    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.arm();
        if video::carries_video(job) {
            // Not streamed: the answer is delivered whole when the request returns.
            return Box::pin(self.video(job, Some(context)));
        }
        Box::pin(async move { self.chat.submit_with_delivery(&prepare(job)?, context).await })
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        self.chat.poll(handle)
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::UnsupportedInput, "A sent Gemini text request cannot confirm remote cancellation")) })
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod video_tests;

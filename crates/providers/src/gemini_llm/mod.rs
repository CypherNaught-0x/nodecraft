//! Direct Google text generation through its documented OpenAI-compatible endpoint (#44).
//! Reuses the bounded chat/stream implementation; no new wire dialect or gateway.
use crate::llm::{ApiKey, Endpoint, LlmClient};
use crate::secrets::Secret;
use crate::{BoxFuture, ErrorKind, Http, JobHandle, JobInput, JobProgress, Provider, ProviderError, ProviderJob, Submission, SubmitContext};
use std::sync::Arc;

pub const MODEL: &str = "gemini-3.8-flash";
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_TOKENS: u64 = 65_536;
pub const DEFAULT_TOKENS: u64 = 4096;

pub struct GeminiLlm(LlmClient);

fn invalid() -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, "Gemini text needs Gemini 3.8 Flash, at most 64 text inputs totaling 1 MiB, and max_tokens from 1 to 65536")
}

fn prepare(job: &ProviderJob) -> Result<ProviderJob, ProviderError> {
    if job.idempotency_key.len() > 256 {
        return Err(ProviderError::new(ErrorKind::UnsupportedInput, "Gemini text request keys must be at most 256 bytes"));
    }
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
    let cap = match job.params.get("max_tokens") {
        None => DEFAULT_TOKENS,
        Some(value) => value.as_u64().filter(|n| (1..=MAX_TOKENS).contains(n)).ok_or_else(invalid)?,
    };
    let mut checked = job.clone();
    checked.params.insert("max_tokens".into(), cap.into());
    Ok(checked)
}

impl GeminiLlm {
    pub fn new(key: &Secret, http: Arc<dyn Http>) -> Self {
        Self(LlmClient::new(
            Endpoint::openai_compatible("gemini-llm", "https://generativelanguage.googleapis.com/v1beta/openai", Some(ApiKey::from(key))),
            http,
        ))
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
        Box::pin(async move { self.0.submit(&prepare(job)?).await })
    }
    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.arm();
        Box::pin(async move { self.0.submit_with_delivery(&prepare(job)?, context).await })
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        self.0.poll(handle)
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::UnsupportedInput, "A sent Gemini text request cannot confirm remote cancellation")) })
    }
}

#[cfg(test)]
mod tests;

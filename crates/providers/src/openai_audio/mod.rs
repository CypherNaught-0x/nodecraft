//! OpenAI's synchronous speech and file transcription APIs (#39). The speech model is
//! supported only until 2027-01-06; Realtime synthesis is a different, unsupported API.
mod request;
mod response;
#[cfg(test)]
mod tests;

use crate::secrets::Secret;
use crate::{BoxFuture, Completed, ErrorKind, Http, JobHandle, JobProgress, Provider, ProviderError, ProviderJob, Submission, SubmitContext};
use std::sync::Arc;

pub const SPEECH: &str = "gpt-4o-mini-tts-2025-12-15";
pub const TRANSCRIBE: &str = "gpt-transcribe";
const ROOT: &str = "https://api.openai.com/v1/audio";
const INPUT_BYTES: usize = 25_000_000;
const OUTPUT_BYTES: usize = 32 * 1024 * 1024;
const TRANSCRIPT_BYTES: usize = 4 * 1024 * 1024;
fn invalid(message: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, message)
}
fn malformed(message: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, message)
}

pub struct OpenAiAudio {
    key: Secret,
    http: Arc<dyn Http>,
}
impl OpenAiAudio {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http }
    }
    async fn perform(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Completed, ProviderError> {
        let request = request::prepare(job)?
            .header("User-Agent", crate::USER_AGENT)
            .secret_header("Authorization", format!("Bearer {}", self.key.expose()))
            .timeout_ms(600_000)
            .max_response_bytes(if job.model == SPEECH { OUTPUT_BYTES } else { TRANSCRIPT_BYTES });
        if let Some(context) = context {
            context.paid();
        }
        // Fixed origin, no redirects or transparent replay. Errors are deliberately generic:
        // transports and providers may echo a key, prompt or recording name in their details.
        let reply = self.http.send_no_redirect(request).await.map_err(|error| ProviderError {
            kind: error.kind,
            message: "OpenAI audio request failed in transport; it may have been billed".into(),
            retry_after_ms: error.retry_after_ms.filter(|ms| *ms <= 3_600_000),
        })?;
        if response::refusal_kind(&reply).is_some()
            && let Some(context) = context
        {
            context.refused();
        }
        response::parse(reply, &job.model)
    }
}
impl Provider for OpenAiAudio {
    fn id(&self) -> &str {
        "openai-audio"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move { self.perform(job, None).await.map(Submission::Finished) })
    }
    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.arm();
        Box::pin(async move { self.perform(job, Some(context)).await.map(Submission::Finished) })
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "OpenAI audio completes at submit; there is no remote job to poll")) })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("OpenAI audio cannot cancel a sent synchronous request")) })
    }
}

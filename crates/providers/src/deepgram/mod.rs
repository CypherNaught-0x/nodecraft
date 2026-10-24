//! Bounded prerecorded Nova-3 transcription, using the shared synchronous job runner (#40).
mod response;
#[cfg(test)]
mod tests;

use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpMethod, HttpRequest, JobHandle, JobInput, JobProgress, Provider, ProviderError, ProviderJob, Submission,
    SubmitContext,
};
use std::sync::Arc;

pub const MODEL: &str = "nova-3";
pub const MAX_INPUT_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TRANSCRIPT_BYTES: usize = 1024 * 1024;
const URL: &str = "https://api.deepgram.com/v1/listen?model=nova-3&language=en&smart_format=true&multichannel=false&mip_opt_out=true";

fn invalid() -> ProviderError {
    ProviderError::new(
        ErrorKind::UnsupportedInput,
        "Deepgram needs one WAV, MP3, FLAC or Ogg audio input, at most 25 MiB and 30 minutes, with no prompt or parameters",
    )
}
fn malformed() -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, "Deepgram returned an invalid or oversized transcription response")
}

fn request(job: &ProviderJob) -> Result<HttpRequest, ProviderError> {
    if job.model != MODEL || !job.params.is_empty() || job.idempotency_key.len() > 256 {
        return Err(invalid());
    }
    let [JobInput::Media { role, mime, bytes }] = job.inputs.as_slice() else { return Err(invalid()) };
    if role != "audio" || bytes.is_empty() || bytes.len() > MAX_INPUT_BYTES {
        return Err(invalid());
    }
    let actual = nodecraft_media::sniff(bytes).map(|value| value.mime).ok_or_else(invalid)?;
    if !matches!(actual, "audio/wav" | "audio/mpeg" | "audio/flac" | "audio/ogg") || mime != actual {
        return Err(invalid());
    }
    // Bounded header inspection, not a decoder: Deepgram validates the encoded payload.
    let sound = nodecraft_media::probe_sound(bytes);
    if !sound.duration_ms.is_some_and(|n| (1..=1_800_000).contains(&n))
        || !sound.channels.is_some_and(|n| (1..=2).contains(&n))
        || !sound.sample_rate.is_some_and(|n| (8_000..=192_000).contains(&n))
    {
        return Err(invalid());
    }
    let mut body = Vec::new();
    body.try_reserve_exact(bytes.len()).map_err(|_| invalid())?;
    body.extend_from_slice(bytes);
    Ok(HttpRequest { method: HttpMethod::Post, body, ..HttpRequest::get(URL) }
        .header("Content-Type", actual)
        .header("User-Agent", crate::USER_AGENT)
        .timeout_ms(600_000)
        .max_response_bytes(MAX_RESPONSE_BYTES))
}

pub struct Deepgram {
    key: Secret,
    http: Arc<dyn Http>,
}
impl Deepgram {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http }
    }

    async fn perform(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Completed, ProviderError> {
        let req = request(job)?.secret_header("Authorization", format!("Token {}", self.key.expose()));
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.http.send_no_redirect(req).await.map_err(|error| ProviderError {
            kind: error.kind,
            message: "Deepgram transport failed; transcription may still run and be billed".into(),
            retry_after_ms: error.retry_after_ms.filter(|n| *n <= 3_600_000),
        })?;
        response::parse(reply, &self.key)
    }
}
impl Provider for Deepgram {
    fn id(&self) -> &str {
        "deepgram"
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
        Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "Deepgram file transcription has no resumable handle")) })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::UnsupportedInput, "A sent Deepgram transcription cannot confirm remote cancellation or refund")) })
    }
}

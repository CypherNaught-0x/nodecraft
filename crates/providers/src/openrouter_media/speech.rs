//! Text to speech: `{model, input, voice?, response_format: "mp3"}` answered with the MP3 bytes.
use super::{API, MIB, SPEECH_ID, invalid, malformed, model_ok, prompt, sent, short_value};
use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpMethod, HttpRequest, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission,
    SubmitContext,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// OpenRouter's longest speech input it documents (a transcript).
pub const MAX_CHARS: usize = 10_000;
/// The largest MP3 accepted: two hours at 128 kbps is about 115 MiB.
const MAX_AUDIO: usize = 128 * MIB;

pub struct OpenRouterSpeech {
    key: Secret,
    http: Arc<dyn Http>,
}

impl OpenRouterSpeech {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http }
    }

    fn body(job: &ProviderJob) -> Result<Vec<u8>, ProviderError> {
        if !model_ok(&job.model) {
            return Err(invalid("the OpenRouter speech model id is not one OpenRouter could name"));
        }
        if let Some(name) = job.params.keys().find(|name| name.as_str() != "voice") {
            return Err(invalid(format!("OpenRouter speech models take no setting called `{name}`")));
        }
        if job.inputs.iter().any(|input| !matches!(input, crate::JobInput::Text { role, .. } if role == "prompt")) {
            return Err(invalid("OpenRouter speech models read one prompt and nothing else"));
        }
        let mut body = Map::new();
        body.insert("model".into(), json!(job.model));
        body.insert("input".into(), json!(prompt(job, MAX_CHARS)?));
        // Some models document a default voice and list none.
        if let Some(voice) = job.params.get("voice") {
            body.insert("voice".into(), json!(short_value("voice", voice, 128)?));
        }
        body.insert("response_format".into(), json!("mp3"));
        serde_json::to_vec(&Value::Object(body)).map_err(|_| invalid("the OpenRouter speech request couldn't be written"))
    }

    async fn perform(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Completed, ProviderError> {
        let body = Self::body(job)?;
        let request = HttpRequest { method: HttpMethod::Post, body, ..HttpRequest::get(format!("{API}/audio/speech")) }
            .header("Content-Type", "application/json")
            .secret_header("Authorization", format!("Bearer {}", self.key.expose()))
            .timeout_ms(300_000)
            .max_response_bytes(MAX_AUDIO);
        if let Some(context) = context {
            context.unknown();
        }
        let reply = self.http.send_no_redirect(request).await.map_err(|error| sent(error, "The OpenRouter speech request"))?;
        if reply.status != 200 {
            return Err(ProviderError::from_status(reply.status, &reply.body));
        }
        let audio = crate::check_media(&reply.body, Some(nodecraft_media::MediaKind::Audio))
            .map_err(|_| malformed("OpenRouter's speech reply isn't audio NodeCraft can read"))?;
        Ok(Completed { outputs: vec![Output::Bytes { mime: audio.mime.to_string(), bytes: Arc::from(audio.bytes) }], usage: crate::Usage::default() })
    }
}

impl Provider for OpenRouterSpeech {
    fn id(&self) -> &str {
        SPEECH_ID
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
        Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "OpenRouter speech finishes at submit; there is nothing to poll")) })
    }

    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("OpenRouter can't cancel a speech request once it is sent")) })
    }
}

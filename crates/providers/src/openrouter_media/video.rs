//! Video jobs: create, poll, download. The handle keeps only the job id and model.
use super::{API, MIB, VIDEO_ID, invalid, malformed, model_ok, prompt, sent, short_value, whole};
use crate::secrets::Secret;
use crate::{
    BoxFuture, Completed, ErrorKind, Http, HttpMethod, HttpRequest, JobHandle, JobInput, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission,
    SubmitContext, Usage,
};
use base64::Engine;
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// Settings passed through, as the model's listing names them.
pub const PARAMS: [&str; 5] = ["duration", "resolution", "aspect_ratio", "generate_audio", "seed"];
const MAX_CHARS: usize = 8_000;
const MAX_FRAME: usize = 10 * MIB;
const MAX_JSON: usize = MIB;
const MAX_VIDEO: usize = 512 * MIB;
const FRAME_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

pub struct OpenRouterVideo {
    key: Secret,
    http: Arc<dyn Http>,
}

/// A job id as OpenRouter returns them: short, URL-safe, never the key.
fn job_id(id: &str, key: &Secret) -> Result<(), ProviderError> {
    let ok = !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)) && !id.contains(key.expose());
    if ok { Ok(()) } else { Err(malformed("OpenRouter returned a video job id NodeCraft can't use")) }
}

fn frame(role: &str, mime: &str, bytes: &[u8]) -> Result<Value, ProviderError> {
    if !FRAME_TYPES.contains(&mime) || bytes.len() > MAX_FRAME {
        return Err(invalid("a frame must be a PNG, JPEG or WebP of at most 10 MiB"));
    }
    let limits = nodecraft_media::Limits { max_input_bytes: MAX_FRAME as u64, max_width: 8192, max_height: 8192, max_alloc: 512 * MIB as u64 };
    let meta = nodecraft_media::probe_image(bytes, &limits).map_err(|_| invalid("a frame image is corrupt or too large to read"))?;
    if meta.mime != mime {
        return Err(invalid("a frame image's contents don't match its type"));
    }
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{data}")}, "frame_type": role}))
}

/// What OpenRouter says went wrong, bounded and on one line.
fn reason(value: Option<&Value>) -> String {
    let text = match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Object(object)) => object.get("message").and_then(Value::as_str).unwrap_or("no details").to_owned(),
        _ => "no details".into(),
    };
    let line: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(300).collect();
    line.trim().to_owned()
}

impl OpenRouterVideo {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http }
    }

    fn body(job: &ProviderJob) -> Result<Vec<u8>, ProviderError> {
        if !model_ok(&job.model) {
            return Err(invalid("the OpenRouter video model id is not one OpenRouter could name"));
        }
        if let Some(name) = job.params.keys().find(|name| !PARAMS.contains(&name.as_str())) {
            return Err(invalid(format!("OpenRouter video models take no setting called `{name}`")));
        }
        let mut body = Map::new();
        body.insert("model".into(), json!(job.model));
        body.insert("prompt".into(), json!(prompt(job, MAX_CHARS)?));
        if let Some(value) = job.params.get("duration") {
            body.insert("duration".into(), json!(whole("duration", value, 600)?));
        }
        for name in ["resolution", "aspect_ratio"] {
            if let Some(value) = job.params.get(name) {
                body.insert(name.into(), json!(short_value(name, value, 16)?));
            }
        }
        if let Some(value) = job.params.get("generate_audio") {
            let on = value.as_bool().ok_or_else(|| invalid("the setting `generate_audio` is on or off"))?;
            body.insert("generate_audio".into(), json!(on));
        }
        if let Some(value) = job.params.get("seed") {
            body.insert("seed".into(), json!(whole("seed", value, 2_147_483_647)?));
        }
        let mut frames = Vec::new();
        for input in &job.inputs {
            match input {
                JobInput::Text { role, .. } if role == "prompt" => {}
                JobInput::Media { role, mime, bytes } if matches!(role.as_str(), "first_frame" | "last_frame") => {
                    if frames.iter().any(|f: &Value| f.get("frame_type").and_then(Value::as_str) == Some(role.as_str())) {
                        return Err(invalid(format!("OpenRouter video takes one {}", role.replace('_', " "))));
                    }
                    frames.push(frame(role, mime, bytes)?);
                }
                _ => return Err(invalid("OpenRouter video models take a prompt and optional first and last frames")),
            }
        }
        if !frames.is_empty() {
            body.insert("frame_images".into(), Value::Array(frames));
        }
        serde_json::to_vec(&Value::Object(body)).map_err(|_| invalid("the OpenRouter video request couldn't be written"))
    }

    fn authorized(&self, request: HttpRequest) -> HttpRequest {
        request.secret_header("Authorization", format!("Bearer {}", self.key.expose()))
    }

    async fn create(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Submission, ProviderError> {
        let body = Self::body(job)?;
        let request = self.authorized(
            HttpRequest { method: HttpMethod::Post, body, ..HttpRequest::get(format!("{API}/videos")) }
                .header("Content-Type", "application/json")
                .timeout_ms(120_000)
                .max_response_bytes(MAX_JSON),
        );
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.http.send_no_redirect(request).await.map_err(|error| sent(error, "The OpenRouter video request"))?;
        if !(200..=202).contains(&reply.status) {
            return Err(ProviderError::from_status(reply.status, &reply.body));
        }
        let created: Value = serde_json::from_slice(&reply.body).map_err(|_| malformed("OpenRouter's video reply isn't the JSON its API describes"))?;
        let id = created.get("id").and_then(Value::as_str).ok_or_else(|| malformed("OpenRouter's video reply has no job id"))?;
        job_id(id, &self.key)?;
        Ok(Submission::Queued(JobHandle { remote_id: id.to_owned(), data: json!({"v": 1, "model": job.model}) }))
    }

    fn check(&self, handle: &JobHandle) -> Result<(), ProviderError> {
        job_id(&handle.remote_id, &self.key)?;
        let data = handle.data.as_object().filter(|data| data.len() == 2).ok_or_else(|| invalid("OpenRouter video handle refused"))?;
        if data.get("v").and_then(Value::as_u64) != Some(1) || !data.get("model").and_then(Value::as_str).is_some_and(model_ok) {
            return Err(invalid("OpenRouter video handle refused"));
        }
        Ok(())
    }

    async fn progress(&self, handle: &JobHandle) -> Result<JobProgress, ProviderError> {
        self.check(handle)?;
        let id = &handle.remote_id;
        let request = self.authorized(HttpRequest::get(format!("{API}/videos/{id}")).timeout_ms(30_000).max_response_bytes(MAX_JSON));
        let reply = self.http.send_no_redirect(request).await?;
        if reply.status != 200 {
            return Err(ProviderError::from_status(reply.status, &reply.body));
        }
        let status: Value = serde_json::from_slice(&reply.body).map_err(|_| malformed("OpenRouter's video status isn't the JSON its API describes"))?;
        if status.get("id").and_then(Value::as_str).is_some_and(|other| other != id) {
            return Err(malformed("OpenRouter returned another video job's status"));
        }
        match status.get("status").and_then(Value::as_str) {
            Some("pending") => Ok(JobProgress::Queued { position: None }),
            Some("in_progress") => Ok(JobProgress::Running { fraction: None, message: None, partial: None }),
            Some("failed") => {
                Ok(JobProgress::Failed(ProviderError::new(ErrorKind::Server, format!("OpenRouter couldn't make the video: {}", reason(status.get("error"))))))
            }
            Some("cancelled" | "expired") => Ok(JobProgress::Failed(ProviderError::new(ErrorKind::Server, "OpenRouter cancelled the video job or it expired"))),
            Some("completed") => {
                let cost = status.get("usage").and_then(|u| u.get("cost")).and_then(Value::as_f64).and_then(super::usd);
                // The download needs the key; a redirect to storage never carries it to another host.
                let request = self.authorized(HttpRequest::get(format!("{API}/videos/{id}/content?index=0")).timeout_ms(600_000).max_response_bytes(MAX_VIDEO));
                let reply = self.http.send(request).await?;
                if reply.status != 200 {
                    return Err(ProviderError::new(
                        ErrorKind::Server,
                        format!("the OpenRouter video download failed (HTTP {}); resume to try again", reply.status),
                    ));
                }
                let video = crate::check_media(&reply.body, Some(nodecraft_media::MediaKind::Video))
                    .map_err(|_| malformed("OpenRouter's video isn't one NodeCraft can read"))?;
                Ok(JobProgress::Done(Completed {
                    outputs: vec![Output::Bytes { mime: video.mime.to_string(), bytes: Arc::from(video.bytes) }],
                    usage: Usage { cost, ..Usage::default() },
                }))
            }
            _ => Err(malformed("OpenRouter's video status is unknown")),
        }
    }
}

impl Provider for OpenRouterVideo {
    fn id(&self) -> &str {
        VIDEO_ID
    }

    fn retry_ambiguous_submit(&self) -> bool {
        false
    }

    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(self.create(job, None))
    }

    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.arm();
        Box::pin(self.create(job, Some(context)))
    }

    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(self.progress(handle))
    }

    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("OpenRouter has no way to cancel a video job; it finishes and is billed")) })
    }

    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        5_000
    }
}

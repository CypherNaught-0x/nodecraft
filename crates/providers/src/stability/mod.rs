//! Direct Stability Image Core, Audio 3 and Fast 3D over the common bounded HTTP/task host.
mod request;
mod response;
#[cfg(test)]
mod tests;

use crate::secrets::Secret;
use crate::{BoxFuture, Completed, ErrorKind, Http, HttpRequest, JobHandle, JobProgress, Provider, ProviderError, ProviderJob, Submission, SubmitContext};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const CORE: &str = "stable-image-core";
pub const AUDIO: &str = "stable-audio-3";
pub const FAST_3D: &str = "stable-fast-3d";
const API: &str = "https://api.stability.ai/v2beta";
const MIB: usize = 1024 * 1024;
const METADATA: usize = 64 * 1024;
const IMAGE_BYTES: usize = 16 * MIB;
// 380 s of stereo 44.1 kHz float32 WAV is 134,064,000 bytes before its small header.
const AUDIO_BYTES: usize = 128 * MIB;
const MESH_BYTES: usize = 64 * MIB;

fn invalid() -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, "Stability AI model, inputs or parameters are unsupported or exceed limits")
}
fn malformed() -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, "Stability AI returned invalid, mismatched or oversized media or metadata")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Model {
    Core,
    Audio,
    Mesh,
}
impl Model {
    fn read(value: &str) -> Result<Self, ProviderError> {
        match value {
            CORE => Ok(Self::Core),
            AUDIO => Ok(Self::Audio),
            FAST_3D => Ok(Self::Mesh),
            _ => Err(invalid()),
        }
    }
    fn cap(self) -> usize {
        match self {
            Self::Core => IMAGE_BYTES,
            Self::Audio => AUDIO_BYTES,
            Self::Mesh => MESH_BYTES,
        }
    }
    fn path(self) -> &'static str {
        match self {
            Self::Core => "stable-image/generate/core",
            Self::Audio => "audio/stable-audio/text-to-audio",
            Self::Mesh => "3d/stable-fast-3d",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioHandle {
    v: u8,
    duration: u16,
}
fn handle(value: &JobHandle) -> Result<AudioHandle, ProviderError> {
    if !valid_id(&value.remote_id) {
        return Err(invalid());
    }
    let data = AudioHandle::deserialize(&value.data).map_err(|_| invalid())?;
    if data.v != 1 || !(1..=380).contains(&data.duration) {
        return Err(invalid());
    }
    Ok(data)
}
fn valid_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub struct Stability {
    key: Secret,
    http: Arc<dyn Http>,
}
impl Stability {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http }
    }
    fn authorize(&self, request: HttpRequest) -> HttpRequest {
        request.secret_header("Authorization", format!("Bearer {}", self.key.expose())).header("User-Agent", crate::USER_AGENT).timeout_ms(600_000)
    }
    async fn perform(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Submission, ProviderError> {
        let prepared = request::prepare(job)?;
        let req = self.authorize(prepared.request);
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.http.send_no_redirect(req).await.map_err(response::transport)?;
        response::bounds(&reply, if prepared.model == Model::Audio { METADATA } else { prepared.model.cap() })?;
        if response::refusal(&reply).is_some()
            && let Some(context) = context
        {
            context.refused();
        }
        if prepared.model == Model::Audio {
            response::status(&reply, 202)?;
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Accepted {
                id: String,
            }
            let accepted: Accepted = serde_json::from_slice(&reply.body).map_err(|_| malformed())?;
            if !valid_id(&accepted.id) || accepted.id.contains(self.key.expose()) {
                return Err(malformed());
            }
            let data = serde_json::to_value(AudioHandle { v: 1, duration: prepared.duration }).map_err(|_| malformed())?;
            Ok(Submission::Queued(JobHandle { remote_id: accepted.id, data }))
        } else {
            response::status(&reply, 200)?;
            response::media(reply, prepared.model, None).map(Submission::Finished)
        }
    }
}
impl Provider for Stability {
    fn id(&self) -> &str {
        "stability"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move { self.perform(job, None).await })
    }
    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.arm();
        Box::pin(async move { self.perform(job, Some(context)).await })
    }
    fn poll<'a>(&'a self, value: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async move {
            let handle = handle(value)?;
            if value.remote_id.contains(self.key.expose()) {
                return Err(invalid());
            }
            let req = self
                .authorize(HttpRequest::get(format!("{API}/audio/results/{}", value.remote_id)).header("Accept", "audio/*").max_response_bytes(AUDIO_BYTES));
            let reply = self.http.send_no_redirect(req).await.map_err(response::transport)?;
            response::bounds(&reply, AUDIO_BYTES)?;
            if reply.status == 202 {
                if reply.body.len() > METADATA {
                    return Err(malformed());
                }
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Pending {
                    id: String,
                    status: String,
                }
                let pending: Pending = serde_json::from_slice(&reply.body).map_err(|_| malformed())?;
                if pending.id != value.remote_id || pending.status != "in-progress" {
                    return Err(malformed());
                }
                return Ok(JobProgress::Running { fraction: None, message: None, partial: None });
            }
            response::status(&reply, 200)?;
            response::media(reply, Model::Audio, Some(handle.duration)).map(JobProgress::Done)
        })
    }
    fn cancel<'a>(&'a self, value: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            let _ = handle(value)?;
            Err(ProviderError::new(
                ErrorKind::UnsupportedInput,
                "Stability AI has no documented cancellation endpoint; the generation may still run and be billed",
            ))
        })
    }
    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        10_000
    }
}

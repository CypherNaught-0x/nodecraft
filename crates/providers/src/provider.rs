//! The `Provider` trait every adapter implements, and the data it exchanges.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ProviderError;

/// A boxed future: `Send` on native targets, not in the browser (where `reqwest`'s futures aren't
/// `Send` and everything runs on one thread). Mirrors `exec::driver::TaskFuture`.
#[cfg(not(target_arch = "wasm32"))]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// `Send + Sync` on native targets, nothing in the browser.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSendSync: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync + ?Sized> MaybeSendSync for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSendSync {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSendSync for T {}

/// One input handed to a model.
#[derive(Debug, Clone, PartialEq)]
pub enum JobInput {
    /// A text input in a role such as `prompt`, `negative_prompt` or `system`.
    Text { role: String, text: String },
    /// A media input in a role such as `image`, `mask`, `start_frame` or `reference`.
    Media { role: String, mime: String, bytes: Arc<[u8]> },
}

/// A generation request for one model.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderJob {
    /// The provider's model id from the catalog (e.g. `fal-ai/flux/dev`).
    pub model: String,
    /// Parameters, already validated against the model's schema.
    pub params: Map<String, Value>,
    pub inputs: Vec<JobInput>,
    /// The same for every attempt of this job: send it where the API supports idempotency, so a
    /// retry after a lost reply never creates (and charges for) a second generation.
    pub idempotency_key: String,
}

/// A submitted job. Serialisable so a run can be resumed (polled again) after a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobHandle {
    /// The provider's id for the job (request id, prompt id, task id).
    pub remote_id: String,
    /// Whatever else the adapter needs to poll or cancel (status URLs, server, model).
    #[serde(default)]
    pub data: Value,
}

/// A result a job produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Text(String),
    /// Bytes the provider returned inline.
    Bytes {
        mime: String,
        bytes: Arc<[u8]>,
    },
    /// A file to download (with [`crate::fetch_output`]); `mime` when the provider said.
    Url {
        url: String,
        mime: Option<String>,
    },
}

/// Where a job is, from [`Provider::poll`].
#[derive(Debug, Clone, PartialEq)]
pub enum JobProgress {
    /// Waiting in the provider's queue; `position` when it says.
    Queued {
        position: Option<u32>,
    },
    /// Working. `fraction` is 0–1 when known; `partial` carries streamed text or a preview.
    Running {
        fraction: Option<f32>,
        message: Option<String>,
        partial: Option<Output>,
    },
    Done(Vec<Output>),
    Failed(ProviderError),
}

/// A model service: fal.ai, Replicate, OpenAI, a ComfyUI server, … Adapters live in their own
/// modules behind a Cargo feature each.
pub trait Provider: MaybeSendSync {
    /// Stable lowercase id (`fal`, `replicate`, `comfyui`); also the scheduler lane prefix.
    fn id(&self) -> &str;

    /// Starts a job. Must not wait for it to finish (beyond what the API forces).
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<JobHandle, ProviderError>>;

    /// Reports how a submitted job is doing.
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>>;

    /// Asks the provider to stop the job. Best effort: some APIs can't cancel.
    fn cancel<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>>;

    /// How long to wait between polls while the job is queued or running. Adapters for slow
    /// providers (video, 3D) return longer intervals.
    fn poll_interval_ms(&self, progress: &JobProgress) -> u64 {
        match progress {
            JobProgress::Queued { .. } => 1_000,
            _ => 500,
        }
    }
}

/// Cooperative cancellation as providers see it; the engine wraps exec's `CancelToken`.
pub trait CancelSignal: MaybeSendSync {
    fn is_cancelled(&self) -> bool;
    /// Resolves once cancelled.
    fn cancelled(&self) -> BoxFuture<'_, ()>;
}

/// Waiting, as providers see it; the engine wraps the app's runtime.
pub trait Sleep: MaybeSendSync {
    fn sleep(&self, ms: u64) -> BoxFuture<'static, ()>;
}

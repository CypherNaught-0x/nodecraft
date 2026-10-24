//! The `Provider` trait every adapter implements, and the data it exchanges.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use nodecraft_types::{BillableUnits, Money};
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
///
/// Chat models read text inputs in order as a conversation: `system` text is the system prompt,
/// `assistant` text is an earlier reply, and any other role (`prompt`, `user`, …) is the user's.
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

/// A paid request an adapter is about to send, saved first so it can be sent again unchanged,
/// under the same idempotency key, after a restart lost its reply (#664). Its body holds what the
/// provider is sent (prompt text, links to files already uploaded) and never a credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedSubmit {
    /// The request body exactly as sent.
    pub body: String,
    /// Whatever else the adapter needs to send it again and follow it (its handle data, say).
    #[serde(default)]
    pub data: Value,
}

/// Longest [`StagedSubmit::body`] an adapter stages or sends again.
pub const MAX_STAGED_BODY: usize = 1024 * 1024;

/// Saves a [`StagedSubmit`] before its request is sent. An error stops the request unsent, and is
/// the job's failure reason.
pub type Stager = dyn Fn(&StagedSubmit) -> Result<(), String> + Send + Sync;

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

/// What a job used, as far as the provider reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// What the provider says the job cost (LiteLLM, for example). `None` when it doesn't say;
    /// budgets then charge the estimate.
    pub cost: Option<Money>,
    /// How many of its own units the provider says it billed (fal's `X-Fal-Billable-Units`).
    /// Evidence for the result's record, never a cost: what a unit costs is the model's price.
    pub billable_units: Option<BillableUnits>,
}

/// A finished job: its outputs and what it used.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Completed {
    pub outputs: Vec<Output>,
    pub usage: Usage,
}

impl Completed {
    /// Outputs with no usage reported.
    pub fn new(outputs: Vec<Output>) -> Self {
        Self { outputs, usage: Usage::default() }
    }
}

/// What [`Provider::submit`] returns: queue-style APIs hand back a handle to poll; synchronous
/// ones (chat, OpenAI Images, TTS) finish within the request.
#[derive(Debug, Clone, PartialEq)]
pub enum Submission {
    Queued(JobHandle),
    Finished(Completed),
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
    Done(Completed),
    Failed(ProviderError),
}

/// A model service: fal.ai, Replicate, OpenAI, a ComfyUI server, … Adapters live in their own
/// modules behind a Cargo feature each.
pub trait Provider: MaybeSendSync {
    /// Stable lowercase id (`fal`, `replicate`, `comfyui`); also the scheduler lane prefix.
    fn id(&self) -> &str;

    /// A hosted implementation of the logical `comfyui` provider bills execution even when
    /// its catalog describes a local/free workflow. The engine captures this policy with the
    /// physical binding and requires unknown-price admission before submitting to that host.
    fn metered_comfy_workflows(&self) -> bool {
        false
    }

    /// Whether an uncertain, no-handle submit may be retried automatically. This is a stable
    /// policy of this provider instance, not remote deduplication or proof of nonbilling.
    /// The compatibility default preserves legacy adapters; paid synchronous adapters can refuse.
    fn retry_ambiguous_submit(&self) -> bool {
        true
    }

    /// Starts a job. Queue-style APIs return [`Submission::Queued`] without waiting; synchronous
    /// APIs return [`Submission::Finished`].
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>>;

    /// Evidence-aware runner entry. Legacy constructors may already have effects, so taint
    /// synchronously before invoking them. Only audited crate adapters can prove nonacceptance.
    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a crate::SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.unknown();
        self.submit(job)
    }

    /// Sends `staged` again exactly as it was staged through [`crate::SubmitContext`] before a
    /// restart (#664), under `job`'s idempotency key, so the provider answers with the request it
    /// may already have accepted rather than starting (and billing) another. `job` carries only
    /// its model and key. Unsupported, and nothing is sent, unless the adapter stages requests.
    fn replay<'a>(
        &'a self,
        job: &'a ProviderJob,
        staged: &'a StagedSubmit,
        context: &'a crate::SubmitContext,
    ) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        let _ = (job, staged, context);
        Box::pin(async { Err(ProviderError::new(crate::ErrorKind::UnsupportedInput, "this provider can't send a request saved before a restart again")) })
    }

    /// What this job would cost, asked of the provider before it is submitted (#663), for models
    /// the catalog prices `by_usage`. Must be free: it sends nothing billable and never uses the
    /// job's idempotency key, which callers leave empty. Unsupported unless the provider can say.
    fn quote<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Money, ProviderError>> {
        let _ = job;
        Box::pin(async { Err(ProviderError::new(crate::ErrorKind::UnsupportedInput, "this provider can't quote a job before it runs")) })
    }

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

/// Time, as providers see it; the engine wraps the app's runtime (exec's `Runtime`).
pub trait Clock: MaybeSendSync {
    fn sleep(&self, ms: u64) -> BoxFuture<'static, ()>;
    /// Milliseconds on a monotonic clock (any origin).
    fn now_ms(&self) -> u64;
}

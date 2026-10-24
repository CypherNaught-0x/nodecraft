#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft providers: the Provider trait and adapters to model services.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.
//!
//! - [`Provider`]: submit / poll / cancel, implemented by one module per service behind a Cargo
//!   feature each (CONTRIBUTING.md, "Adding a model or provider").
//! - [`ProviderError`] with the shared [`ErrorKind`]s, which say whether to retry.
//! - [`Http`]: the transport, with [`ReqwestHttp`] (feature `http`) for desktop and browser.
//! - [`run_to_completion`] / [`resume`] / [`fetch_output`]: the submit → poll → download loop
//!   queue-style APIs share.
//!
//! `exec` (same layer) can't depend on this crate or the other way round: the engine connects
//! them by implementing `exec::driver::JobRunner` with [`run_to_completion`], wrapping exec's
//! `CancelToken` as a [`CancelSignal`] and the app's runtime as a [`Clock`]. A failed run that
//! carries a handle ([`RunFailure::handle`]) must be resumed, never resubmitted.

pub mod broker;
#[cfg(feature = "comfy-cloud")]
pub mod comfy_cloud;
#[cfg(feature = "comfyui")]
pub mod comfyui;
mod delivery;
#[cfg(feature = "elevenlabs")]
pub mod elevenlabs;
mod error;
#[cfg(feature = "fal")]
pub mod fal;
#[cfg(feature = "gemini-images")]
pub mod gemini_images;
#[cfg(feature = "gemini-video")]
pub mod gemini_video;
#[cfg(feature = "google-cloud")]
pub mod google_cloud;
#[cfg(feature = "higgsfield")]
pub mod higgsfield;
pub mod http;
#[cfg(feature = "kling")]
pub mod kling;
#[cfg(feature = "llm")]
pub mod llm;
#[cfg(feature = "meshy")]
pub mod meshy;
/// Deterministic testing tools, available locally in unit tests or with feature `testkit`.
#[cfg(any(test, feature = "testkit"))]
pub mod mock;
#[cfg(any(feature = "tripo", feature = "meshy", feature = "higgsfield", feature = "seedance", feature = "kling"))]
mod model_file;
#[cfg(feature = "openai-audio")]
pub mod openai_audio;
#[cfg(feature = "openai-images")]
pub mod openai_images;
mod provider;
#[cfg(feature = "replicate")]
pub mod replicate;
pub mod secrets;
#[cfg(feature = "seedance")]
pub mod seedance;
#[cfg(feature = "seedream")]
pub mod seedream;
mod task;
#[cfg(feature = "tripo")]
pub mod tripo;

pub use delivery::{HttpFailure, MAX_STREAMED_TEXT, MAX_TEXT_DELTA, RequestContext, SubmitContext};
pub use error::{ErrorKind, ProviderError};
#[cfg(feature = "http")]
pub use http::ReqwestHttp;
pub use http::{ChunkSink, Http, HttpMethod, HttpRequest, HttpResponse, MAX_REDIRECTS, USER_AGENT};
pub use provider::{
    BoxFuture, CancelSignal, Clock, Completed, JobHandle, JobInput, JobProgress, MAX_STAGED_BODY, MaybeSendSync, Output, Provider, ProviderJob, StagedSubmit,
    Stager, Submission, Usage,
};
pub use task::{
    DEFAULT_JOB_DEADLINE_MS, Downloaded, RunFailure, RunLimits, RunUpdate, check_media, fetch_output, replay, resume, run_to_completion,
    run_to_completion_staged,
};

#[cfg(test)]
#[path = "mock/tests.rs"]
mod mock_tests;

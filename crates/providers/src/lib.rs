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
//! `CancelToken` as a [`CancelSignal`] and the app's runtime as [`Sleep`].

mod error;
pub mod http;
mod provider;
mod task;

pub use error::{ErrorKind, ProviderError};
#[cfg(feature = "http")]
pub use http::ReqwestHttp;
pub use http::{Http, HttpMethod, HttpRequest, HttpResponse, USER_AGENT};
pub use provider::{BoxFuture, CancelSignal, JobHandle, JobInput, JobProgress, MaybeSendSync, Output, Provider, ProviderJob, Sleep};
pub use task::{DEFAULT_JOB_DEADLINE_MS, Downloaded, RunLimits, RunUpdate, fetch_output, resume, run_to_completion};

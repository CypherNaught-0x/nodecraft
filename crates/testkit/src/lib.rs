#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft testkit: test fixtures, the mock provider and snapshot helpers (#79).
//!
//! - [`mock::MockProvider`]: a deterministic fake provider for text, image, video and audio
//!   models, scriptable per model (latency in polls, reported cost, failures at submit, poll or
//!   job), recording every request. Unit tests never call a real provider.
//! - [`kinds`]: generation node kinds that call it (`mock.text`, `mock.image`), and a registry
//!   with them and the built-in kinds.
//! - [`golden`]: runs a canvas end to end (plan, run executor, provider calls) and compares what
//!   each step produced with a blessed JSON file.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.
//! The testkit is a dev-dependency only.

pub mod golden;
pub mod kinds;
pub mod mock;

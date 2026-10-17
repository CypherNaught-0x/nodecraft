#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft exec: run planner, scheduler, queue, cancellation and cost quotes.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

mod payload;

pub mod cost;
pub mod driver;
pub mod planner;
pub mod scheduler;
pub mod staleness;

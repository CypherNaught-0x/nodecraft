#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft actions: deterministic image, video and text actions (CPU reference).
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

/// Custom text actions: a bounded, deterministic expression language (#165).
pub mod expr;
/// Bounded deterministic CPU image operations.
pub mod image;
/// Brush masks and canvas expansion for inpainting and outpainting (#102).
pub mod mask;
/// Bounded deterministic CPU text operations; no document mutation.
pub mod text;

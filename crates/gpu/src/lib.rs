#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft gpu: wgpu actions, custom WGSL actions and the 3D preview renderer.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

/// Bounded image compute backend.
pub mod image;
/// Custom WGSL shader actions (#90).
pub mod shader;

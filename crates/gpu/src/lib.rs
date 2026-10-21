#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft gpu: wgpu actions, custom WGSL actions and the 3D preview renderer.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

/// Bounded image compute backend.
pub mod image;
/// The 3D preview renderer (#111).
pub mod mesh;
/// Custom WGSL shader actions (#90).
pub mod shader;

use futures::FutureExt;

/// Runs `work` inside out-of-memory, internal and validation error scopes, and says whether any
/// of them caught an error, so a failed GPU call is refused instead of reaching the device's
/// uncaptured-error handler (whose default panics).
pub(crate) fn scoped<T>(device: &wgpu::Device, work: impl FnOnce() -> T) -> (T, bool) {
    let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let value = work();
    let caught = [validation.pop(), internal.pop(), oom.pop()].into_iter().any(|scope| scope.now_or_never().flatten().is_some());
    (value, caught)
}

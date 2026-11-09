//! Trusted 3D preparation worker; Trunk generates its loader from Rust.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#[cfg(target_arch = "wasm32")]
fn main() {
    let _ =
        nodecraft_mesh_preview::worker::start(|center, radius| nodecraft_gpu::mesh::Orbit::default().checked_view_projection(center, radius, 1.0).is_some());
}
#[cfg(not(target_arch = "wasm32"))]
fn main() {}

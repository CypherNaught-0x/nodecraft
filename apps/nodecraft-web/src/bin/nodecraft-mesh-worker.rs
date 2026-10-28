//! Trusted 3D preparation worker; Trunk generates its loader from Rust.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#[cfg(target_arch = "wasm32")]
fn main() {
    let _ = nodecraft_mesh_preview::worker::start(|_, _| true);
}
#[cfg(not(target_arch = "wasm32"))]
fn main() {}

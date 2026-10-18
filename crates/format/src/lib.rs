#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft format: project files, content-addressed asset store and migrations.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

mod archive;
mod assets;
mod compatibility;
pub mod journal;
mod legacy;
pub mod storage;
#[cfg(any(test, feature = "validation-metrics"))]
mod validation_metrics;
#[cfg(any(test, feature = "validation-metrics"))]
pub use validation_metrics::{ValidationMetrics, take_validation_metrics};
mod v2;

pub use archive::{MAX_ARCHIVE_BYTES, MAX_MANIFEST_BYTES, Snapshot, decode, encode, validate_current_project, validate_v1_project};
pub use assets::{AssetStore, MAX_ASSET_BYTES, MAX_ASSETS, MAX_TOTAL_ASSET_BYTES, assets_in_changes};
#[cfg(not(target_arch = "wasm32"))]
pub use storage::NativeStorage;
pub use storage::{LoadedProject, MemoryStorage, ProjectAccess, ProjectStorage, StorageLocation};

fn invalid(message: &'static str) -> nodecraft_types::Error {
    nodecraft_types::Error::new(nodecraft_types::ErrorKind::InvalidInput, message)
}

pub(crate) fn hash(bytes: &[u8]) -> nodecraft_types::Blake3 {
    nodecraft_types::Blake3::from_bytes(*blake3::hash(bytes).as_bytes())
}

/// Project/journal references use types::Blake3; standalone media cache keys use ContentHash.
/// Free functions avoid orphan From impls between two types owned by other crates.
pub fn canonical_hash(hash: nodecraft_media::ContentHash) -> nodecraft_types::Blake3 {
    nodecraft_types::Blake3::from_bytes(hash.0)
}
pub fn media_hash(hash: nodecraft_types::Blake3) -> nodecraft_media::ContentHash {
    nodecraft_media::ContentHash(*hash.as_bytes())
}

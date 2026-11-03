#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft format: project files, content-addressed asset store and migrations.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

mod archive;
mod archive_plan;
mod asset_context;
pub mod asset_source;
mod asset_store;
mod assets;
pub mod backup;
mod checkpoint_plan;
mod compatibility;
pub mod disk_quota;
/// Local batch export (#98): names, collision policy and output-format capability. The write
/// itself is `engine`'s, sharing `history.export`'s (#557) verifying atomic writer.
pub mod export;
mod frame_plan;
pub mod interchange;
pub mod journal;
#[cfg(not(target_arch = "wasm32"))]
pub mod native_assets;
#[cfg(not(target_arch = "wasm32"))]
mod native_atomic;
#[cfg(not(target_arch = "wasm32"))]
mod native_cleanup;
#[cfg(not(target_arch = "wasm32"))]
pub mod native_workspace;
#[cfg(not(target_arch = "wasm32"))]
mod reader_import;
#[cfg(not(target_arch = "wasm32"))]
pub use native_assets::NativeAssetWorkspace;
mod legacy;
/// Disk-space inventory and advisory preflight for saves and opens (#367).
pub mod space;
pub mod storage;
#[cfg(any(test, feature = "validation-metrics"))]
mod validation_metrics;
pub mod zip_index;
#[cfg(any(test, feature = "validation-metrics"))]
pub use validation_metrics::{ValidationMetrics, take_validation_metrics};
mod v2;

pub use archive::{MAX_ARCHIVE_BYTES, MAX_MANIFEST_BYTES, Snapshot, decode, decode_indexed, encode, validate_current_project, validate_v1_project};
pub use archive_plan::{ArchivePlan, StorageCapability};
pub use asset_context::AssetContext;
pub use asset_store::{AssetDescriptor, VerifiedAsset, VerifiedReader};
pub use asset_store::{PreviewAssetRead, PreviewReadError, SealedAsset};
pub use assets::{AssetStore, MAX_ASSET_BYTES, MAX_ASSETS, MAX_TOTAL_ASSET_BYTES, assets_in_changes, image_header_meta};
pub use checkpoint_plan::CheckpointPlan;
pub use frame_plan::{FramePlan, MAX_STREAM_JOURNAL_BYTES, MAX_STREAM_TAIL_BYTES, RecordedAssets};
#[cfg(not(target_arch = "wasm32"))]
pub use storage::NativeStorage;
pub use storage::{LoadedProject, MemoryStorage, PreparedSnapshot, ProjectAccess, ProjectStorage, StorageLocation};

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

#[cfg(test)]
mod svg_tests;

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft plugin store: verifying `.ncraft-plugin` data packages and keeping them installed
//! (phase B of the plugin RFC, #172). Nothing in a package executes; packages install disabled,
//! and enabling publishes their registrations all or none.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod archive;
pub mod backend;
pub mod layout;
pub mod store;

pub use archive::{ArchiveLimits, Package};
#[cfg(not(target_arch = "wasm32"))]
pub use backend::FolderBackend;
pub use backend::MemoryBackend;
pub use store::{EnableReport, Entry, ImportReport, Index, State, Store, StoreBackend, StoreLock};

#[cfg(test)]
mod tests;

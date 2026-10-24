#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft plugin store: verifying `.ncraft-plugin` packages and keeping them installed (phase
//! B of the plugin RFC, #172). Packages install disabled, and enabling publishes their
//! registrations all or none. Data packages run nothing; text action packages (#173) run only
//! host-evaluated expressions, and only under the user's grant for their exact digest.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod archive;
pub mod author;
pub mod backend;
pub mod grants;
pub mod layout;
pub mod store;

pub use archive::{ArchiveLimits, Package};
#[cfg(not(target_arch = "wasm32"))]
pub use backend::FolderBackend;
pub use backend::MemoryBackend;
pub use grants::{Grant, Grants, Rights};
pub use store::{EnableReport, Entry, ImportReport, Index, RevokeReport, State, Store, StoreBackend, StoreLock, grantable_by_this_build};

#[cfg(test)]
mod tests;

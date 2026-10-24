#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft plugin contract: the versioned package and wire format of the plugin RFC
//! (`docs/plugins-rfc.md`, phase A, #171), with no runtime, I/O or guest execution.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.
//! Standalone: no workspace crate, so another implementation can be checked against the same
//! fixtures (`crates/plugin/fixtures`, described in `docs/plugin-wire.md`).
//!
//! Everything here reads untrusted bytes and returns a [`WireError`] with a stable [`Code`],
//! never a panic. Sizes are checked before anything sized by input is allocated.

pub mod digest;
pub mod error;
pub mod frame;
pub mod id;
pub mod inventory;
pub mod job;
pub mod json;
pub mod lock;
pub mod manifest;
pub mod negotiate;
pub mod preserve;
pub mod register;
pub mod signature;

pub use digest::PackageDigest;
pub use error::{Code, WireError};
pub use frame::{MAX_FRAME_BYTES, Message};
pub use id::{LocalId, PackageId, RegisteredId};
pub use inventory::Inventory;
pub use manifest::Manifest;

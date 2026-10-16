#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft types: ids, modalities, port types, values, hashes, money and the error base (pure data).
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

mod asset;
mod error;
mod ids;
mod money;
mod value;

pub use asset::{AssetRef, Blake3, MediaMeta, Mime};
pub use error::{Error, ErrorKind};
pub use ids::*;
pub use money::{Currency, Money, Rounding};
pub use value::{Modality, PortType, Value};

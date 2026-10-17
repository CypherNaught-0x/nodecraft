#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft nodes: node kinds, their registry and parameter schemas.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod audio;
mod descriptor;
pub mod image;
mod registry;
pub mod router;
mod source;
pub mod text;
pub mod video;

pub use descriptor::{Category, InputValue, JobSpec, ModeId, NodeDescriptor, NodeKind, NodeMigration, PlanCtx};
pub use nodecraft_types::{Choice, ParamControl, ParamSchema, ParamSpec, ParamValues, ResolvedParams, VisibleIf};
pub use registry::Registry;
pub use source::SourceKind;

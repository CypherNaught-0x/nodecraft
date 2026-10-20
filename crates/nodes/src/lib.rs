#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft nodes: node kinds, their registry and parameter schemas.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod action;
pub mod audio;
pub mod batch;
mod contract;
mod descriptor;
pub mod generation;
pub mod image;
pub mod mask;
pub mod note;
mod registry;
pub mod router;
pub mod shader;
mod source;
pub mod technique;
pub mod text;
pub mod video;

pub use contract::{InputContract, ModeContract, NodeContract, OutputContract, PortCount};
pub use descriptor::{Category, GenerateInput, GenerateOutput, InputValue, JobSpec, ModeId, NodeDescriptor, NodeKind, NodeMigration, PlanCtx};
pub use nodecraft_types::{Choice, ParamControl, ParamSchema, ParamSpec, ParamValues, ResolvedParams, VisibleIf};
pub use registry::Registry;
pub use source::SourceKind;

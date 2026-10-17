#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft engine: session, command registry, undo stack, event bus and settings.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod build_info;
pub mod clipboard_cmds;
mod command_schemas;
pub mod commands;
pub mod create_cmds;
pub mod edge_cmds;
pub mod generate;
pub mod graph_cmds;
pub mod jobs;
pub mod keys_cmds;
pub mod navigation_cmds;
pub mod node_cmds;
pub mod search_cmds;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod stale_cmds;

pub use session::{DocChanged, Session};

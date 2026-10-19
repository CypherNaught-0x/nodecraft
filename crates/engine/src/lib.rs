#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft engine: session, command registry, undo stack, event bus and settings.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod build_info;
pub mod clipboard_cmds;
mod command_schemas;
pub mod commands;
pub mod comment_cmds;
pub mod create_cmds;
pub mod edge_cmds;
mod execution_identity;
pub mod file_cmds;
pub mod generate;
pub mod graph_cmds;
pub mod history_cmds;
pub mod import;
pub mod jobs;
pub mod keys_cmds;
pub mod layout_cmds;
pub mod navigation_cmds;
pub mod node_cmds;
pub mod order_cmds;
pub mod ownership_cmds;
pub mod perf;
pub mod perf_cmds;
mod persistence;
pub mod search_cmds;

pub mod run_ceiling;
pub mod run_cmds;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod split;
pub mod stale_cmds;
pub mod tag_cmds;
pub mod tag_filter_cmds;
pub mod turn_into_cmds;

pub use session::{DocChanged, Session};

pub mod provider_routing;
pub mod provider_setup;

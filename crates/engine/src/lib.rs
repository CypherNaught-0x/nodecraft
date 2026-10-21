#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft engine: session, command registry, undo stack, event bus and settings.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod action_exec;
pub mod app_cmds;
pub mod assist_cmds;
pub mod build_info;
pub mod bulk_cmds;
pub mod cache;
pub mod cache_cmds;
pub mod clipboard_cmds;
mod command_schemas;
pub mod commands;
pub mod comment_cmds;
pub mod create_cmds;
pub mod describe_cmds;
pub mod document_cmds;
pub mod edge_cmds;
pub mod element_cmds;
mod execution_identity;
mod export;
pub mod export_cmds;
pub mod file_cmds;
pub mod generate;
pub mod graph_cmds;
pub mod history_cmds;
pub mod import;
pub mod jobs;
pub mod keys_cmds;
pub mod layer_cmds;
pub mod layout_cmds;
pub mod library;
pub mod library_cmds;
pub mod live_actions;
pub mod mask_cmds;
pub mod mesh_cmds;
pub mod model_cmds;
pub mod model_swap;
pub mod navigation_cmds;
pub mod node_cmds;
pub mod option_cmds;
pub mod option_fetch;
pub mod order_cmds;
pub mod ownership_cmds;
pub mod perf;
pub mod perf_cmds;
mod persistence;
pub mod plugin_cmds;
pub mod resolve_cmds;
pub mod search_cmds;

pub mod generation_catalog;
pub mod run_ceiling;
pub mod run_cmds;
pub mod run_host;
pub mod run_jobs;
pub mod run_journal;
pub mod save_runner;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod shader_cmds;
pub mod sheet_cmds;
pub mod split;
pub mod stale_cmds;
pub mod tag_cmds;
pub mod tag_filter_cmds;
pub mod technique_cmds;
pub mod technique_node_cmds;
pub mod turn_into_cmds;
pub mod update_check;
pub mod update_install;

pub use session::{DocChanged, PreparedImports, Session};

pub mod provider_routing;
pub mod provider_setup;

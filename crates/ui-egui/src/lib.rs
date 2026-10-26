#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft ui-egui: the egui shell: canvas widget, node cards, panels and theme tokens.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod app;
pub mod audio;
pub mod budgets;
mod bulk;
pub mod canvas;
pub mod card;
pub mod clipboard;
pub mod comments;
pub mod history;
pub mod ids;
pub mod keys;
pub mod mesh_view;
pub mod model_picker;
pub mod navigation;
pub mod noodle;
pub mod order;
pub mod palette;
pub mod plugins;
pub mod preview;
pub mod search;
pub mod selection;
pub mod tags;
pub(crate) mod text_preview;
pub mod theme;
pub mod turn_into;
pub mod video;
#[cfg(target_arch = "wasm32")]
mod web_storage;

pub use app::NodeCraftApp;
/// The update restart's helper modes, which the desktop app's `main` runs before any window (#540).
#[cfg(not(target_arch = "wasm32"))]
pub use app::updates::install::{finish_update, remember_update_roots, wait_for_exit};

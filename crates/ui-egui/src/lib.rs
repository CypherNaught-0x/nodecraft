#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft ui-egui: the egui shell: canvas widget, node cards, panels and theme tokens.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod app;
pub mod canvas;
pub mod card;
pub mod clipboard;
pub mod comments;
pub mod history;
pub mod ids;
pub mod keys;
pub mod navigation;
pub mod noodle;
pub mod order;
pub mod palette;
pub mod preview;
pub mod search;
pub mod selection;
pub mod tags;
pub mod theme;
pub mod turn_into;

pub use app::NodeCraftApp;

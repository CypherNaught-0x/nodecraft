#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft automation: control channel, MCP server and HTTP serve mode.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod commands;
#[cfg(not(target_arch = "wasm32"))]
pub mod control;
pub mod mcp;

use nodecraft_engine::Session;
use nodecraft_graph::{Canvas, Change, Changeset, Project};
use nodecraft_types::{CanvasId, Error, ProjectId};

/// A session on a new, empty project with one canvas: what headless automation starts from until
/// project files can be opened (#54).
pub fn blank_session(project: ProjectId, canvas: CanvasId) -> Result<Session, Error> {
    let mut document = Project::new(project, "Untitled")?;
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }]).apply(&mut document)?;
    Session::new(document)
}

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft automation: control channel (native only), MCP server and the command tools both
//! expose. HTTP serve mode lives in `apps/nodecraft-cli`, not here.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod commands;
#[cfg(not(target_arch = "wasm32"))]
pub mod control;
pub mod mcp;
pub mod tools;

use nodecraft_engine::Session;
use nodecraft_graph::{Canvas, Change, Changeset, Project};
use nodecraft_types::{CanvasId, Error, ProjectId};

/// A session on a new, empty project with one canvas: what headless automation starts from until
/// project files can be opened (#54).
pub fn blank_session(project: ProjectId, canvas: CanvasId) -> Result<Session, Error> {
    let mut document = Project::new(project, "Untitled")?;
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }]).apply(&mut document)?;
    let mut session = Session::new(document)?;
    session.perf_mut().set_clock(nodecraft_engine::perf::native_clock());
    Ok(session)
}

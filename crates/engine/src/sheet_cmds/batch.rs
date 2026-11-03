//! Naming a Batch Generate sheet (`sheet.rename`) and choosing which rows "retry failed" runs
//! (#163). A sheet is reopened from the Sheets panel or `sheet.list` by its title, so the title
//! is what a person names a batch by.
use super::{MAX_TITLE_BYTES, edit, rows, schema, sheet, uuid};
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Canvas, Change};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameParams {
    canvas: CanvasId,
    title: String,
}

fn rename(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: RenameParams = params(value)?;
    // The same rule as `sheet.create`; the canvas must exist and be a sheet.
    let canvas = sheet(c, p.canvas)?;
    let title = p.title.trim();
    if title.is_empty() || p.title.len() > MAX_TITLE_BYTES {
        return Err(invalid("a sheet's title must have 1 to 256 bytes"));
    }
    if canvas.title() == title {
        return Err(Error::new(ErrorKind::Unsupported, "the sheet already has that title"));
    }
    edit(c, vec![Change::SetCanvasTitle { canvas: p.canvas, title: title.to_owned() }], json!({"canvas": p.canvas, "title": title}))
}

/// The rows "retry failed" runs, top to bottom: those whose latest run failed (`failed`, as the
/// run host reported it) and those whose active result is partial (#480: a re-run asks only for
/// the items it lacks), leaving out any still queued or running (`running`). A row whose latest
/// run succeeded is never in it, so retrying never runs a success again; a row that succeeded
/// once and then failed is in it, because its latest run is the failure.
pub fn retry_failed(canvas: &Canvas, failed: &BTreeSet<NodeId>, running: &BTreeSet<NodeId>) -> Vec<NodeId> {
    rows(canvas)
        .into_iter()
        .filter(|row| !running.contains(&row.id))
        .filter(|row| failed.contains(&row.id) || row.active.and_then(|id| row.history.iter().find(|g| g.id == id)).is_some_and(|g| g.is_partial()))
        .map(|row| row.id)
        .collect()
}

pub(super) fn spec() -> CommandSpec {
    CommandSpec {
        id: "sheet.rename",
        label: "Rename sheet",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: || {
            schema(
                &["canvas", "title"],
                json!({"canvas": uuid(), "title": {"type": "string", "minLength": 1, "maxLength": MAX_TITLE_BYTES, "description": "1 to 256 bytes of UTF-8, not only spaces; surrounding spaces are dropped"}}),
            )
        },
        params_doc: "{canvas, title} -> {canvas, title}; renames a Batch Generate sheet, as one undo step. sheet.list lists sheets by title, and the project file keeps it.",
        enabled: |c, v| super::enabled(c, v, rename),
        run: rename,
    }
}

#[cfg(test)]
mod tests;

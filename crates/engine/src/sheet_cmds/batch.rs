//! Naming a Batch Generate sheet (`sheet.rename`) and choosing which rows "retry failed" runs
//! (#163). A sheet is reopened from the Sheets panel or `sheet.list` by its title, so the title
//! is what a person names a batch by.
use super::{MAX_TITLE_BYTES, edit, rows, schema, sheet, uuid};
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::{run_host::HeldView, run_jobs::JobView};
use nodecraft_graph::{Canvas, Change};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};

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
    if title.is_empty() || title.len() > MAX_TITLE_BYTES {
        return Err(invalid("a sheet's title must have 1 to 256 bytes once surrounding spaces are dropped"));
    }
    if title.chars().any(char::is_control) {
        return Err(invalid("a sheet's title can't hold control characters such as line breaks or tabs"));
    }
    if canvas.title() == title {
        return Err(Error::new(ErrorKind::Unsupported, "the sheet already has that title"));
    }
    edit(c, vec![Change::SetCanvasTitle { canvas: p.canvas, title: title.to_owned() }], json!({"canvas": p.canvas, "title": title}))
}

/// What "retry failed" leaves out of a sheet, from what the run host shows now: rows still
/// queued or running, rows with a paid result held for a decision (#369: retrying would pay for
/// it again under new keys), and rows whose latest job may already have been charged although it
/// failed or was cancelled after it was sent ([`JobView::may_have_been_charged`]). The person runs
/// those one by one, or places or discards the held result, knowing the cost.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetryHolds {
    pub running: BTreeSet<NodeId>,
    pub held: BTreeSet<NodeId>,
    pub maybe_charged: BTreeSet<NodeId>,
}

impl RetryHolds {
    /// The holds on `canvas` among `jobs` (the run host's views, in its order: the latest job of a
    /// card is its last view), `held` results and `charged`, the cards the host remembers as
    /// possibly charged after their views are gone (`RunHost::possibly_charged`). One pass over
    /// each.
    pub fn of(canvas: CanvasId, jobs: &[JobView], held: &[HeldView], charged: &BTreeSet<(CanvasId, NodeId)>) -> Self {
        let mut holds = Self::default();
        let mut latest: BTreeMap<NodeId, &JobView> = BTreeMap::new();
        for job in jobs.iter().filter(|job| job.canvas == canvas) {
            if !job.state.is_final() {
                holds.running.insert(job.node);
            }
            latest.insert(job.node, job);
        }
        holds.maybe_charged = latest.into_iter().filter(|(_, job)| job.may_have_been_charged()).map(|(node, _)| node).collect();
        holds.maybe_charged.extend(charged.iter().filter(|(on, _)| *on == canvas).map(|(_, node)| *node));
        holds.held = held.iter().filter(|view| view.canvas == canvas).map(|view| view.node).collect();
        holds
    }

    fn holds(&self, row: NodeId) -> bool {
        self.running.contains(&row) || self.held.contains(&row) || self.maybe_charged.contains(&row)
    }
}

/// The rows "retry failed" runs, top to bottom: those whose latest run failed (`failed`, as the
/// run host reported it) and those whose active result is partial (#480: a re-run asks only for
/// the items it lacks, under the keys kept for any that may have been billed, #517), leaving out
/// every row `holds` names. A row whose latest run succeeded is never in it, so retrying never
/// runs a success again; a row that succeeded once and then failed is in it, because its latest
/// run is the failure.
pub fn retry_failed(canvas: &Canvas, failed: &BTreeSet<NodeId>, holds: &RetryHolds) -> Vec<NodeId> {
    rows(canvas)
        .into_iter()
        .filter(|row| !holds.holds(row.id))
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
                json!({"canvas": uuid(), "title": {"type": "string", "minLength": 1, "maxLength": MAX_TITLE_BYTES, "description": "1 to 256 bytes of UTF-8 once surrounding spaces are dropped (maxLength bounds characters; the byte limit is checked too), no control characters"}}),
            )
        },
        params_doc: "{canvas, title} -> {canvas, title}; renames a Batch Generate sheet, as one undo step. sheet.list lists sheets by title, and the project file keeps it.",
        enabled: |c, v| super::enabled(c, v, rename),
        run: rename,
    }
}

#[cfg(test)]
mod tests;

//! Atomic selected-node layout using a revision-stamped, scoped host geometry capability.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_graph::{
    Change, Changeset, MAX_CHANGES,
    layout::{self, Extent, Mode},
};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ProjectId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
/// A trusted host computes current logical geometry at its zoom. The engine enforces the
/// document stamp, exact selection membership, counts and all numeric/output bounds.
#[derive(Clone, Debug)]
pub struct LayoutContext {
    pub project: ProjectId,
    pub canvas: CanvasId,
    pub revision: u64,
    pub selection: Vec<NodeId>,
    pub extents: Vec<Extent>,
    pub gap: f64,
    pub blocked: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum LayoutMode {
    Grid,
    Layered,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    mode: LayoutMode,
    #[serde(default, deserialize_with = "columns")]
    columns: Option<usize>,
}
fn columns<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<usize>, D::Error> {
    usize::deserialize(d).map(Some)
}
fn run(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: Params = params(value)?;
    let host = c.resources.require::<LayoutContext>()?;
    let conflict = |m| Error::new(ErrorKind::Conflict, m);
    if host.project != c.document.id() || host.revision != c.revision {
        return Err(conflict("layout geometry is stale; refresh the live canvas"));
    }
    if host.blocked {
        return Err(conflict("finish the active editor or gesture before formatting"));
    }
    let canvas = c.document.canvases().get(&host.canvas).ok_or_else(|| conflict("layout canvas is unavailable"))?;
    // Preflight before copying vectors/building adjacency or sets.
    let n = host.selection.len();
    if !(2..=MAX_CHANGES).contains(&n) || n > canvas.nodes().len() || host.extents.len() != n {
        return Err(conflict("select at least two nodes with complete current geometry"));
    }
    let selected: BTreeSet<_> = host.selection.iter().copied().collect();
    if selected.len() != n || selected.iter().any(|id| !canvas.nodes().contains_key(id)) {
        return Err(conflict("layout selection is invalid"));
    }
    let metrics: BTreeSet<_> = host.extents.iter().map(|e| e.id).collect();
    if metrics != selected {
        return Err(conflict("layout geometry does not match selection"));
    }
    let mode = match p.mode {
        LayoutMode::Grid => Mode::Grid,
        LayoutMode::Layered => Mode::Layered,
    };
    let positions = layout::arrange(canvas, &host.extents, mode, p.columns, host.gap)?;
    let result = json!({"moved":positions.len()});
    if positions.is_empty() {
        return Ok(Effect::None { result });
    }
    let changes = Changeset::new(positions.into_iter().map(|(id, pos)| Change::MoveNode { canvas: host.canvas, id, pos }).collect());
    Ok(Effect::Edit { changes, result, gesture: None })
}
fn enabled(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    c.ensure_editable()?;
    if let Effect::Edit { changes, .. } = run(c, v)? {
        c.preview_changes(&changes)?;
    }
    Ok(())
}
fn schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"mode":{"type":"string","enum":["grid","layered"]},"columns":{"type":"integer","minimum":1,"maximum":MAX_CHANGES}},"required":["mode"],"additionalProperties":false,"allOf":[{"if":{"properties":{"mode":{"const":"layered"}}},"then":{"not":{"required":["columns"]}}}]})
}
pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "canvas.auto_format",
        label: "Format selection",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{mode:grid|layered,columns?:integer}; atomic selected-node layout using current host geometry; columns is grid-only.",
        enabled,
        run,
    }]
}

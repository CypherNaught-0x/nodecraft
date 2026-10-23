//! Atomic selected-node layout using a revision-stamped, scoped host geometry capability.
use crate::commands::{CommandSpec, Context, Effect, Phase, params};
use nodecraft_graph::{
    Change, Changeset, MAX_CHANGES,
    layout::{self, Extent, Mode},
};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ProjectId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
/// A trusted host computes actual-instance logical geometry at reference zoom 1.0, including
/// decoration insets; camera zoom must never affect persistent positions. The engine enforces the
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
    /// The nodes to arrange when the caller has no selection to point at (agents, #360). The
    /// host builds its geometry for exactly these; absent means the host's current selection.
    #[serde(default, deserialize_with = "nodes")]
    nodes: Option<Vec<NodeId>>,
}
/// The explicit `nodes` a trusted host should measure, when the params name them. The engine
/// checks them again against the host's geometry; this only tells the host what to measure.
pub fn requested_nodes(value: &Value) -> Option<Vec<NodeId>> {
    let nodes = value.get("nodes")?.as_array()?;
    if nodes.len() > MAX_CHANGES {
        return None;
    }
    nodes.iter().map(|id| serde_json::from_value(id.clone()).ok()).collect()
}
fn nodes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Vec<NodeId>>, D::Error> {
    Vec::<NodeId>::deserialize(d).map(Some)
}
fn columns<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<usize>, D::Error> {
    usize::deserialize(d).map(Some)
}
fn preflight<'a>(c: &'a Context<'_>, value: &Value) -> Result<(&'a LayoutContext, &'a nodecraft_graph::Canvas, Mode, Option<usize>), Error> {
    c.editable()?;
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
    if let Some(nodes) = &p.nodes
        && (nodes.len() != n || nodes.iter().collect::<BTreeSet<_>>() != selected.iter().collect())
    {
        return Err(conflict("layout geometry does not match the requested nodes"));
    }
    let metrics: BTreeSet<_> = host.extents.iter().map(|e| e.id).collect();
    if metrics != selected {
        return Err(conflict("layout geometry does not match selection"));
    }
    let mode = match p.mode {
        LayoutMode::Grid => Mode::Grid,
        LayoutMode::Layered => Mode::Layered,
    };
    layout::validate_input(canvas, &host.extents, mode, p.columns, host.gap)?;
    Ok((host, canvas, mode, p.columns))
}
#[cfg(test)]
thread_local! { static ARRANGEMENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
fn run(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let (host, canvas, mode, columns) = preflight(c, value)?;
    if c.phase == Phase::Preview {
        // Availability admits structure only; Dispatch retains normal staged persistence admission.
        return Ok(Effect::None { result: Value::Null });
    }
    #[cfg(test)]
    ARRANGEMENTS.with(|n| n.set(n.get() + 1));
    let positions = layout::arrange(canvas, &host.extents, mode, columns, host.gap)?;
    let result = json!({"moved":positions.len()});
    if positions.is_empty() {
        return Ok(Effect::None { result });
    }
    let changes = Changeset::new(positions.into_iter().map(|(id, pos)| Change::MoveNode { canvas: host.canvas, id, pos }).collect());
    Ok(Effect::Edit { changes, result, gesture: None })
}
fn enabled(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    // Advisory structural admission: arrange/output range and transaction validation happen
    // exactly once during dispatch. Menu queries never clone the project or rank the DAG.
    preflight(c, v)?;
    Ok(())
}
fn schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"mode":{"type":"string","enum":["grid","layered"]},"columns":{"type":"integer","minimum":1,"maximum":MAX_CHANGES},"nodes":{"type":"array","items":{"type":"string","format":"uuid"},"minItems":2,"maxItems":MAX_CHANGES,"uniqueItems":true,"description":"Nodes on the shown canvas to arrange; absent means the current selection"}},"required":["mode"],"additionalProperties":false,"allOf":[{"if":{"properties":{"mode":{"const":"layered"}}},"then":{"not":{"required":["columns"]}}}]})
}
pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "canvas.auto_format",
        label: "Format selection",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{mode:grid|layered,columns?:integer,nodes?:[uuid]}; atomic layout of `nodes` on the shown canvas (agents: name them rather than relying on the selection), else of the selection, using current host geometry; columns is grid-only.",
        enabled,
        run,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;
    use nodecraft_graph::{Canvas, Node, Project};
    #[test]
    fn thousand_selected_repeated_enabled_checks_never_arrange_or_preview_edits() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Admission").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let ids: Vec<_> = (0u128..1000).map(|n| NodeId::from_bytes(n.to_le_bytes())).collect();
        Changeset::new(ids.iter().map(|id| Change::AddNode { canvas, node: Node::new(*id, "test.text") }).collect()).apply(&mut project).unwrap();
        let mut session = Session::new(project).unwrap();
        session.swap_resource(Some(LayoutContext {
            project: session.document().id(),
            canvas,
            revision: 0,
            selection: ids.clone(),
            extents: ids.iter().map(|id| Extent { id: *id, width: 320.0, height: 220.0, left: 0.0, top: 24.0, right: 16.0, bottom: 8.0 }).collect(),
            gap: 64.0,
            blocked: false,
        }));
        ARRANGEMENTS.with(|n| n.set(0));
        for _ in 0..20 {
            for mode in ["grid", "layered"] {
                session.enabled("canvas.auto_format", &json!({"mode":mode})).unwrap();
            }
        }
        ARRANGEMENTS.with(|n| assert_eq!(n.get(), 0));
        assert_eq!(session.revision(), 0);
        session.dispatch("canvas.auto_format", &json!({"mode":"grid"})).unwrap();
        ARRANGEMENTS.with(|n| assert_eq!(n.get(), 1));
        assert_eq!(session.revision(), 1);
    }
    #[test]
    fn named_nodes_must_be_exactly_what_the_host_measured() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Named").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let ids: Vec<_> = (0u128..3).map(|n| NodeId::from_bytes(n.to_le_bytes())).collect();
        Changeset::new(ids.iter().map(|id| Change::AddNode { canvas, node: Node::new(*id, "test.text") }).collect()).apply(&mut project).unwrap();
        let mut session = Session::new(project).unwrap();
        let measured = &ids[..2];
        session.swap_resource(Some(LayoutContext {
            project: session.document().id(),
            canvas,
            revision: 0,
            selection: measured.to_vec(),
            extents: measured.iter().map(|id| Extent { id: *id, width: 320.0, height: 220.0, left: 0.0, top: 24.0, right: 16.0, bottom: 8.0 }).collect(),
            gap: 64.0,
            blocked: false,
        }));
        // A host that measured something else than the caller named must not move either set.
        for nodes in [json!(ids), json!([ids[0], ids[2]]), json!([ids[0], ids[0]])] {
            let error = session.dispatch("canvas.auto_format", &json!({"mode":"grid","nodes":nodes})).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Conflict, "{nodes}");
        }
        assert_eq!(session.revision(), 0);
        session.dispatch("canvas.auto_format", &json!({"mode":"grid","nodes":[ids[1], ids[0]]})).unwrap();
        assert_eq!(session.revision(), 1);
        assert_eq!(requested_nodes(&json!({"nodes":[ids[1]]})), Some(vec![ids[1]]));
        assert_eq!(requested_nodes(&json!({"nodes":["not an id"]})), None);
        assert_eq!(requested_nodes(&json!({"mode":"grid"})), None);
    }
}

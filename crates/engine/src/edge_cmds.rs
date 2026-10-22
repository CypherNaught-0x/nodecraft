//! Atomic multi-change edits used by the canvas: rewiring a noodle and deleting a selection
//! (nodes, noodles and fields).
//! Each is one changeset, so it is one undo step and either applies completely or not at all.

use nodecraft_graph::{Change, Changeset, Edge, MAX_CHANGES, NodeFieldAssignment};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, FieldId, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

use crate::commands::{CommandSpec, Context, Effect, invalid, params};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewireParams {
    pub canvas: CanvasId,
    pub edge: Edge,
    /// Edges removed before connecting: the rerouted noodle, or the occupant of a single input.
    #[serde(default)]
    pub replace: Vec<EdgeId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteParams {
    pub canvas: CanvasId,
    #[serde(default)]
    pub nodes: Vec<NodeId>,
    #[serde(default)]
    pub edges: Vec<EdgeId>,
    /// Fields removed with the selection; their surviving cards stay, unassigned.
    #[serde(default)]
    pub fields: Vec<FieldId>,
}

/// Rewiring touches at most the dragged noodle and the input's current occupant.
const MAX_REPLACED: usize = 2;

fn edit(changes: Vec<Change>, result: Value) -> Effect {
    Effect::Edit { changes: Changeset::new(changes), result, gesture: None }
}

fn rewire(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: RewireParams = params(value)?;
    if p.replace.len() > MAX_REPLACED {
        return Err(invalid("rewire replaces at most two edges"));
    }
    let replace: BTreeSet<EdgeId> = p.replace.into_iter().collect();
    let result = json!({"id": p.edge.id});
    let mut changes: Vec<Change> = replace.into_iter().map(|id| Change::Disconnect { canvas: p.canvas, id }).collect();
    if let Some(node) = crate::graph_cmds::upgraded(c, p.canvas, p.edge.to.node, &p.edge.to.port) {
        changes.push(Change::SetNode { canvas: p.canvas, node });
    }
    changes.push(Change::Connect { canvas: p.canvas, edge: p.edge });
    Ok(edit(changes, result))
}

fn delete(context: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: DeleteParams = params(value)?;
    if p.nodes.len().saturating_add(p.edges.len()).saturating_add(p.fields.len()) > MAX_CHANGES {
        return Err(invalid("delete exceeds the operation limit"));
    }
    let nodes: BTreeSet<NodeId> = p.nodes.into_iter().collect();
    let edges: BTreeSet<EdgeId> = p.edges.into_iter().collect();
    let fields: BTreeSet<FieldId> = p.fields.into_iter().collect();
    if nodes.is_empty() && edges.is_empty() && fields.is_empty() {
        return Err(invalid("nothing to delete"));
    }
    // Listed edges go first; removing a node then takes its remaining incident edges with it.
    let mut changes: Vec<Change> = edges
        .into_iter()
        .map(|id| Change::Disconnect { canvas: p.canvas, id })
        .chain(nodes.iter().map(|&id| Change::RemoveNode { canvas: p.canvas, id }))
        .collect();
    if !fields.is_empty() {
        let canvas = context.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas not found"))?;
        if let Some(field) = fields.iter().find(|id| !canvas.fields().contains_key(id)) {
            return Err(Error::new(ErrorKind::NotFound, format!("field {field} not found")));
        }
        // Like `field.remove`: members that survive the delete leave the field before it goes.
        let assignments: Vec<NodeFieldAssignment> = canvas
            .nodes()
            .values()
            .filter(|n| !nodes.contains(&n.id) && n.field.is_some_and(|f| fields.contains(&f)))
            .map(|n| NodeFieldAssignment { node: n.id, field: None })
            .collect();
        if !assignments.is_empty() {
            changes.push(Change::SetNodeFields { canvas: p.canvas, assignments });
        }
        changes.extend(fields.into_iter().map(|id| Change::RemoveField { canvas: p.canvas, id }));
    }
    // Unassigning surviving members is one more change.
    if changes.len() > MAX_CHANGES {
        return Err(invalid("delete exceeds the operation limit"));
    }
    Ok(edit(changes, Value::Null))
}

fn uuid() -> Value {
    json!({"type": "string", "format": "uuid"})
}
fn ids(max: usize) -> Value {
    json!({"type": "array", "items": uuid(), "maxItems": max})
}
fn object(properties: Value, required: &[&str]) -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "properties": properties, "required": required, "additionalProperties": false})
}
fn rewire_schema() -> Value {
    let endpoint = json!({"type": "object", "properties": {"node": uuid(), "port": {"type": "string", "maxLength": 256}}, "required": ["node", "port"], "additionalProperties": false});
    let edge = json!({"type": "object", "properties": {"id": uuid(), "from": endpoint, "to": endpoint, "order": {"type": "integer", "minimum": 0, "maximum": u32::MAX}},
        "required": ["id", "from", "to", "order"], "additionalProperties": false});
    object(json!({"canvas": uuid(), "edge": edge, "replace": ids(MAX_REPLACED)}), &["canvas", "edge"])
}
fn delete_schema() -> Value {
    object(json!({"canvas": uuid(), "nodes": ids(MAX_CHANGES), "edges": ids(MAX_CHANGES), "fields": ids(MAX_CHANGES)}), &["canvas"])
}

fn enabled_edit(context: &Context<'_>, value: &Value, run: fn(&Context<'_>, &Value) -> Result<Effect, Error>) -> Result<(), Error> {
    // Full validation only when previewing; dispatch stages the changeset once itself.
    if let Effect::Edit { changes, .. } = run(context, value)? {
        context.preview_changes(&changes)?;
    }
    Ok(())
}
fn enabled_rewire(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    enabled_edit(c, v, rewire)
}
fn enabled_delete(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    enabled_edit(c, v, delete)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "edge.rewire",
            alternate_shortcuts: &[],
            input_schema: rewire_schema,
            label: "Rewire connection",
            shortcut: None,
            params_doc: "{canvas: CanvasId, edge: {id: EdgeId, from: {node: NodeId, port: string}, to: {node: NodeId, port: string}, order: u32}, replace?: [EdgeId]}; removes up to two existing edges and connects `edge`, as one undo step.",
            enabled: enabled_rewire,
            run: rewire,
        },
        CommandSpec {
            id: "canvas.delete",
            label: "Delete",
            shortcut: Some("Delete"),
            alternate_shortcuts: &["Backspace"],
            input_schema: delete_schema,
            params_doc: "{canvas: CanvasId, nodes?: [NodeId], edges?: [EdgeId], fields?: [FieldId]}; removes the edges, then the nodes with their remaining connections, then the fields (their remaining cards stay, unassigned), as one undo step.",
            enabled: enabled_delete,
            run: delete,
        },
    ]
}

#[cfg(test)]
mod tests {
    use crate::Session;
    use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Node, Port, Project};
    use nodecraft_types::{CanvasId, EdgeId, ErrorKind, Modality, NodeId, PortType, ProjectId};
    use serde_json::json;

    const C: CanvasId = CanvasId::from_bytes([1; 16]);

    fn id(n: u8) -> NodeId {
        NodeId::from_bytes([n; 16])
    }
    fn edge(e: u8, from: u8, to: u8) -> Edge {
        Edge {
            id: EdgeId::from_bytes([e; 16]),
            from: Endpoint { node: id(from), port: "out".into() },
            to: Endpoint { node: id(to), port: "in".into() },
            order: 0,
        }
    }
    /// Nodes 1, 2, 3 with a single text input each; 1 → 2.
    fn session() -> Session {
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Edges").unwrap();
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        for n in 1..=3 {
            let mut node = Node::new(id(n), "test.text");
            let port_type = PortType { modality: Modality::Text, many: false };
            node.inputs.push(Port { name: "in".into(), port_type });
            node.outputs.push(Port { name: "out".into(), port_type });
            Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
        }
        Change::Connect { canvas: C, edge: edge(10, 1, 2) }.apply(&mut project).unwrap();
        Session::new(project).unwrap()
    }
    fn edges(session: &Session) -> Vec<EdgeId> {
        session.document().canvases().get(&C).unwrap().edges().keys().copied().collect()
    }

    #[test]
    fn rewire_replaces_an_occupied_input_in_one_undo_step() {
        let mut s = session();
        let before = s.document().clone();
        // 3 → 2 replaces 1 → 2 on the single input.
        let params = json!({"canvas": C, "edge": edge(11, 3, 2), "replace": [EdgeId::from_bytes([10; 16])]});
        s.enabled("edge.rewire", &params).unwrap();
        assert_eq!(s.dispatch("edge.rewire", &params).unwrap(), json!({"id": EdgeId::from_bytes([11; 16])}));
        assert_eq!(edges(&s), vec![EdgeId::from_bytes([11; 16])]);
        let after = s.document().clone();
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), &before);
        assert!(!s.can_undo());
        s.dispatch("edit.redo", &json!({})).unwrap();
        assert_eq!(s.document(), &after);
    }

    #[test]
    fn rerouting_reuses_the_moving_edge_id() {
        let mut s = session();
        let before = s.document().clone();
        // The UI reroutes 1 → 2 to 1 → 3 by disconnecting edge 10 and connecting it again.
        let moved = Edge { id: EdgeId::from_bytes([10; 16]), ..edge(10, 1, 3) };
        let params = json!({"canvas": C, "edge": moved, "replace": [EdgeId::from_bytes([10; 16])]});
        s.enabled("edge.rewire", &params).unwrap();
        s.dispatch("edge.rewire", &params).unwrap();
        let canvas = s.document().canvases().get(&C).unwrap();
        assert_eq!(canvas.edges().get(&EdgeId::from_bytes([10; 16])).map(|e| e.to.node), Some(id(3)));
        let after = s.document().clone();
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), &before);
        s.dispatch("edit.redo", &json!({})).unwrap();
        assert_eq!(s.document(), &after);
    }

    #[test]
    fn rewire_is_atomic_and_disabled_for_bad_targets() {
        let mut s = session();
        let before = s.document().clone();
        // Without replacing the occupant the single input rejects a second edge.
        let occupied = json!({"canvas": C, "edge": edge(11, 3, 2)});
        assert!(s.enabled("edge.rewire", &occupied).is_err());
        assert!(s.dispatch("edge.rewire", &occupied).is_err());
        // A cycle (2 → 1 while 1 → 2) is rejected; nothing changes.
        let cycle = json!({"canvas": C, "edge": edge(11, 2, 1)});
        assert!(s.dispatch("edge.rewire", &cycle).is_err());
        let missing = json!({"canvas": C, "edge": edge(11, 3, 1), "replace": [EdgeId::from_bytes([99; 16])]});
        assert!(s.dispatch("edge.rewire", &missing).is_err());
        let many =
            json!({"canvas": C, "edge": edge(11, 3, 1), "replace": [EdgeId::from_bytes([1; 16]), EdgeId::from_bytes([2; 16]), EdgeId::from_bytes([3; 16])]});
        assert_eq!(s.dispatch("edge.rewire", &many).unwrap_err().kind, ErrorKind::InvalidInput);
        assert_eq!(s.document(), &before);
        assert!(!s.can_undo());
    }

    #[test]
    fn delete_removes_edges_and_nodes_as_one_step() {
        let mut s = session();
        s.dispatch("edge.connect", &json!({"canvas": C, "edge": edge(11, 2, 3)})).unwrap();
        let before = s.document().clone();
        // Edge 10 is listed and also incident to node 2; listing both must still work.
        let params = json!({"canvas": C, "nodes": [id(2), id(2)], "edges": [EdgeId::from_bytes([10; 16])]});
        s.enabled("canvas.delete", &params).unwrap();
        s.dispatch("canvas.delete", &params).unwrap();
        let canvas = s.document().canvases().get(&C).unwrap();
        assert!(canvas.edges().is_empty());
        assert_eq!(canvas.nodes().keys().copied().collect::<Vec<_>>(), vec![id(1), id(3)]);
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), &before);
    }

    #[test]
    fn delete_rejects_empty_missing_and_oversized_requests() {
        let mut s = session();
        let before = s.document().clone();
        for params in [
            json!({"canvas": C}),
            json!({"canvas": C, "nodes": [id(9)]}),
            json!({"canvas": C, "edges": [EdgeId::from_bytes([99; 16])]}),
            json!({"canvas": C, "nodes": vec![id(1); nodecraft_graph::MAX_CHANGES + 1]}),
            json!({"canvas": CanvasId::from_bytes([7; 16]), "nodes": [id(1)]}),
        ] {
            assert!(s.enabled("canvas.delete", &params).is_err(), "{params}");
            assert!(s.dispatch("canvas.delete", &params).is_err(), "{params}");
        }
        assert_eq!(s.document(), &before);
        assert!(!s.can_undo());
    }

    #[test]
    fn delete_removes_fields_keeping_surviving_cards_in_one_step() {
        let mut s = session();
        let (a, b) = (nodecraft_types::FieldId::from_bytes([20; 16]), nodecraft_types::FieldId::from_bytes([21; 16]));
        for (field, nodes) in [(a, vec![id(1), id(2)]), (b, vec![id(3)])] {
            let params = json!({"canvas": C, "field": {"id": field, "title": "F", "rect": {"pos": {"x": 0.0, "y": 0.0}, "width": 100.0, "height": 100.0}}, "nodes": nodes});
            s.dispatch("field.create", &params).unwrap();
        }
        let before = s.document().clone();
        // Node 2 goes with the fields; node 1 and 3 stay, out of any field.
        let params = json!({"canvas": C, "nodes": [id(2)], "fields": [a, b, a]});
        s.enabled("canvas.delete", &params).unwrap();
        s.dispatch("canvas.delete", &params).unwrap();
        let canvas = s.document().canvases().get(&C).unwrap();
        assert!(canvas.fields().is_empty());
        assert_eq!(canvas.nodes().keys().copied().collect::<Vec<_>>(), vec![id(1), id(3)]);
        assert!(canvas.nodes().values().all(|n| n.field.is_none()));
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), &before);
        // A field deleted with all its members needs no unassigning.
        let params = json!({"canvas": C, "nodes": [id(3)], "fields": [b]});
        s.dispatch("canvas.delete", &params).unwrap();
        let canvas = s.document().canvases().get(&C).unwrap();
        assert_eq!(canvas.fields().keys().copied().collect::<Vec<_>>(), vec![a]);
        assert!(!canvas.nodes().contains_key(&id(3)));
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), &before);
        // One unassign change over the limit refuses the delete with its own error.
        let many: Vec<_> = (0..nodecraft_graph::MAX_CHANGES - 1)
            .map(|n| {
                let mut bytes = [0xEE; 16];
                bytes[..8].copy_from_slice(&(n as u64).to_le_bytes());
                EdgeId::from_bytes(bytes)
            })
            .collect();
        let full = json!({"canvas": C, "edges": many, "nodes": [id(2)], "fields": [a]});
        assert_eq!(s.dispatch("canvas.delete", &full).unwrap_err().message, "delete exceeds the operation limit");
        // A missing field refuses the whole delete.
        let missing = json!({"canvas": C, "nodes": [id(1)], "fields": [nodecraft_types::FieldId::from_bytes([99; 16])]});
        assert!(s.enabled("canvas.delete", &missing).is_err());
        assert!(s.dispatch("canvas.delete", &missing).is_err());
        assert_eq!(s.document(), &before);
    }
}

#[cfg(test)]
#[path = "edge_cmds/ports638_tests.rs"]
mod ports638_tests;

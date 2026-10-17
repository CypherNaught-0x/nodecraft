//! Pasting a group of nodes and the edges between them as one undo step (copy, paste,
//! duplicate). The caller supplies fresh ids, so replay stays deterministic.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Edge, MAX_CHANGES, Node};
use nodecraft_types::{CanvasId, Error};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasteParams {
    pub canvas: CanvasId,
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

fn paste(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: PasteParams = params(value)?;
    if p.nodes.is_empty() {
        return Err(invalid("nothing to paste"));
    }
    if p.nodes.len().saturating_add(p.edges.len()) > MAX_CHANGES {
        return Err(invalid("paste exceeds the operation limit"));
    }
    if p.nodes.iter().any(|node| !node.history.is_empty() || node.active.is_some()) {
        return Err(invalid("pasted nodes cannot carry generation history"));
    }
    // Nodes of installed kinds must match their kind: ports as the instance declares them and
    // parameters its schema accepts. Unknown kinds (e.g. from a plugin not installed here) are kept
    // as data, like a loaded project keeps them.
    for node in &p.nodes {
        if let Ok(kind) = c.kinds.get(&node.kind) {
            kind.validate_instance(node, false).map_err(|error| Error::new(error.kind, format!("pasted {} node: {}", node.kind, error.message)))?;
        }
    }
    let ids: Vec<_> = p.nodes.iter().map(|node| node.id).collect();
    let canvas = p.canvas;
    let changes = Changeset::new(
        p.nodes.into_iter().map(|node| Change::AddNode { canvas, node }).chain(p.edges.into_iter().map(|edge| Change::Connect { canvas, edge })).collect(),
    );
    Ok(Effect::Edit { changes, result: json!({"nodes": ids}), gesture: None })
}

fn enabled_paste(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    // Full validation only when previewing; dispatch stages the changeset once itself.
    if let Effect::Edit { changes, .. } = paste(c, value)? {
        c.preview_changes(&changes)?;
    }
    Ok(())
}

/// The node and edge shapes are `node.add`'s and `edge.connect`'s, so every surface describes
/// them the same way.
fn paste_schema() -> Value {
    let add = crate::command_schemas::add_schema();
    let node = add.pointer("/properties/node").cloned().unwrap_or_else(|| json!({"type": "object"}));
    let edge = crate::command_schemas::connect_schema().pointer("/properties/edge").cloned().unwrap_or_else(|| json!({"type": "object"}));
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "nodes"],
        "properties": {"canvas": {"type": "string", "format": "uuid"},
            "nodes": {"type": "array", "minItems": 1, "maxItems": MAX_CHANGES, "items": node},
            "edges": {"type": "array", "maxItems": MAX_CHANGES, "items": edge}},
        "$defs": add.get("$defs").cloned().unwrap_or_else(|| json!({}))})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "canvas.paste",
        label: "Paste nodes",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: paste_schema,
        params_doc: "{canvas: CanvasId, nodes: [graph::Node], edges?: [graph::Edge]}; inserts the nodes, then the edges between them, as one undo step. Ids must be new; nodes carry no generation history.",
        enabled: enabled_paste,
        run: paste,
    }]
}

#[cfg(test)]
mod tests {
    use crate::Session;
    use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Generation, Node, Port, Project};
    use nodecraft_types::{CanvasId, EdgeId, GenerationId, Modality, NodeId, PortType, ProjectId};
    use serde_json::json;

    const C: CanvasId = CanvasId::from_bytes([1; 16]);

    fn session() -> Session {
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Paste").unwrap();
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        Session::new(project).unwrap()
    }
    fn node(n: u8) -> Node {
        let mut node = Node::new(NodeId::from_bytes([n; 16]), "test.text");
        let port_type = PortType { modality: Modality::Text, many: false };
        node.inputs.push(Port { name: "in".into(), port_type });
        node.outputs.push(Port { name: "out".into(), port_type });
        node
    }
    fn edge() -> Edge {
        Edge {
            id: EdgeId::from_bytes([9; 16]),
            from: Endpoint { node: NodeId::from_bytes([1; 16]), port: "out".into() },
            to: Endpoint { node: NodeId::from_bytes([2; 16]), port: "in".into() },
            order: 0,
        }
    }

    #[test]
    fn pastes_nodes_and_edges_as_one_undo_step() {
        let mut s = session();
        let before = s.document().clone();
        let params = json!({"canvas": C, "nodes": [node(1), node(2)], "edges": [edge()]});
        s.enabled("canvas.paste", &params).unwrap();
        s.dispatch("canvas.paste", &params).unwrap();
        let canvas = s.document().canvases().get(&C).unwrap();
        assert_eq!((canvas.nodes().len(), canvas.edges().len()), (2, 1));
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), &before);
        assert!(!s.can_undo());
    }

    #[test]
    fn nodes_of_installed_kinds_must_match_them() {
        let mut s = session();
        let before = s.document().clone();
        let id = NodeId::from_bytes([4; 16]);
        let good = s.kinds().get("source.text").unwrap().descriptor().instantiate(id, &std::collections::BTreeMap::new()).unwrap();
        // Ports changed to accept a list input the kind doesn't have.
        let mut ports = good.clone();
        ports.inputs.push(Port { name: "sneaky".into(), port_type: PortType { modality: Modality::Text, many: true } });
        // A parameter of the wrong type.
        let mut param = good.clone();
        param.params.insert("text".into(), nodecraft_types::Value::Bool(true));
        for bad in [ports, param] {
            let params = json!({"canvas": C, "nodes": [bad]});
            assert!(s.enabled("canvas.paste", &params).is_err());
            assert!(s.dispatch("canvas.paste", &params).is_err());
        }
        assert_eq!(s.document(), &before);
        s.dispatch("canvas.paste", &json!({"canvas": C, "nodes": [good]})).unwrap();
        // The schema describes nodes as node.add does.
        let schema = (s.registry().get("canvas.paste").unwrap().input_schema)();
        assert_eq!(schema.pointer("/properties/nodes/items/required"), Some(&json!(["id", "kind"])));
        assert!(schema.get("$defs").is_some());
    }

    #[test]
    fn rejects_empty_dangling_history_and_reused_ids_atomically() {
        let mut s = session();
        s.dispatch("canvas.paste", &json!({"canvas": C, "nodes": [node(1)]})).unwrap();
        let before = s.document().clone();
        let mut with_history = node(3);
        let generation = GenerationId::from_bytes([3; 16]);
        with_history.history.push(Generation { id: generation, outputs: vec![] });
        with_history.active = Some(generation);
        for params in [
            json!({"canvas": C, "nodes": []}),
            // An edge to a node that is neither pasted nor present.
            json!({"canvas": C, "nodes": [node(2)], "edges": [Edge { to: Endpoint { node: NodeId::from_bytes([5; 16]), port: "in".into() }, ..edge() }]}),
            json!({"canvas": C, "nodes": [node(1)]}),
            json!({"canvas": C, "nodes": [with_history]}),
            json!({"canvas": CanvasId::from_bytes([7; 16]), "nodes": [node(4)]}),
        ] {
            assert!(s.enabled("canvas.paste", &params).is_err(), "{params}");
            assert!(s.dispatch("canvas.paste", &params).is_err(), "{params}");
        }
        assert_eq!(s.document(), &before);
    }
}

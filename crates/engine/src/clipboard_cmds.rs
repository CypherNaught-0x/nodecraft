//! Pasting a group of nodes and the edges between them as one undo step (copy, paste,
//! duplicate). The caller supplies fresh ids directly or in an explicit copy map, so replay
//! stays deterministic.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Edge, MAX_CHANGES, Node};
use nodecraft_types::{CanvasId, Error, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasteParams {
    pub canvas: CanvasId,
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// With this map, nodes retain their original ids in the input; the command rebases
    /// node ids, edge endpoints and supported embedded references together. Edge ids are
    /// still supplied fresh by the caller.
    #[serde(default)]
    pub node_ids: BTreeMap<NodeId, NodeId>,
}

fn paste(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let mut p: PasteParams = params(value)?;
    if p.nodes.is_empty() {
        return Err(invalid("nothing to paste"));
    }
    if p.nodes.len().saturating_add(p.edges.len()) > MAX_CHANGES {
        return Err(invalid("paste exceeds the operation limit"));
    }
    if p.nodes.iter().any(|node| !node.history.is_empty() || node.active.is_some()) {
        return Err(invalid("pasted nodes cannot carry generation history"));
    }
    if !p.node_ids.is_empty() {
        if p.node_ids.len() != p.nodes.len() {
            return Err(invalid("node_ids must map every pasted node to one unique fresh id"));
        }
        let originals: BTreeSet<_> = p.nodes.iter().map(|node| node.id).collect();
        let fresh: BTreeSet<_> = p.node_ids.values().copied().collect();
        if originals.len() != p.nodes.len() || fresh.len() != p.nodes.len() || p.node_ids.keys().any(|id| !originals.contains(id)) {
            return Err(invalid("node_ids must map every pasted node to one unique fresh id"));
        }
        for node in &mut p.nodes {
            node.id = p.node_ids.get(&node.id).copied().ok_or_else(|| invalid("a pasted node has no fresh id"))?;
        }
        for edge in &mut p.edges {
            edge.from.node = p.node_ids.get(&edge.from.node).copied().unwrap_or(edge.from.node);
            edge.to.node = p.node_ids.get(&edge.to.node).copied().unwrap_or(edge.to.node);
        }
        crate::deck_cmds::remap_sources(&mut p.nodes, &p.node_ids)?;
    }
    // Nodes of installed kinds must match their kind: ports as the instance declares them and
    // parameters its schema accepts. Unknown kinds (e.g. from a plugin not installed here) are kept
    // as data, like a loaded project keeps them.
    for node in &p.nodes {
        if let Ok(kind) = c.kinds.get(&node.kind) {
            kind.validate_instance(node, false).map_err(|error| Error::new(error.kind, format!("pasted {} node: {}", node.kind, error.message)))?;
        }
    }
    // Each distinct reference is verified once per paste: the same media in many nodes or
    // list items streams its bytes once.
    let mut verified = std::collections::HashSet::new();
    for node in &p.nodes {
        // params() has bounded the JSON and detached graph payload. Bound list traversal
        // independently before growing the explicit reference stack.
        node.validate_payload()?;
        for root in node.params.values().chain(node.config.values()) {
            validate_assets(c, root, &mut verified)?;
        }
    }
    let ids: Vec<_> = p.nodes.iter().map(|node| node.id).collect();
    let canvas = p.canvas;
    let changes = Changeset::new(
        p.nodes.into_iter().map(|node| Change::AddNode { canvas, node }).chain(p.edges.into_iter().map(|edge| Change::Connect { canvas, edge })).collect(),
    );
    Ok(Effect::Edit { changes, result: json!({"nodes": ids}), gesture: None })
}

fn validate_assets<'v>(
    c: &Context<'_>,
    root: &'v nodecraft_types::Value,
    verified: &mut std::collections::HashSet<&'v nodecraft_types::AssetRef>,
) -> Result<(), Error> {
    let mut stack = vec![(root, 0usize)];
    let mut seen = 0usize;
    while let Some((value, depth)) = stack.pop() {
        seen = seen.saturating_add(1);
        if depth > nodecraft_graph::MAX_VALUE_DEPTH || seen > nodecraft_graph::MAX_VALUE_ITEMS {
            return Err(invalid("asset references exceed value limits"));
        }
        match value {
            nodecraft_types::Value::Media(asset) => {
                if !verified.contains(asset) {
                    c.validate_asset(asset)?;
                    verified.insert(asset);
                }
            }
            nodecraft_types::Value::List(values) => {
                if seen.saturating_add(stack.len()).saturating_add(values.len()) > nodecraft_graph::MAX_VALUE_ITEMS {
                    return Err(invalid("asset references exceed value limits"));
                }
                stack.extend(values.iter().map(|value| (value, depth.saturating_add(1))));
            }
            _ => {}
        }
    }
    Ok(())
}

fn enabled_paste(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    // Full validation only when previewing; dispatch runs `paste` right after this check and
    // stages the changeset once itself, so it shouldn't verify every asset twice.
    if matches!(c.phase, crate::commands::Phase::Dispatch) {
        return c.editable();
    }
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
            "edges": {"type": "array", "maxItems": MAX_CHANGES, "items": edge},
            "node_ids": {"type":"object", "maxProperties":MAX_CHANGES,
                "propertyNames":{"type":"string","format":"uuid"},
                "additionalProperties":{"type":"string","format":"uuid"},
                "description":"Optional original-to-fresh node ids. Input nodes and edge endpoints keep their original ids; edge ids must already be fresh. Rebases supported embedded references too. Omit when supplying already-rebased nodes."}},
        "$defs": add.get("$defs").cloned().unwrap_or_else(|| json!({}))})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "canvas.paste",
        label: "Paste nodes",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: paste_schema,
        params_doc: "{canvas: CanvasId, nodes: [graph::Node], edges?: [graph::Edge], node_ids?: {original NodeId: fresh NodeId}}; inserts the group as one undo step, without generation history. Normally all ids are supplied fresh. With node_ids, supply original node ids and edge endpoints plus a complete one-to-one fresh-node map; the command rebases them and supported Deck source references together. Edge ids must already be fresh. Uncopied Deck sources remain unchanged.",
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
    fn explicit_copy_ids_rebase_nodes_and_wire_endpoints_as_one_undo_step() {
        let mut s = session();
        s.dispatch("canvas.paste", &json!({"canvas":C,"nodes":[node(1),node(2)]})).unwrap();
        let before = s.document().clone();
        let mut wire = edge();
        wire.id = EdgeId::from_bytes([19; 16]);
        let ids = std::collections::BTreeMap::from([(node(1).id, node(11).id), (node(2).id, node(12).id)]);
        let params = json!({"canvas":C,"nodes":[node(1),node(2)],"edges":[wire],"node_ids":ids});
        s.enabled("canvas.paste", &params).unwrap();
        s.dispatch("canvas.paste", &params).unwrap();
        let canvas = s.document().canvases().get(&C).unwrap();
        let copied = canvas.edges().get(&EdgeId::from_bytes([19; 16])).unwrap();
        assert_eq!((copied.from.node, copied.to.node), (node(11).id, node(12).id));
        assert!(canvas.nodes().contains_key(&node(11).id) && canvas.nodes().contains_key(&node(12).id));
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), &before);
    }

    #[test]
    fn explicit_copy_ids_reject_partial_extraneous_duplicate_and_taken_targets_without_edits() {
        let mut s = session();
        s.dispatch("canvas.paste", &json!({"canvas":C,"nodes":[node(1),node(2)]})).unwrap();
        let before = s.document().clone();
        let revision = s.revision();
        for ids in [
            std::collections::BTreeMap::from([(node(1).id, node(11).id)]),
            std::collections::BTreeMap::from([(node(1).id, node(11).id), (node(3).id, node(12).id)]),
            std::collections::BTreeMap::from([(node(1).id, node(11).id), (node(2).id, node(11).id)]),
            std::collections::BTreeMap::from([(node(1).id, node(1).id), (node(2).id, node(12).id)]),
        ] {
            let params = json!({"canvas":C,"nodes":[node(1),node(2)],"node_ids":ids});
            assert!(s.enabled("canvas.paste", &params).is_err());
            assert!(s.dispatch("canvas.paste", &params).is_err());
            assert_eq!(s.document(), &before);
            assert_eq!(s.revision(), revision);
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
        with_history.history.push(Generation::new(generation, vec![]));
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
    #[test]
    fn direct_paste_requires_actual_destination_assets_including_nested_config() {
        let mut s = session();
        let mut n = node(1);
        let mut reference =
            nodecraft_types::AssetRef { hash: nodecraft_types::Blake3::from_bytes([7; 32]), mime: "text/plain".parse().unwrap(), meta: Default::default() };
        n.config.insert("nested".into(), nodecraft_types::Value::List(vec![nodecraft_types::Value::Media(reference.clone())]));
        let before = s.document().clone();
        let p = json!({"canvas":C,"nodes":[n]});
        assert!(s.enabled("canvas.paste", &p).is_err());
        assert!(s.dispatch("canvas.paste", &p).is_err());
        assert_eq!(s.document(), &before);
        reference.hash = s.import_asset(b"public asset fixture").unwrap();
        let mut n = node(1);
        n.config.insert("nested".into(), nodecraft_types::Value::List(vec![nodecraft_types::Value::Media(reference.clone())]));
        let p = json!({"canvas":C,"nodes":[n]});
        s.enabled("canvas.paste", &p).unwrap();
        s.dispatch("canvas.paste", &p).unwrap();
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), &before);
        s.dispatch("edit.redo", &json!({})).unwrap();
        reference.mime = "image/png".parse().unwrap();
        let mut n = node(2);
        n.params.insert("asset".into(), nodecraft_types::Value::Media(reference));
        let p = json!({"canvas":C,"nodes":[n]});
        assert!(s.enabled("canvas.paste", &p).is_err());
        assert!(s.dispatch("canvas.paste", &p).is_err());
    }
    #[test]
    fn paste_verifies_each_distinct_reference_once() {
        let mut s = session();
        let shared =
            nodecraft_types::AssetRef { hash: s.import_asset(b"shared fixture").unwrap(), mime: "text/plain".parse().unwrap(), meta: Default::default() };
        let other = nodecraft_types::AssetRef { hash: s.import_asset(b"other fixture").unwrap(), ..shared.clone() };
        let nodes: Vec<_> = (1..=3)
            .map(|n| {
                let mut node = node(n);
                let media = nodecraft_types::Value::Media(shared.clone());
                node.config.insert("nested".into(), nodecraft_types::Value::List(vec![media.clone(), media.clone()]));
                node.params.insert("asset".into(), if n == 3 { nodecraft_types::Value::Media(other.clone()) } else { media });
                node
            })
            .collect();
        nodecraft_format::take_validation_metrics();
        s.dispatch("canvas.paste", &json!({"canvas":C,"nodes":nodes})).unwrap();
        // Nine references, two distinct: one check each, not one per occurrence.
        assert_eq!(nodecraft_format::take_validation_metrics().reference_checks, 2);
    }
}

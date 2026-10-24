//! Copy, cut, paste and duplicate on the canvas. Nodes travel through the OS clipboard as
//! versioned JSON, so a copy can be pasted into another project or another NodeCraft window.
//! Pasted nodes get fresh ids and land centred on the pointer; generation results stay behind
//! (pasted nodes start without history).
use crate::{ids, selection::Selection};
use egui::{Pos2, Rect, Vec2};
use nodecraft_graph::{Canvas, Edge, Node, Position, Project};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
thread_local! { pub(crate) static PROJECTED_NODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
pub const FORMAT: &str = "nodecraft.nodes";
pub const VERSION: u32 = 1;
/// Largest clipboard text read as nodes; anything bigger is not parsed.
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
/// Most nodes and edges one paste carries.
pub const MAX_ITEMS: usize = nodecraft_graph::MAX_CHANGES;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    pub format: String,
    pub version: u32,
    /// Informational origin only; destination bytes, not this UUID, prove media availability.
    #[serde(default)]
    pub project: Option<ProjectId>,
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

/// The selected nodes and every edge between two of them, without generation results.
pub fn payload(project: ProjectId, canvas: &Canvas, selection: &Selection) -> Option<Payload> {
    projection::check(project, canvas, selection).ok()?;
    build(project, canvas, selection)
}
/// The actual UI path additionally verifies references before constructing projected nodes.
pub(crate) fn checked_payload(session: &nodecraft_engine::Session, canvas: CanvasId, selection: &Selection) -> Result<Option<Payload>, nodecraft_types::Error> {
    let Some(scene) = session.document().canvases().get(&canvas) else { return Ok(None) };
    projection::check(session.document().id(), scene, selection).map_err(|_| {
        nodecraft_types::Error::new(nodecraft_types::ErrorKind::InvalidInput, "This selection is too large to copy and paste back in one go; copy fewer nodes.")
    })?;
    for id in selection.nodes() {
        if let Some(node) = scene.nodes().get(id) {
            validate_assets(session, node.params.values().chain(node.config.values()))?;
        }
    }
    Ok(build(session.document().id(), scene, selection))
}
pub(crate) fn validate_assets<'a>(session: &nodecraft_engine::Session, roots: impl Iterator<Item = &'a Value>) -> Result<(), nodecraft_types::Error> {
    for root in roots {
        let mut stack = vec![(root, 0usize)];
        let mut seen = 0usize;
        while let Some((value, depth)) = stack.pop() {
            seen = seen.saturating_add(1);
            if depth > nodecraft_graph::MAX_VALUE_DEPTH || seen > nodecraft_graph::MAX_VALUE_ITEMS {
                return Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::InvalidInput, "clipboard values exceed limits"));
            }
            match value {
                Value::Media(asset) => session.validate_asset(asset)?,
                Value::List(values) => {
                    if seen.saturating_add(stack.len()).saturating_add(values.len()) > nodecraft_graph::MAX_VALUE_ITEMS {
                        return Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::InvalidInput, "clipboard values exceed limits"));
                    }
                    stack.extend(values.iter().map(|v| (v, depth.saturating_add(1))));
                }
                _ => {}
            }
        }
    }
    Ok(())
}
fn build(project: ProjectId, canvas: &Canvas, selection: &Selection) -> Option<Payload> {
    let nodes: Vec<Node> = selection
        .nodes()
        .iter()
        .filter_map(|id| canvas.nodes().get(id))
        .map(|node| {
            #[cfg(test)]
            PROJECTED_NODES.with(|n| n.set(n.get() + 1));
            let Node { id, kind, title, pos, params, config, inputs, outputs, history: _, active: _, tag, field } = node;
            Node {
                id: *id,
                kind: kind.clone(),
                title: title.clone(),
                pos: *pos,
                params: params.clone(),
                config: config.clone(),
                inputs: inputs.clone(),
                outputs: outputs.clone(),
                history: Vec::new(),
                active: None,
                tag: *tag,
                field: *field,
            }
        })
        .collect();
    if nodes.is_empty() {
        return None;
    }
    let edges = canvas.edges().values().filter(|e| selection.contains(e.from.node) && selection.contains(e.to.node)).cloned().collect();
    Some(Payload { format: FORMAT.into(), version: VERSION, project: Some(project), nodes, edges })
}
mod deck_sources;
mod projection;

pub fn to_text(payload: &Payload) -> Option<String> {
    serde_json::to_string(payload).ok()
}

/// What clipboard text holds.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    /// Not a NodeCraft copy: paste it as plain text.
    Text,
    /// A NodeCraft copy this version can't paste, and why.
    Unreadable(&'static str),
    Nodes(Payload),
}

pub fn parse(text: &str) -> Parsed {
    let trimmed = text.trim_start();
    // Look for the marker in the first 4 KiB as bytes, so a multibyte character at the cut can't
    // hide it.
    let head = trimmed.as_bytes().get(..trimmed.len().min(4096)).unwrap_or_default();
    let ours = trimmed.starts_with('{') && head.windows(FORMAT.len()).any(|window| window == FORMAT.as_bytes());
    if !ours {
        return Parsed::Text;
    }
    if text.len() > MAX_BYTES {
        return Parsed::Unreadable("the copy is larger than 16 MiB");
    }
    let Ok(payload) = serde_json::from_str::<Payload>(text) else {
        return Parsed::Unreadable("it was copied by a different NodeCraft version or is damaged");
    };
    if payload.format != FORMAT || payload.version != VERSION {
        Parsed::Unreadable("it was copied by a different NodeCraft version")
    } else if payload.nodes.is_empty() {
        Parsed::Unreadable("it holds no nodes")
    } else if payload.nodes.len().saturating_add(payload.edges.len()) > MAX_ITEMS {
        Parsed::Unreadable("it holds more than 10,000 nodes and noodles")
    } else if has_duplicates(payload.nodes.iter().map(|n| n.id)) || has_duplicates(payload.edges.iter().map(|e| e.id)) {
        Parsed::Unreadable("it holds the same node or noodle twice")
    } else {
        Parsed::Nodes(payload)
    }
}

fn has_duplicates<T: Ord>(ids: impl Iterator<Item = T>) -> bool {
    let mut seen = BTreeSet::new();
    ids.into_iter().any(|id| !seen.insert(id))
}

/// Whether any node carries a media reference (an asset hash) in its parameters or config.
pub fn has_media(payload: &Payload) -> bool {
    let mut stack: Vec<&Value> = payload.nodes.iter().flat_map(|n| n.params.values().chain(n.config.values())).collect();
    while let Some(value) = stack.pop() {
        match value {
            Value::Media(_) => return true,
            Value::List(items) => stack.extend(items.iter()),
            _ => {}
        }
    }
    false
}

/// Give the payload fresh ids (unused in `project` and among themselves), keep only edges between
/// pasted nodes, drop tags and fields `canvas` doesn't have, and move the group so the centre of
/// its cards (each `card` sized) lands on `centre`.
pub fn place(
    project: &Project,
    canvas: CanvasId,
    payload: Payload,
    centre: Pos2,
    rect_of: impl Fn(&Node) -> Rect,
    salt: u64,
) -> Option<(Vec<Node>, Vec<Edge>)> {
    let bounds = group_centre(&payload.nodes, rect_of);
    let shift = bounds.map_or(Vec2::ZERO, |c| centre - c);
    let target = project.canvases().get(&canvas);
    let mut chosen_nodes = BTreeSet::new();
    let mut map: BTreeMap<NodeId, NodeId> = BTreeMap::new();
    let mut nodes = Vec::with_capacity(payload.nodes.len());
    for mut node in payload.nodes {
        let fresh = ids::fresh((node.id, salt), |bytes| {
            let id = NodeId::from_bytes(bytes);
            chosen_nodes.contains(&id) || project.canvases().values().any(|c| c.nodes().contains_key(&id))
        })
        .map(NodeId::from_bytes)?;
        chosen_nodes.insert(fresh);
        map.insert(node.id, fresh);
        node.id = fresh;
        node.pos = Position { x: node.pos.x + shift.x, y: node.pos.y + shift.y };
        if node.tag.is_some_and(|tag| !target.is_some_and(|c| c.tags().contains_key(&tag))) {
            node.tag = None;
        }
        if node.field.is_some_and(|field| !target.is_some_and(|c| c.fields().contains_key(&field))) {
            node.field = None;
        }
        nodes.push(node);
    }
    deck_sources::remap(&mut nodes, &map)?;
    let mut chosen_edges = BTreeSet::new();
    let mut edges = Vec::new();
    for mut edge in payload.edges {
        let (Some(from), Some(to)) = (map.get(&edge.from.node), map.get(&edge.to.node)) else { continue };
        let fresh = ids::fresh((edge.id, salt), |bytes| {
            let id = EdgeId::from_bytes(bytes);
            chosen_edges.contains(&id) || project.canvases().values().any(|c| c.edges().contains_key(&id))
        })
        .map(EdgeId::from_bytes)?;
        chosen_edges.insert(fresh);
        edge.id = fresh;
        edge.from.node = *from;
        edge.to.node = *to;
        edges.push(edge);
    }
    Some((nodes, edges))
}

/// The centre of the cards of `nodes`, each card's rect given by `rect_of`.
pub fn group_centre(nodes: &[Node], rect_of: impl Fn(&Node) -> Rect) -> Option<Pos2> {
    let bounds = nodes.iter().map(rect_of).fold(Rect::NOTHING, |r, c| r.union(c));
    bounds.is_finite().then(|| bounds.center())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Change, Endpoint, Generation, Port};
    use nodecraft_types::{CanvasId, GenerationId, Modality, PortType, ProjectId};

    const C: CanvasId = CanvasId::from_bytes([1; 16]);

    fn project() -> Project {
        let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Clip").unwrap();
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        let port_type = PortType { modality: Modality::Text, many: false };
        for (n, x) in [(1u8, 0.0), (2, 400.0), (3, 800.0)] {
            let mut node = Node::new(NodeId::from_bytes([n; 16]), "test");
            node.pos = Position { x, y: 100.0 };
            node.inputs.push(Port { name: "in".into(), port_type });
            node.outputs.push(Port { name: "out".into(), port_type });
            if n == 1 {
                let generation = GenerationId::from_bytes([1; 16]);
                node.history.push(Generation::new(generation, vec![]));
                node.active = Some(generation);
            }
            Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
        }
        for (e, (a, b)) in [(10u8, (1u8, 2u8)), (11, (2, 3))] {
            let edge = Edge {
                id: EdgeId::from_bytes([e; 16]),
                from: Endpoint { node: NodeId::from_bytes([a; 16]), port: "out".into() },
                to: Endpoint { node: NodeId::from_bytes([b; 16]), port: "in".into() },
                order: 0,
            };
            Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
        }
        project
    }

    #[test]
    fn copies_round_trip_with_inner_edges_and_without_results() {
        let project = project();
        let canvas = project.canvases().get(&C).unwrap();
        let mut selection = Selection::default();
        selection.replace([NodeId::from_bytes([1; 16]), NodeId::from_bytes([2; 16])].into_iter().collect());
        let copied = payload(project.id(), canvas, &selection).unwrap();
        assert_eq!(copied.nodes.len(), 2);
        assert_eq!(copied.edges.iter().map(|e| e.id).collect::<Vec<_>>(), vec![EdgeId::from_bytes([10; 16])]);
        assert!(copied.nodes.iter().all(|n| n.history.is_empty() && n.active.is_none()));
        let text = to_text(&copied).unwrap();
        assert_eq!(parse(&text), Parsed::Nodes(copied.clone()));

        let card = Vec2::new(300.0, 200.0);
        let rect_of = |n: &Node| Rect::from_min_size(Pos2::new(n.pos.x, n.pos.y), card);
        let (nodes, edges) = place(&project, C, copied, Pos2::new(1000.0, 1000.0), rect_of, 7).unwrap();
        // Fresh ids, remapped edge, and the group centred on the target.
        assert!(nodes.iter().all(|n| !canvas.nodes().contains_key(&n.id)));
        let (a, b) = (nodes.first().unwrap(), nodes.get(1).unwrap());
        assert_ne!(a.id, b.id);
        assert_eq!((edges.first().unwrap().from.node, edges.first().unwrap().to.node), (a.id, b.id));
        // Card centres: corners 400 apart, so the group's card box is centred on the target.
        assert_eq!(((a.pos.x + b.pos.x + card.x) / 2.0, a.pos.y + card.y / 2.0), (1000.0, 1000.0));
        let mut pasted = project.clone();
        let changes = nodes
            .into_iter()
            .map(|node| Change::AddNode { canvas: C, node })
            .chain(edges.into_iter().map(|edge| Change::Connect { canvas: C, edge }))
            .collect();
        nodecraft_graph::Changeset::new(changes).apply(&mut pasted).unwrap();
    }

    #[test]
    fn foreign_or_hostile_text_is_not_nodes() {
        for text in ["", "hello", "{}", r#"{"format":"other","version":1,"nodes":[]}"#, "[1,2]"] {
            assert_eq!(parse(text), Parsed::Text, "{text}");
        }
        // Ours, but not readable here: say so rather than paste the JSON as text.
        for text in
            [r#"{"format":"nodecraft.nodes","version":2,"nodes":[]}"#.to_owned(), r#"{"format":"nodecraft.nodes","version":1,"nodes":[],"x":1}"#.to_owned()]
        {
            assert!(matches!(parse(&text), Parsed::Unreadable(_)), "{text}");
        }
        let empty = Payload { format: FORMAT.into(), version: VERSION, project: None, nodes: vec![], edges: vec![] };
        assert_eq!(parse(&to_text(&empty).unwrap()), Parsed::Unreadable("it holds no nodes"));
        assert!(matches!(parse(&format!("{{\"format\":\"{FORMAT}\"{}", " ".repeat(MAX_BYTES))), Parsed::Unreadable(_)));
        assert!(payload(project().id(), project().canvases().get(&C).unwrap(), &Selection::default()).is_none());
    }

    #[test]
    fn tags_and_fields_the_target_canvas_lacks_are_dropped() {
        let project = project();
        let mut node = project.canvases().get(&C).unwrap().nodes().values().next().unwrap().clone();
        node.history.clear();
        node.active = None;
        node.tag = Some(nodecraft_types::TagId::from_bytes([8; 16]));
        node.field = Some(nodecraft_types::FieldId::from_bytes([9; 16]));
        let payload = Payload { format: FORMAT.into(), version: VERSION, project: None, nodes: vec![node], edges: vec![] };
        let (nodes, _) = place(&project, C, payload, Pos2::ZERO, |n| Rect::from_min_size(Pos2::new(n.pos.x, n.pos.y), Vec2::new(300.0, 200.0)), 1).unwrap();
        assert!(nodes.iter().all(|n| n.tag.is_none() && n.field.is_none()));
    }

    #[test]
    fn markers_past_a_multibyte_cut_duplicates_and_media_are_handled() {
        let project = project();
        let canvas = project.canvases().get(&C).unwrap();
        let mut selection = Selection::default();
        selection.replace([NodeId::from_bytes([2; 16])].into_iter().collect());
        let copied = payload(project.id(), canvas, &selection).unwrap();
        // The marker sits near the start; a title of é's straddles the 4 KiB cut.
        let mut long = copied.clone();
        let text = |title: &str| {
            let mut p = long.clone();
            p.nodes.first_mut().unwrap().title = title.to_owned();
            to_text(&p).unwrap()
        };
        let title = (0..2).map(|pad| "a".repeat(pad) + &"é".repeat(2100)).find(|t| !text(t).is_char_boundary(4096)).unwrap();
        long.nodes.first_mut().unwrap().title = title;
        assert!(matches!(parse(&to_text(&long).unwrap()), Parsed::Nodes(_)), "the multibyte cut must not hide the marker");
        // Duplicate ids are refused.
        let mut twice = copied.clone();
        let first = twice.nodes.first().cloned().unwrap();
        twice.nodes.push(first);
        assert_eq!(parse(&to_text(&twice).unwrap()), Parsed::Unreadable("it holds the same node or noodle twice"));
        // Media references are detected, nested in lists too.
        assert!(!has_media(&copied));
        let mut media = copied;
        let asset =
            nodecraft_types::AssetRef { hash: nodecraft_types::Blake3::from_bytes([1; 32]), mime: "image/png".parse().unwrap(), meta: Default::default() };
        media.nodes.first_mut().unwrap().params.insert("asset".into(), Value::List(vec![Value::Media(asset)]));
        assert!(has_media(&media));
    }
}

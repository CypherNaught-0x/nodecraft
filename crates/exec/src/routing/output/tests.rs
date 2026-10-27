use super::*;
use nodecraft_graph::{Change, Edge, Project};
use nodecraft_types::{CanvasId, EdgeId, Modality, NodeId, ProjectId};
use serde_json::json;
const C: CanvasId = CanvasId::from_bytes([149; 16]);
fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
fn from(n: u8) -> Endpoint {
    Endpoint { node: id(n), port: "out".into() }
}
fn add(p: &mut Project, kinds: &Registry, n: u8, kind: &str) {
    let node = kinds.get(kind).unwrap().instantiate_instance(id(n), Default::default(), &Default::default()).unwrap();
    Change::AddNode { canvas: C, node }.apply(p).unwrap();
}
fn wire(p: &mut Project, e: u8, a: u8, b: u8, order: u32) {
    Change::Connect { canvas: C, edge: Edge { id: EdgeId::from_bytes([e; 16]), from: from(a), to: Endpoint { node: id(b), port: "in".into() }, order } }
        .apply(p)
        .unwrap();
}
fn setup(kind: &str) -> (Project, Registry) {
    let kinds = Registry::builtins().unwrap();
    let mut p = Project::new(ProjectId::from_bytes([149; 16]), "Public Router metadata").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    add(&mut p, &kinds, 1, kind);
    add(&mut p, &kinds, 10, router::ID);
    add(&mut p, &kinds, 11, router::ID);
    (p, kinds)
}
#[test]
fn router949_scalar_list_and_duplicate_terminal_shapes_are_preserved() {
    for kind in ["source.text", "text.split"] {
        let (mut p, kinds) = setup(kind);
        wire(&mut p, 1, 1, 10, 0);
        wire(&mut p, 2, 10, 11, 0);
        let canvas = p.canvases().get(&C).unwrap();
        let direct = output_type(canvas, &kinds, &from(1)).unwrap();
        assert_eq!(output_type(canvas, &kinds, &from(10)).unwrap(), direct);
        assert_eq!(output_type(canvas, &kinds, &from(11)).unwrap(), direct);
        assert_eq!(output_sources(canvas, &kinds, &from(11)).unwrap().len(), 1);
        wire(&mut p, 3, 1, 10, 1);
        let canvas = p.canvases().get(&C).unwrap();
        assert!(output_type(canvas, &kinds, &from(11)).unwrap().many);
        assert_eq!(output_sources(canvas, &kinds, &from(11)).unwrap().len(), 2, "duplicate feeds retain their structural multiplicity");
    }
}
#[test]
fn router949_empty_branches_cycles_invalid_and_unknown_sources_never_become_scalar() {
    let (mut p, kinds) = setup("source.text");
    assert!(output_type(p.canvases().get(&C).unwrap(), &kinds, &from(10)).is_err());
    wire(&mut p, 1, 1, 10, 0);
    let clean = p.clone();
    assert!(output_type(clean.canvases().get(&C).unwrap(), &kinds, &from(10)).is_ok());
    wire(&mut p, 2, 11, 10, 1);
    let canvas = p.canvases().get(&C).unwrap();
    assert!(output_type(canvas, &kinds, &from(10)).is_err(), "a good feed cannot hide an empty Router branch");
    let mut raw = serde_json::to_value(canvas).unwrap();
    let edge = Edge { id: EdgeId::from_bytes([3; 16]), from: from(10), to: Endpoint { node: id(11), port: "in".into() }, order: 0 };
    raw["edges"][edge.id.to_string()] = serde_json::to_value(&edge).unwrap();
    let cycle: Canvas = serde_json::from_value(raw).unwrap();
    assert!(output_type(&cycle, &kinds, &from(10)).is_err());
    for corrupt in ["kind", "ports", "modality"] {
        let mut raw = serde_json::to_value(clean.canvases().get(&C).unwrap()).unwrap();
        match corrupt {
            "kind" => raw["nodes"][id(1).to_string()]["kind"] = json!("missing.source"),
            "ports" => raw["nodes"][id(10).to_string()]["outputs"][0]["name"] = json!("bad"),
            _ => raw["nodes"][id(10).to_string()]["config"]["modality"] = serde_json::to_value(nodecraft_types::Value::Bool(true)).unwrap(),
        }
        let broken: Canvas = serde_json::from_value(raw).unwrap();
        assert!(output_type(&broken, &kinds, &from(10)).is_err(), "{corrupt}");
    }
}
#[test]
fn router949_mixed_scalar_and_list_inputs_remain_a_list_and_keep_order() {
    let (mut p, kinds) = setup("source.text");
    add(&mut p, &kinds, 2, "text.split");
    wire(&mut p, 1, 2, 10, 0);
    wire(&mut p, 2, 1, 10, 1);
    wire(&mut p, 3, 10, 11, 0);
    let canvas = p.canvases().get(&C).unwrap();
    let sources = output_sources(canvas, &kinds, &from(11)).unwrap();
    assert_eq!(sources.iter().map(|s| s.node.id).collect::<Vec<_>>(), vec![id(2), id(1)]);
    assert_eq!(output_type(canvas, &kinds, &from(11)).unwrap(), PortType { modality: Modality::Text, many: true });
}

/// A real dynamic descriptor callback, rather than a counter inside the resolver under test.
struct Dynamic {
    descriptor: nodecraft_nodes::NodeDescriptor,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl nodecraft_nodes::NodeKind for Dynamic {
    fn descriptor(&self) -> &nodecraft_nodes::NodeDescriptor {
        &self.descriptor
    }
    fn instance<'a>(&'a self, _: &nodecraft_graph::Node) -> Result<std::borrow::Cow<'a, nodecraft_nodes::NodeDescriptor>, Error> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(std::borrow::Cow::Owned(self.descriptor.clone()))
    }
    fn mode(&self, _: &[PortType]) -> Option<nodecraft_nodes::ModeId> {
        None
    }
    fn plan(&self, _: &nodecraft_nodes::PlanCtx<'_>) -> Result<Vec<nodecraft_nodes::JobSpec>, Error> {
        Ok(Vec::new())
    }
}
#[test]
fn router949_duplicate_dynamic_sources_validate_once_without_losing_feeds() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (mut p, mut kinds) = setup("source.text");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut descriptor = kinds.get("source.text").unwrap().descriptor().clone();
    descriptor.id = "test.dynamic".into();
    descriptor.quick_add = None;
    kinds.register(Box::new(Dynamic { descriptor, calls: calls.clone() })).unwrap();
    let node = kinds.get("test.dynamic").unwrap().instantiate_instance(id(1), Default::default(), &Default::default()).unwrap();
    Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
    for n in 1..=16 {
        wire(&mut p, n, 1, 10, u32::from(n));
    }
    wire(&mut p, 30, 10, 11, 0);
    calls.store(0, Ordering::SeqCst);
    let canvas = p.canvases().get(&C).unwrap();
    let mut resolver = OutputResolver::new(canvas, &kinds);
    for root in [10, 11, 10] {
        let sources = resolver.sources(&from(root)).unwrap();
        assert_eq!(sources.len(), 16);
        assert!(sources.iter().all(|source| source.node.id == id(1)));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one immutable source descriptor must be checked once per resolver, with all duplicate feeds preserved");
    }
    assert!(resolver.output_type(&from(11)).unwrap().many);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = OutputResolver::new(canvas, &kinds).sources(&from(11)).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "a new canvas/chooser capture never borrows a previous validation proof");
}

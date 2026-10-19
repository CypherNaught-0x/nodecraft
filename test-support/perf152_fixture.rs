// Shared command-built fixture: the landed #464 200-node rows remain one consumer.
use nodecraft_engine::Session;
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
use nodecraft_types::{CanvasId, EdgeId, Error, Modality, NodeId, ParamSchema, PortType, ProjectId};
use serde_json::json;

pub fn id(n: usize) -> NodeId {
    NodeId::from_bytes(u128::try_from(n + 1000).unwrap().to_be_bytes())
}
pub fn canvas() -> CanvasId {
    CanvasId::from_bytes([152; 16])
}
pub fn empty() -> Project {
    let mut p = Project::new(ProjectId::from_bytes([152; 16]), "Performance fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas(), "Canvas") }.apply(&mut p).unwrap();
    p
}
struct Relay(NodeDescriptor);
impl NodeKind for Relay {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        None
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![])
    }
}
pub fn session(p: Project) -> Session {
    let port = |name: &str, many| nodecraft_graph::Port { name: name.into(), port_type: PortType { modality: Modality::Text, many } };
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    kinds
        .register(Box::new(Relay(NodeDescriptor {
            id: "perf.many_relay".into(),
            label: "Benchmark relay".into(),
            category: Category::Action,
            inputs: vec![port("in", true)],
            outputs: vec![port("out", false)],
            params: ParamSchema::default(),
            quick_add: None,
        })))
        .unwrap();
    Session::with_registries(p, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap()
}
pub fn build(s: &mut Session, canvas: CanvasId, shape: &str, count: usize, relay: &str) {
    assert!((2..=1000).contains(&count));
    let source = |n: usize| match shape {
        "chain" | "fan_out" => n == 0,
        "fan_in" => n != count - 1,
        "rows" => n.is_multiple_of(10),
        "disconnected" => true,
        _ => panic!("topology"),
    };
    for n in 0..count {
        let kind = if source(n) { "source.text" } else { relay };
        s.dispatch("node.create", &json!({"canvas":canvas,"id":id(n),"kind":kind,"pos":{"x":(n%10) as f32*400.,"y":(n/10) as f32*320.}})).unwrap();
    }
    let edges: Vec<_> = match shape {
        "chain" => (1..count).map(|n| (n - 1, n)).collect(),
        "fan_out" => (1..count).map(|n| (0, n)).collect(),
        "fan_in" => (0..count - 1).map(|n| (n, count - 1)).collect(),
        "rows" => (1..count).filter(|n| n % 10 != 0).map(|n| (n - 1, n)).collect(),
        "disconnected" => vec![],
        _ => panic!("topology"),
    };
    for (n, (a, b)) in edges.into_iter().enumerate() {
        let edge = Edge {
            id: EdgeId::from_bytes(u128::try_from(n + 5000).unwrap().to_be_bytes()),
            from: Endpoint { node: id(a), port: "out".into() },
            to: Endpoint { node: id(b), port: "in".into() },
            order: u32::try_from(n).unwrap(),
        };
        s.dispatch("edge.connect", &json!({"canvas":canvas,"edge":edge})).unwrap();
    }
    for n in (0..count).filter(|n| source(*n)) {
        s.dispatch(
            "node.param_set",
            &json!({"canvas":canvas,"id":id(n),"name":"text","value":{"type":"text","value":format!("Row {}: a quiet forest at dawn.",n/10)}}),
        )
        .unwrap();
    }
}

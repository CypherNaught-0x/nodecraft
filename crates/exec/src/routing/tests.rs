use super::*;
use crate::planner::{InputSource, Planned, Reason, RunMode, plan};
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Generation, Port, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, Registry};
use nodecraft_types::{CanvasId, EdgeId, Error, GenerationId, Modality, ParamSchema, PortType, ProjectId, Value};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const TEXT: PortType = PortType { modality: Modality::Text, many: false };
const LIST: PortType = PortType { modality: Modality::Text, many: true };

struct Relay(NodeDescriptor);
impl NodeKind for Relay {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        (!inputs.is_empty()).then(|| ModeId::new("relay")).and_then(Result::ok)
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let value = Value::List(ctx.inputs.iter().map(|input| input.value.clone()).collect());
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value }])
    }
}

fn kinds() -> Registry {
    let mut kinds = Registry::builtins().unwrap();
    let relay = |id: &str, input: PortType| NodeDescriptor {
        id: id.into(),
        label: "Relay".into(),
        category: Category::Generation,
        inputs: vec![Port { name: "in".into(), port_type: input }],
        outputs: vec![Port { name: "out".into(), port_type: TEXT }],
        params: ParamSchema::default(),
        quick_add: None,
    };
    kinds.register(Box::new(Relay(relay("test.list", LIST)))).unwrap();
    kinds.register(Box::new(Relay(relay("test.single", TEXT)))).unwrap();
    kinds
}
fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
fn text(n: u8, content: &str) -> Change {
    let values = [("text".to_owned(), Value::Text(content.into()))].into_iter().collect();
    Change::AddNode { canvas: C, node: kinds().get("source.text").unwrap().descriptor().instantiate(id(n), &values).unwrap() }
}
fn router(n: u8) -> Change {
    Change::AddNode { canvas: C, node: kinds().get(router::ID).unwrap().instantiate_instance(id(n), Default::default(), &Default::default()).unwrap() }
}
fn relay(n: u8, kind: &str, result: Option<&str>) -> Change {
    let mut node = kinds().get(kind).unwrap().descriptor().instantiate(id(n), &Default::default()).unwrap();
    if let Some(result) = result {
        let g = GenerationId::from_bytes([n; 16]);
        node.history.push(Generation::new(g, vec![Value::Text(result.into())]));
        node.active = Some(g);
    }
    Change::AddNode { canvas: C, node }
}
fn wire(e: u8, from: u8, to: u8, order: u32) -> Change {
    Change::Connect {
        canvas: C,
        edge: Edge {
            id: EdgeId::from_bytes([e; 16]),
            from: Endpoint { node: id(from), port: "out".into() },
            to: Endpoint { node: id(to), port: "in".into() },
            order,
        },
    }
}
fn project(changes: Vec<Change>) -> Project {
    let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Routers").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    Changeset::new(changes).apply(&mut p).unwrap();
    p
}
fn ids(ns: &[u8]) -> BTreeSet<NodeId> {
    ns.iter().map(|n| id(*n)).collect()
}

#[test]
fn consumers_read_through_routers_and_router_chains_in_order() {
    // 1 "a", 2 "b" → router 10 → router 11 ← 3 "c";  router 11 → list relay 20.
    let p = project(vec![
        text(1, "a"),
        text(2, "b"),
        text(3, "c"),
        router(10),
        router(11),
        relay(20, "test.list", None),
        wire(50, 1, 10, 0),
        wire(51, 2, 10, 1),
        wire(52, 10, 11, 0),
        wire(53, 3, 11, 1),
        wire(54, 11, 20, 0),
    ]);
    let canvas = p.canvases().get(&C).unwrap();
    let resolved = incoming(canvas);
    let feeds: Vec<NodeId> = resolved.get(&id(20)).unwrap().iter().map(|(_, _, from, _)| *from).collect();
    assert_eq!(feeds, vec![id(1), id(2), id(3)]);
    let Planned::Ready(plan) = plan(canvas, &kinds(), &RunMode::Selected, &ids(&[20]), &BTreeSet::new()).unwrap() else { panic!() };
    let text = |t: &str| InputSource::Value(Value::Text(t.into()));
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps.first().unwrap().inputs.first().unwrap().from, vec![text("a"), text("b"), text("c")]);
}

#[test]
fn routers_never_run_and_dependencies_pass_through_them() {
    // relay 2 (no result) → router 10 → relay 3: with-upstream runs 2 then 3, never 10.
    let p = project(vec![
        text(1, "a"),
        relay(2, "test.single", None),
        router(10),
        relay(3, "test.single", None),
        wire(50, 1, 2, 0),
        wire(51, 2, 10, 0),
        wire(52, 10, 3, 0),
    ]);
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Ready(plan) = plan(canvas, &kinds(), &RunMode::WithUpstream, &ids(&[3]), &BTreeSet::new()).unwrap() else { panic!() };
    assert_eq!(plan.steps.iter().map(|s| s.node).collect::<Vec<_>>(), vec![id(2), id(3)]);
    assert_eq!(plan.edges, vec![(id(2), id(3))]);
    // Selecting a router alone is nothing to run; with others, it is dropped.
    assert!(crate::planner::plan(canvas, &kinds(), &RunMode::Selected, &ids(&[10]), &BTreeSet::new()).is_err());
    let Planned::Ready(plan) = crate::planner::plan(canvas, &kinds(), &RunMode::Selected, &ids(&[2, 10]), &BTreeSet::new()).unwrap() else { panic!() };
    assert_eq!(plan.steps.iter().map(|s| s.node).collect::<Vec<_>>(), vec![id(2)]);
}

#[test]
fn a_single_input_fed_several_values_through_a_router_is_blocked() {
    let p = project(vec![text(1, "a"), text(2, "b"), router(10), relay(3, "test.single", None), wire(50, 1, 10, 0), wire(51, 2, 10, 1), wire(52, 10, 3, 0)]);
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Blocked(blockers) = plan(canvas, &kinds(), &RunMode::Selected, &ids(&[3]), &BTreeSet::new()).unwrap() else { panic!() };
    assert!(blockers.iter().any(|b| matches!(&b.reason, Reason::Invalid(m) if m.contains("through a router"))), "{blockers:?}");
}

#[test]
fn a_router_cycle_and_a_huge_fan_out_stay_bounded() {
    // Hostile data the graph wouldn't normally hold: resolution must still terminate quickly.
    let p = project(vec![text(1, "a"), router(10), router(11), relay(20, "test.list", None), wire(50, 1, 10, 0), wire(51, 10, 11, 0), wire(52, 11, 20, 0)]);
    let canvas = p.canvases().get(&C).unwrap();
    let mut raw: BTreeMap<NodeId, Vec<Feed<'_>>> = BTreeMap::new();
    raw.insert(id(10), vec![("in", 0, id(11), "out")]);
    raw.insert(id(11), vec![("in", 0, id(10), "out")]);
    let mut out = Vec::new();
    expand(canvas, &raw, id(10), "out", &mut BTreeSet::new(), &mut out);
    assert!(out.is_empty());
    // 70 noodles from router 10 into router 11, and 70 from 11 into the consumer: 4,900 paths,
    // capped just past the list limit.
    let mut changes = vec![text(1, "a"), router(10), router(11), relay(20, "test.list", None), wire(1, 1, 10, 0)];
    for k in 0..70u8 {
        changes.push(Change::Connect {
            canvas: C,
            edge: Edge {
                id: EdgeId::from_bytes([k, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 9]),
                from: Endpoint { node: id(10), port: "out".into() },
                to: Endpoint { node: id(11), port: "in".into() },
                order: u32::from(k),
            },
        });
        changes.push(Change::Connect {
            canvas: C,
            edge: Edge {
                id: EdgeId::from_bytes([k, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 9]),
                from: Endpoint { node: id(11), port: "out".into() },
                to: Endpoint { node: id(20), port: "in".into() },
                order: u32::from(k),
            },
        });
    }
    let p = project(changes);
    let canvas = p.canvases().get(&C).unwrap();
    assert_eq!(incoming(canvas).get(&id(20)).unwrap().len(), MAX_FEEDS_PER_PORT);
    let Planned::Blocked(_) = plan(canvas, &kinds(), &RunMode::Selected, &ids(&[20]), &BTreeSet::new()).unwrap() else { panic!("over the list limit") };
}

#[test]
fn a_change_behind_a_router_makes_the_consumer_stale() {
    let kinds = kinds();
    let mut p = project(vec![text(1, "a"), router(10), relay(3, "test.single", None), wire(50, 1, 10, 0), wire(51, 10, 3, 0)]);
    let hash = crate::staleness::input_hash(p.canvases().get(&C).unwrap(), &kinds, id(3)).unwrap();
    let g = GenerationId::from_bytes([33; 16]);
    Changeset::new(vec![
        Change::AddGeneration {
            canvas: C,
            id: id(3),
            index: 0,
            generation: Generation { id: g, outputs: vec![Value::Text("r".into())], input_hash: Some(hash) },
        },
        Change::SetActiveGeneration { canvas: C, id: id(3), generation: Some(g) },
    ])
    .apply(&mut p)
    .unwrap();
    assert!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).is_empty());
    Change::SetParam { canvas: C, id: id(1), name: "text".into(), value: Some(Value::Text("b".into())) }.apply(&mut p).unwrap();
    assert_eq!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds), ids(&[3]));
}

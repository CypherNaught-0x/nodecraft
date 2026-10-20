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
    let resolved = incoming(canvas, &kinds()).unwrap();
    let feeds: Vec<NodeId> = resolved.feeds.get(&id(20)).unwrap().iter().map(|(_, _, from, _)| *from).collect();
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

/// A router node with an explicit id.
fn router_at(id_: NodeId) -> Change {
    Change::AddNode { canvas: C, node: kinds().get(router::ID).unwrap().instantiate_instance(id_, Default::default(), &Default::default()).unwrap() }
}
fn numbered(prefix: u8, k: u16) -> [u8; 16] {
    let mut b = [prefix; 16];
    b[..2].copy_from_slice(&k.to_le_bytes());
    b
}
fn link(e: [u8; 16], from: NodeId, to: NodeId, order: u32) -> Change {
    Change::Connect {
        canvas: C,
        edge: Edge { id: EdgeId::from_bytes(e), from: Endpoint { node: from, port: "out".into() }, to: Endpoint { node: to, port: "in".into() }, order },
    }
}

#[test]
fn a_lattice_of_empty_routers_resolves_in_linear_time() {
    // R0 has no inputs; each of R1..R40 is fed twice by the previous router: 2^40 paths, all
    // ending at nothing. Resolution must not walk them.
    let r = |k: u16| NodeId::from_bytes(numbered(7, k));
    let mut changes = vec![relay(20, "test.list", None), router_at(r(0))];
    for k in 1..=40u16 {
        changes.push(router_at(r(k)));
        changes.push(link(numbered(8, k), r(k - 1), r(k), 0));
        changes.push(link(numbered(9, k), r(k - 1), r(k), 1));
    }
    changes.push(link(numbered(10, 0), r(40), id(20), 0));
    let p = project(changes);
    let canvas = p.canvases().get(&C).unwrap();
    let start = std::time::Instant::now();
    assert!(incoming(canvas, &kinds()).unwrap().feeds.get(&id(20)).unwrap().is_empty());
    // The consumer is wired, but only to routers with nothing behind them: blocked, not run as
    // if unconnected.
    let Planned::Blocked(blockers) = plan(canvas, &kinds(), &RunMode::Selected, &ids(&[20]), &BTreeSet::new()).unwrap() else { panic!() };
    assert!(blockers.iter().any(|b| matches!(&b.reason, Reason::Invalid(m) if m.contains("router with no inputs"))), "{blockers:?}");
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn router_fan_out_is_capped_and_a_list_cannot_reach_a_single_input() {
    // 70 noodles from router 10 into router 11, and 70 from 11 into a list consumer: 4,900
    // paths, capped just past the list limit, so the planner reports the oversize.
    let mut changes = vec![text(1, "a"), router(10), router(11), relay(20, "test.list", None), wire(1, 1, 10, 0)];
    for k in 0..70u16 {
        changes.push(link(numbered(11, k), id(10), id(11), u32::from(k)));
        changes.push(link(numbered(12, k), id(11), id(20), u32::from(k)));
    }
    let p = project(changes);
    let canvas = p.canvases().get(&C).unwrap();
    assert_eq!(incoming(canvas, &kinds()).unwrap().feeds.get(&id(20)).unwrap().len(), MAX_FEEDS_PER_PORT);
    let Planned::Blocked(_) = plan(canvas, &kinds(), &RunMode::Selected, &ids(&[20]), &BTreeSet::new()).unwrap() else { panic!("over the list limit") };

    // A list output (from a list-output relay with a result) through a router into a single
    // input: the graph allows each edge, the planner refuses the pair.
    let mut kinds = kinds();
    kinds
        .register(Box::new(Relay(NodeDescriptor {
            id: "test.split".into(),
            label: "Split".into(),
            category: Category::Generation,
            inputs: vec![],
            outputs: vec![Port { name: "out".into(), port_type: LIST }],
            params: ParamSchema::default(),
            quick_add: None,
        })))
        .unwrap();
    let mut split = kinds.get("test.split").unwrap().descriptor().instantiate(id(5), &Default::default()).unwrap();
    let g = GenerationId::from_bytes([5; 16]);
    split.history.push(Generation::new(g, vec![Value::List(vec![Value::Text("x".into())])]));
    split.active = Some(g);
    let list_router = kinds.get(router::ID).unwrap().instantiate_instance(id(10), Default::default(), &Default::default()).unwrap();
    let p = project(vec![
        Change::AddNode { canvas: C, node: split },
        Change::AddNode { canvas: C, node: list_router },
        relay(3, "test.single", None),
        wire(50, 5, 10, 0),
        wire(51, 10, 3, 0),
    ]);
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Blocked(blockers) = plan(canvas, &kinds, &RunMode::Selected, &ids(&[3]), &BTreeSet::new()).unwrap() else { panic!() };
    assert!(blockers.iter().any(|b| matches!(&b.reason, Reason::Invalid(m) if m.contains("a list reaches it"))), "{blockers:?}");
    // When the list-producing upstream is itself scheduled, the deferred Step input must be
    // rejected before any jobs can run, just like an already materialized list result.
    let Planned::Blocked(blockers) = plan(canvas, &kinds, &RunMode::Selected, &ids(&[5, 3]), &BTreeSet::new()).unwrap() else { panic!() };
    assert!(blockers.iter().any(|b| b.node == id(3) && matches!(&b.reason, Reason::Invalid(m) if m.contains("a list reaches it"))));
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
            generation: Generation {
                id: g,
                outputs: vec![Value::Text("r".into())],
                input_hash: Some(hash),
                items: Vec::new(),
                settings: Vec::new(),
                incomplete: None,
            },
        },
        Change::SetActiveGeneration { canvas: C, id: id(3), generation: Some(g) },
    ])
    .apply(&mut p)
    .unwrap();
    assert!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).unwrap().is_empty());
    Change::SetParam { canvas: C, id: id(1), name: "text".into(), value: Some(Value::Text("b".into())) }.apply(&mut p).unwrap();
    assert_eq!(crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds).unwrap(), ids(&[3]));
}

#[test]
fn a_long_router_chain_resolves_without_deep_recursion() {
    // source → 3,000 routers in a row → consumer.
    const N: u16 = 3000;
    let router_id = |k: u16| {
        let mut b = [7u8; 16];
        b[..2].copy_from_slice(&k.to_le_bytes());
        NodeId::from_bytes(b)
    };
    let mut changes = vec![text(1, "a"), relay(20, "test.single", None)];
    for k in 0..N {
        changes.push(Change::AddNode {
            canvas: C,
            node: kinds().get(router::ID).unwrap().instantiate_instance(router_id(k), Default::default(), &Default::default()).unwrap(),
        });
    }
    let chain_link = |e: u16, from: NodeId, to: NodeId| {
        let mut b = [8u8; 16];
        b[..2].copy_from_slice(&e.to_le_bytes());
        Change::Connect {
            canvas: C,
            edge: Edge { id: EdgeId::from_bytes(b), from: Endpoint { node: from, port: "out".into() }, to: Endpoint { node: to, port: "in".into() }, order: 0 },
        }
    };
    changes.push(chain_link(60000, id(1), router_id(0)));
    for k in 1..N {
        changes.push(chain_link(k, router_id(k - 1), router_id(k)));
    }
    changes.push(chain_link(60001, router_id(N - 1), id(20)));
    let p = project(changes);
    let canvas = p.canvases().get(&C).unwrap();
    let feeds: Vec<NodeId> = incoming(canvas, &kinds()).unwrap().feeds.get(&id(20)).unwrap().iter().map(|(_, _, from, _)| *from).collect();
    assert_eq!(feeds, vec![id(1)]);

    // 1,400 consumers on the end of that chain: each router is resolved once, so this is linear.
    let mut q = p.clone();
    let consumers: Vec<Change> = (0..1400u16)
        .flat_map(|c| {
            let node = kinds().get("test.single").unwrap().descriptor().instantiate(NodeId::from_bytes(numbered(9, c)), &Default::default()).unwrap();
            let id_c = node.id;
            [Change::AddNode { canvas: C, node }, chain_link(c.wrapping_add(10000), router_id(N - 1), id_c)]
        })
        .collect();
    Changeset::new(consumers).apply(&mut q).unwrap();
    let canvas = q.canvases().get(&C).unwrap();
    let start = std::time::Instant::now();
    assert_eq!(incoming(canvas, &kinds()).unwrap().feeds.len(), 1401);
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn resolution_past_the_entry_budget_is_refused() {
    // Each router doubles its predecessor (two noodles from it plus the source), so router 13
    // resolves to the 4,097-entry cap; 1,000 list consumers reading it would build ~4.1M entries.
    let r = |k: u16| NodeId::from_bytes(numbered(7, k));
    let mut changes = vec![text(1, "a"), router_at(r(0)), link(numbered(8, 0), id(1), r(0), 0)];
    for k in 1..=13u16 {
        changes.push(router_at(r(k)));
        changes.push(link(numbered(8, k), r(k - 1), r(k), 0));
        changes.push(link(numbered(9, k), r(k - 1), r(k), 1));
    }
    for c in 0..1000u16 {
        let node = kinds().get("test.list").unwrap().descriptor().instantiate(NodeId::from_bytes(numbered(11, c)), &Default::default()).unwrap();
        let consumer = node.id;
        changes.push(Change::AddNode { canvas: C, node });
        changes.push(link(numbered(12, c), r(13), consumer, 0));
    }
    let p = project(changes);
    let canvas = p.canvases().get(&C).unwrap();
    assert_eq!(incoming(canvas, &kinds()).unwrap_err().kind, nodecraft_types::ErrorKind::InvalidInput);
    assert!(plan(canvas, &kinds(), &RunMode::Selected, &[NodeId::from_bytes(numbered(11, 0))].into_iter().collect(), &BTreeSet::new()).is_err());
    assert!(crate::staleness::stale(canvas, &kinds()).is_err());
}

#[test]
fn invalid_router_instances_block_consumers_and_never_produce_hashes() {
    for defect in 0..5 {
        let mut router_node = kinds().get(router::ID).unwrap().instantiate_instance(id(10), Default::default(), &Default::default()).unwrap();
        match defect {
            0 => {
                router_node.config = router::config(Modality::Image);
            }
            1 => {
                router_node.config.insert(router::MODALITY.into(), Value::Bool(true));
            }
            2 => {
                router_node.inputs.push(Port { name: "unexpected".into(), port_type: LIST });
            }
            3 => {
                router_node.inputs.first_mut().unwrap().port_type.many = false;
            }
            _ => {
                router_node.outputs.push(Port { name: "unexpected".into(), port_type: TEXT });
            }
        }
        let p = project(vec![
            text(1, "a"),
            Change::AddNode { canvas: C, node: router_node },
            router(11),
            relay(20, "test.list", None),
            relay(21, "test.list", None),
            relay(22, "test.list", None),
            wire(50, 1, 10, 0),
            wire(51, 10, 11, 0),
            wire(52, 11, 20, 0),
            wire(53, 1, 20, 1),
            wire(54, 11, 21, 0),
            wire(55, 1, 22, 0),
        ]);
        let mut p = p;
        let generation = GenerationId::from_bytes([99; 16]);
        Changeset::new(vec![
            Change::AddGeneration {
                canvas: C,
                id: id(20),
                index: 0,
                generation: Generation {
                    id: generation,
                    outputs: vec![Value::Text("old".into())],
                    input_hash: Some(nodecraft_types::Blake3::from_bytes([7; 32])),
                    items: Vec::new(),
                    settings: Vec::new(),
                    incomplete: None,
                },
            },
            Change::SetActiveGeneration { canvas: C, id: id(20), generation: Some(generation) },
        ])
        .apply(&mut p)
        .unwrap();
        let canvas = p.canvases().get(&C).unwrap();
        let kinds = kinds();
        let resolved = incoming(canvas, &kinds).unwrap();
        assert_eq!(resolved.invalid.get(&(id(20), "in")), Some(&id(10)));
        assert_eq!(resolved.invalid.get(&(id(21), "in")), Some(&id(10)));
        assert_eq!(resolved.wired.len(), canvas.edges().values().map(|e| (e.to.node, e.to.port.as_str())).collect::<BTreeSet<_>>().len());
        for consumer in [20, 21] {
            let Planned::Blocked(blockers) = plan(canvas, &kinds, &RunMode::Selected, &ids(&[consumer]), &BTreeSet::new()).unwrap() else { panic!() };
            assert!(
                blockers
                    .iter()
                    .any(|b| b.upstream == Some(id(10)) && b.port.as_deref() == Some("in") && matches!(&b.reason, Reason::Invalid(m) if m.contains("router")))
            );
            assert!(crate::staleness::input_hash(canvas, &kinds, id(consumer)).is_err());
        }
        assert!(matches!(plan(canvas, &kinds, &RunMode::Selected, &ids(&[22]), &BTreeSet::new()).unwrap(), Planned::Ready(_)));
        // An invalid input recipe is unknown; this successful query is not evidence of freshness.
        assert!(crate::staleness::stale(canvas, &kinds).unwrap().is_empty());
    }
}

#[test]
fn absent_router_kind_is_not_a_passthrough_and_router_only_upstream_stays_empty() {
    let p = project(vec![text(1, "a"), router(10), relay(20, "test.list", None), wire(50, 1, 10, 0), wire(51, 10, 20, 0)]);
    let canvas = p.canvases().get(&C).unwrap();
    let mut registry = Registry::default();
    registry.register(Box::new(Relay(kinds().get("test.list").unwrap().descriptor().clone()))).unwrap();
    let Planned::Blocked(blockers) = plan(canvas, &registry, &RunMode::Selected, &ids(&[20]), &BTreeSet::new()).unwrap() else { panic!() };
    assert!(blockers.iter().any(|b| b.upstream == Some(id(10)) && matches!(&b.reason, Reason::Invalid(m) if m.contains("not installed"))));
    assert!(crate::staleness::input_hash(canvas, &registry, id(20)).is_err());
    assert!(plan(canvas, &kinds(), &RunMode::WithUpstream, &ids(&[10]), &BTreeSet::new()).is_err());
}

#[test]
fn direct_and_routed_inputs_hash_identically_and_duplicate_feeds_survive() {
    let routed = project(vec![text(1, "a"), router(10), relay(20, "test.list", None), wire(50, 1, 10, 0), wire(51, 1, 10, 1), wire(52, 10, 20, 0)]);
    let direct = project(vec![text(1, "a"), relay(20, "test.list", None), wire(50, 1, 20, 0), wire(51, 1, 20, 1)]);
    let routed = routed.canvases().get(&C).unwrap();
    let direct = direct.canvases().get(&C).unwrap();
    assert_eq!(crate::staleness::input_hash(routed, &kinds(), id(20)).unwrap(), crate::staleness::input_hash(direct, &kinds(), id(20)).unwrap());
    assert_eq!(incoming(routed, &kinds()).unwrap().feeds.get(&id(20)).unwrap().len(), 2);
}

#[test]
fn invalid_router_tails_survive_feed_truncation_at_routers_and_consumers() {
    for through_router in [false, true] {
        let mut bad = kinds().get(router::ID).unwrap().instantiate_instance(id(11), Default::default(), &Default::default()).unwrap();
        bad.config.insert(router::MODALITY.into(), Value::Bool(true));
        let mut changes = vec![text(1, "a"), router(10), Change::AddNode { canvas: C, node: bad }, relay(20, "test.list", None)];
        // Valid feeds fill the entire resolved-port allowance before the invalid router is
        // encountered. Neither the router memo cap nor the consumer cap may hide its failure.
        let target = if through_router { id(10) } else { id(20) };
        for order in 0..MAX_FEEDS_PER_PORT {
            changes.push(link(numbered(30, u16::try_from(order).unwrap()), id(1), target, u32::try_from(order).unwrap()));
        }
        changes.push(link(numbered(31, 0), id(11), target, u32::try_from(MAX_FEEDS_PER_PORT).unwrap()));
        if through_router {
            changes.push(link(numbered(31, 1), id(10), id(20), 0));
        }
        let p = project(changes);
        let canvas = p.canvases().get(&C).unwrap();
        let kinds = kinds();
        let resolved = incoming(canvas, &kinds).unwrap();
        assert_eq!(resolved.feeds.get(&id(20)).unwrap().len(), MAX_FEEDS_PER_PORT);
        assert_eq!(resolved.invalid.get(&(id(20), "in")), Some(&id(11)));
        let Planned::Blocked(blockers) = plan(canvas, &kinds, &RunMode::Selected, &ids(&[20]), &BTreeSet::new()).unwrap() else { panic!() };
        assert!(blockers.iter().any(|b| b.node == id(20)
            && b.port.as_deref() == Some("in")
            && b.upstream == Some(id(11))
            && matches!(&b.reason, Reason::Invalid(message) if message.contains("router"))));
        assert!(crate::staleness::input_hash(canvas, &kinds, id(20)).is_err());
    }
}

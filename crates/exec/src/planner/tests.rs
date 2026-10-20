use super::plan as plan_run;
use super::*;
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Generation, Node, Port, Position, Project};
use nodecraft_nodes::{NodeDescriptor, NodeKind};
use nodecraft_types::{CanvasId, EdgeId, GenerationId, Modality, ParamSchema, ProjectId};
use proptest::prelude::*;

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const TEXT: PortType = PortType { modality: Modality::Text, many: false };
const LIST: PortType = PortType { modality: Modality::Text, many: true };

/// A generative stand-in: a list text input, one text output. It "plans" a source job echoing
/// its inputs so tests can see what the planner handed it.
struct Relay(NodeDescriptor);
impl NodeKind for Relay {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        // Needs at least one input, like an image-to-image model.
        (!inputs.is_empty()).then(|| ModeId::new("relay")).and_then(Result::ok)
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let value = Value::List(ctx.inputs.iter().map(|input| input.value.clone()).collect());
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value }])
    }
}

/// A relay whose list input runs once per batch item (#515), like a generation's prompt.
struct Fanning(Relay);
impl NodeKind for Fanning {
    fn descriptor(&self) -> &NodeDescriptor {
        self.0.descriptor()
    }
    fn contract(&self, descriptor: &NodeDescriptor) -> nodecraft_nodes::NodeContract {
        let mut contract = nodecraft_nodes::NodeContract::derive(descriptor);
        for input in &mut contract.inputs {
            input.fans_out = true;
        }
        contract
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        self.0.mode(inputs)
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        self.0.plan(ctx)
    }
}

fn kinds() -> Registry {
    let mut kinds = Registry::builtins().unwrap();
    let relay = NodeDescriptor {
        id: "test.relay".into(),
        label: "Relay".into(),
        category: Category::Generation,
        inputs: vec![Port { name: "in".into(), port_type: LIST }],
        outputs: vec![Port { name: "out".into(), port_type: TEXT }],
        params: ParamSchema { params: vec![] },
        quick_add: None,
    };
    kinds.register(Box::new(Relay(relay.clone()))).unwrap();
    kinds.register(Box::new(Fanning(Relay(NodeDescriptor { id: "test.fanning".into(), label: "Fanning".into(), ..relay.clone() })))).unwrap();
    // A relay with a required prompt, like a generator.
    let prompt = nodecraft_types::ParamSpec {
        name: "prompt".into(),
        label: "Prompt".into(),
        description: None,
        control: nodecraft_types::ParamControl::Text { multiline: false, max_bytes: 1024 },
        required: true,
        default: None,
        visible_if: None,
    };
    let prompted = NodeDescriptor { id: "test.prompted".into(), label: "Prompted".into(), params: ParamSchema { params: vec![prompt] }, ..relay.clone() };
    kinds.register(Box::new(Relay(prompted))).unwrap();
    // One single input, one list output.
    let split = NodeDescriptor {
        id: "test.split".into(),
        label: "Split".into(),
        inputs: vec![Port { name: "in".into(), port_type: TEXT }],
        outputs: vec![Port { name: "items".into(), port_type: LIST }],
        ..relay
    };
    kinds.register(Box::new(Relay(split.clone()))).unwrap();
    // One single input, one single output: what a batch fans out.
    let single =
        NodeDescriptor { id: "test.single".into(), label: "Single".into(), outputs: vec![Port { name: "out".into(), port_type: TEXT }], ..split.clone() };
    kinds.register(Box::new(Relay(single))).unwrap();
    // Two single inputs, to pair two batches.
    let pair = NodeDescriptor {
        id: "test.pair".into(),
        label: "Pair".into(),
        inputs: vec![Port { name: "a".into(), port_type: TEXT }, Port { name: "b".into(), port_type: TEXT }],
        outputs: vec![Port { name: "out".into(), port_type: TEXT }],
        ..split
    };
    kinds.register(Box::new(Relay(pair))).unwrap();
    kinds
}
fn of_kind(n: u8, kind: &str, result: Option<Value>) -> Node {
    let mut node = kinds().get(kind).unwrap().descriptor().instantiate(id(n), &BTreeMap::new()).unwrap();
    if let Some(result) = result {
        let generation = GenerationId::from_bytes([n; 16]);
        node.history.push(Generation { id: generation, outputs: vec![result], input_hash: None, items: Vec::new(), settings: Vec::new(), incomplete: None });
        node.active = Some(generation);
    }
    node
}
fn wire(e: u16, from: (u8, &str), to: (u8, &str), order: u32) -> Change {
    let mut bytes = [0u8; 16];
    bytes[..2].copy_from_slice(&e.to_le_bytes());
    bytes[15] = 0xee;
    Change::Connect {
        canvas: C,
        edge: Edge {
            id: EdgeId::from_bytes(bytes),
            from: Endpoint { node: id(from.0), port: from.1.into() },
            to: Endpoint { node: id(to.0), port: to.1.into() },
            order,
        },
    }
}
fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
fn ids(ns: &[u8]) -> BTreeSet<NodeId> {
    ns.iter().map(|n| id(*n)).collect()
}
fn text(n: u8, content: &str) -> Node {
    let mut values = BTreeMap::new();
    values.insert("text".to_owned(), Value::Text(content.into()));
    let mut node = kinds().get("source.text").unwrap().descriptor().instantiate(id(n), &values).unwrap();
    node.pos = Position { x: f32::from(n) * 400.0, y: 0.0 };
    node
}
fn relay(n: u8, result: Option<&str>) -> Node {
    let mut node = kinds().get("test.relay").unwrap().descriptor().instantiate(id(n), &BTreeMap::new()).unwrap();
    // Message tests use a named node; descriptors now leave automatic titles empty (#57).
    node.title = "Relay".into();
    if let Some(result) = result {
        let generation = GenerationId::from_bytes([n; 16]);
        node.history.push(Generation {
            id: generation,
            outputs: vec![Value::Text(result.into())],
            input_hash: None,
            items: Vec::new(),
            settings: Vec::new(),
            incomplete: None,
        });
        node.active = Some(generation);
    }
    node
}
fn project(nodes: Vec<Node>, links: &[(u8, u8)]) -> Project {
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Plan").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for node in nodes {
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    for (e, (from, to)) in links.iter().enumerate() {
        let edge = Edge {
            id: EdgeId::from_bytes([u8::try_from(e).unwrap().wrapping_add(100); 16]),
            from: Endpoint { node: id(*from), port: "out".into() },
            to: Endpoint { node: id(*to), port: "in".into() },
            order: u32::try_from(e).unwrap(),
        };
        Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    }
    project
}
fn ready(planned: Planned) -> RunPlan {
    match planned {
        Planned::Ready(plan) => plan,
        Planned::Blocked(blockers) => panic!("blocked: {blockers:?}"),
    }
}
fn order(plan: &RunPlan) -> Vec<NodeId> {
    plan.steps.iter().map(|s| s.node).collect()
}

/// 1 (text) → 2 (relay, has result) → 3 (relay, no result) → 4 (relay, no result)
fn chain() -> Project {
    project(vec![text(1, "dawn"), relay(2, Some("forest")), relay(3, None), relay(4, None)], &[(1, 2), (2, 3), (3, 4)])
}

#[test]
fn run_selected_uses_upstream_results_and_plans_jobs_now() {
    let p = chain();
    let canvas = p.canvases().get(&C).unwrap();
    let plan = ready(plan_run(canvas, &kinds(), &RunMode::Selected, &ids(&[3]), &BTreeSet::new()).unwrap());
    assert_eq!(order(&plan), vec![id(3)]);
    let step = plan.steps.first().unwrap();
    assert_eq!(step.inputs.first().unwrap().from, vec![InputSource::Value(Value::Text("forest".into()))]);
    // Every input is known, so the jobs exist already.
    assert_eq!(
        step.jobs,
        Some(vec![JobSpec::Source { node: id(3), output: "out".into(), value: Value::List(vec![Value::List(vec![Value::Text("forest".into())])]) }])
    );
}

#[test]
fn blocker_messages_fall_back_to_kind_for_automatic_titles() {
    let automatic = kinds().get("test.relay").unwrap().descriptor().instantiate(id(1), &BTreeMap::new()).unwrap();
    assert!(automatic.title.is_empty(), "automatic titles remain view-derived");
    let mut unknown = automatic.clone();
    unknown.id = id(2);
    unknown.kind = "plugin.gone".into();
    let p = project(vec![automatic, unknown], &[]);
    let canvas = p.canvases().get(&C).unwrap();
    let missing_result = Blocker { node: id(1), port: Some("in".into()), upstream: Some(id(2)), reason: Reason::NoResult };
    assert_eq!(missing_result.message(canvas), "Can't run test.relay yet: its in input needs a result from plugin.gone.");
    let missing_kind = Blocker { node: id(2), port: None, upstream: None, reason: Reason::UnknownKind };
    assert_eq!(missing_kind.message(canvas), "Can't run plugin.gone: its node kind isn't available.");
}

#[test]
fn run_selected_cant_run_yet_names_the_missing_input() {
    let p = chain();
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Blocked(blockers) = plan_run(canvas, &kinds(), &RunMode::Selected, &ids(&[4]), &BTreeSet::new()).unwrap() else { panic!() };
    assert_eq!(blockers, vec![Blocker { node: id(4), port: Some("in".into()), upstream: Some(id(3)), reason: Reason::NoResult }]);
    let message = blockers.first().unwrap().message(canvas);
    assert_eq!(message, "Can't run Relay yet: its in input needs a result from Relay.");
    // Selecting both runs them in order, the later step fed by the earlier one.
    let plan = ready(plan_run(canvas, &kinds(), &RunMode::Selected, &ids(&[3, 4]), &BTreeSet::new()).unwrap());
    assert_eq!(order(&plan), vec![id(3), id(4)]);
    let last = plan.steps.last().unwrap();
    assert_eq!(last.after, vec![id(3)]);
    assert_eq!(last.inputs.first().unwrap().from, vec![InputSource::Step { node: id(3), port: "out".into(), list: false }]);
    assert_eq!(last.jobs, None);
    assert_eq!(plan.edges, vec![(id(3), id(4))]);
    assert!(last.jobs.is_none());
}

#[test]
fn run_with_upstream_adds_empty_and_stale_ancestors_only() {
    let p = chain();
    let canvas = p.canvases().get(&C).unwrap();
    let kinds = kinds();
    let plan = ready(plan_run(canvas, &kinds, &RunMode::WithUpstream, &ids(&[4]), &BTreeSet::new()).unwrap());
    // 3 has no result; 2 has a fresh one and is used as is; the text source is resolved inline.
    assert_eq!(order(&plan), vec![id(3), id(4)]);
    let plan = ready(plan_run(canvas, &kinds, &RunMode::WithUpstream, &ids(&[4]), &ids(&[2])).unwrap());
    assert_eq!(order(&plan), vec![id(2), id(3), id(4)]);
    let first = plan.steps.first().unwrap();
    assert_eq!(first.inputs.first().unwrap().from, vec![InputSource::Value(Value::Text("dawn".into()))]);
}

#[test]
fn run_field_and_all_stale_choose_their_own_nodes() {
    let mut nodes = vec![text(1, "dawn"), relay(2, Some("forest")), relay(3, None), relay(4, None)];
    let field = FieldId::from_bytes([9; 16]);
    for node in nodes.iter_mut().filter(|n| n.id == id(3) || n.id == id(4)) {
        node.field = Some(field);
    }
    let mut p = project(Vec::new(), &[]);
    let mut changes = vec![Change::PutField { canvas: C, field: nodecraft_graph::Field::new(field, "Field") }];
    changes.extend(nodes.into_iter().map(|node| Change::AddNode { canvas: C, node }));
    nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
    for (e, (from, to)) in [(1u8, 2u8), (2, 3), (3, 4)].into_iter().enumerate() {
        let edge = Edge {
            id: EdgeId::from_bytes([e as u8 + 100; 16]),
            from: Endpoint { node: id(from), port: "out".into() },
            to: Endpoint { node: id(to), port: "in".into() },
            order: 0,
        };
        Change::Connect { canvas: C, edge }.apply(&mut p).unwrap();
    }
    let canvas = p.canvases().get(&C).unwrap();
    let plan = ready(plan_run(canvas, &kinds(), &RunMode::Field(field), &BTreeSet::new(), &BTreeSet::new()).unwrap());
    assert_eq!(order(&plan), vec![id(3), id(4)]);
    // Stale 2 and 4 run, and so does empty 3, which 4 needs; unknown stale ids are ignored.
    let plan = ready(plan_run(canvas, &kinds(), &RunMode::AllStale, &BTreeSet::new(), &ids(&[2, 4, 77])).unwrap());
    assert_eq!(order(&plan), vec![id(2), id(3), id(4)]);
}

#[test]
fn malformed_requests_are_errors_and_bad_nodes_block() {
    let p = chain();
    let canvas = p.canvases().get(&C).unwrap();
    let kinds = kinds();
    assert_eq!(plan_run(canvas, &kinds, &RunMode::Selected, &ids(&[99]), &BTreeSet::new()).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(plan_run(canvas, &kinds, &RunMode::Selected, &BTreeSet::new(), &BTreeSet::new()).unwrap_err().kind, ErrorKind::InvalidInput);
    // Nothing stale is a normal state: an empty plan, not an error.
    let plan = ready(plan_run(canvas, &kinds, &RunMode::AllStale, &BTreeSet::new(), &BTreeSet::new()).unwrap());
    assert!(plan.steps.is_empty());
    // A relay with nothing connected has no mode.
    let p = project(vec![relay(5, None)], &[]);
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Blocked(blockers) = plan_run(canvas, &kinds, &RunMode::Selected, &ids(&[5]), &BTreeSet::new()).unwrap() else { panic!() };
    assert_eq!(blockers.first().map(|b| &b.reason), Some(&Reason::UnsupportedInputs));
    // An unknown kind, and an upstream source whose required content is missing.
    let mut unknown = relay(6, None);
    unknown.kind = "gone.kind".into();
    let image = kinds.get("source.image").unwrap().descriptor().instantiate(id(7), &BTreeMap::new()).unwrap();
    let p = project(vec![unknown, image], &[]);
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Blocked(blockers) = plan_run(canvas, &kinds, &RunMode::Selected, &ids(&[6, 7]), &BTreeSet::new()).unwrap() else { panic!() };
    assert_eq!(blockers.len(), 2);
    assert_eq!(blockers.first().map(|b| &b.reason), Some(&Reason::UnknownKind));
    assert!(matches!(blockers.get(1).map(|b| &b.reason), Some(Reason::Invalid(_))));
}

#[test]
fn list_inputs_keep_noodle_order() {
    let p = project(vec![text(1, "a"), text(2, "b"), relay(3, None)], &[(2, 3), (1, 3)]);
    let canvas = p.canvases().get(&C).unwrap();
    let plan = ready(plan_run(canvas, &kinds(), &RunMode::Selected, &ids(&[3]), &BTreeSet::new()).unwrap());
    // Edge (2 → 3) has order 0 and (1 → 3) order 1.
    let from = &plan.steps.first().unwrap().inputs.first().unwrap().from;
    assert_eq!(from, &vec![InputSource::Value(Value::Text("b".into())), InputSource::Value(Value::Text("a".into()))]);
}

/// A random DAG over up to 10 nodes: each is a text source or a relay (with or without a
/// result), and relays take noodles from earlier nodes only.
fn arb_graph() -> impl Strategy<Value = (Project, BTreeSet<NodeId>, BTreeSet<NodeId>)> {
    (1usize..10)
        .prop_flat_map(|n| {
            (
                proptest::collection::vec((any::<bool>(), any::<bool>()), n),
                proptest::collection::vec((0..n, 0..n), 0..n * 2),
                proptest::collection::btree_set(0..n, 0..n),
                proptest::collection::btree_set(0..n, 1..=n),
            )
        })
        .prop_map(|(shape, links, stale, requested)| {
            let nodes: Vec<Node> = shape
                .iter()
                .enumerate()
                .map(|(i, (is_source, has_result))| {
                    let n = u8::try_from(i + 1).unwrap();
                    if *is_source { text(n, "x") } else { relay(n, has_result.then_some("r")) }
                })
                .collect();
            let is_relay = |i: usize| shape.get(i).is_some_and(|(s, _)| !s);
            let mut seen = BTreeSet::new();
            let links: Vec<(u8, u8)> = links
                .into_iter()
                .filter(|(a, b)| a < b && is_relay(*b) && seen.insert((*a, *b)))
                .map(|(a, b)| (u8::try_from(a + 1).unwrap(), u8::try_from(b + 1).unwrap()))
                .collect();
            let to_ids = |set: BTreeSet<usize>| set.into_iter().map(|i| id(u8::try_from(i + 1).unwrap())).collect::<BTreeSet<_>>();
            (project(nodes, &links), to_ids(stale), to_ids(requested))
        })
}

proptest! {
    #[test]
    fn plans_are_deterministic_ordered_and_complete((p, stale, requested) in arb_graph()) {
        let canvas = p.canvases().get(&C).unwrap();
        let kinds = kinds();
        for mode in [RunMode::Selected, RunMode::WithUpstream] {
            let first = plan_run(canvas, &kinds, &mode, &requested, &stale).unwrap();
            prop_assert_eq!(&first, &plan_run(canvas, &kinds, &mode, &requested, &stale).unwrap());
            let Planned::Ready(plan) = first else { continue };
            let position: BTreeMap<NodeId, usize> = plan.steps.iter().enumerate().map(|(i, s)| (s.node, i)).collect();
            // Every requested node runs, every step's dependencies run before it, and every
            // edge between two steps is a dependency.
            prop_assert!(requested.iter().all(|r| position.contains_key(r)));
            for step in &plan.steps {
                prop_assert!(step.after.iter().all(|a| position.contains_key(a) && position.get(a) < position.get(&step.node)));
            }
            for edge in canvas.edges().values() {
                // Sources feed their content inline even when they run themselves.
                let source = canvas.nodes().get(&edge.from.node).is_some_and(|n| n.kind == "source.text");
                if !source && position.contains_key(&edge.from.node) && position.contains_key(&edge.to.node) {
                    prop_assert!(plan.edges.contains(&(edge.from.node, edge.to.node)));
                }
            }
            match mode {
                RunMode::Selected => prop_assert_eq!(position.keys().copied().collect::<BTreeSet<_>>(), requested.clone()),
                _ => {
                    // Extra steps are relays that were stale or empty.
                    for step in plan.steps.iter().filter(|s| !requested.contains(&s.node)) {
                        let node = canvas.nodes().get(&step.node).unwrap();
                        prop_assert!(stale.contains(&step.node) || active_outputs(node).is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn with_upstream_never_waits_for_a_missing_result((p, stale, requested) in arb_graph()) {
        let canvas = p.canvases().get(&C).unwrap();
        let kinds = kinds();
        // Relays need an input; skip graphs where a requested-or-pulled relay has none.
        if let Planned::Blocked(blockers) = plan_run(canvas, &kinds, &RunMode::WithUpstream, &requested, &stale).unwrap() {
            prop_assert!(blockers.iter().all(|b| b.reason == Reason::UnsupportedInputs), "{:?}", blockers);
        }
        // Selected is blocked exactly when some requested relay is fed by an empty relay
        // outside the request (or has no input at all).
        let blocked = matches!(plan_run(canvas, &kinds, &RunMode::Selected, &requested, &stale).unwrap(), Planned::Blocked(_));
        let expected = requested.iter().any(|r| {
            let node = canvas.nodes().get(r).unwrap();
            let feeds: Vec<_> = canvas.edges().values().filter(|e| e.to.node == *r).collect();
            node.kind == "test.relay"
                && (feeds.is_empty()
                    || feeds.iter().any(|e| !requested.contains(&e.from.node) && canvas.nodes().get(&e.from.node).is_some_and(|u| u.kind == "test.relay" && active_outputs(u).is_none())))
        });
        prop_assert_eq!(blocked, expected);
    }
}

#[test]
fn a_missing_required_parameter_blocks_before_anything_runs() {
    // 1 (text) → 2 (relay, empty) → 3 (prompted, empty prompt): nothing may run, not even 2.
    let p = project(vec![text(1, "a"), relay(2, None), of_kind(3, "test.prompted", None)], &[(1, 2), (2, 3)]);
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Blocked(blockers) = plan_run(canvas, &kinds(), &RunMode::WithUpstream, &ids(&[3]), &BTreeSet::new()).unwrap() else { panic!() };
    assert_eq!(blockers.len(), 1);
    assert!(matches!(blockers.first(), Some(Blocker { node, reason: Reason::Invalid(_), .. }) if *node == id(3)));
}

#[test]
fn list_outputs_splice_into_list_inputs_and_single_inputs_pass_one_value() {
    let items = Value::List(vec![Value::Text("a".into()), Value::Text("b".into())]);
    let mut p = project(vec![text(1, "x"), of_kind(2, "test.split", Some(items)), text(3, "c"), relay(4, None), of_kind(5, "test.split", None)], &[]);
    let changes = vec![wire(1, (2, "items"), (4, "in"), 0), wire(2, (3, "out"), (4, "in"), 1), wire(3, (1, "out"), (5, "in"), 0)];
    nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let kinds = kinds();
    let plan = ready(plan_run(canvas, &kinds, &RunMode::Selected, &ids(&[4, 5]), &BTreeSet::new()).unwrap());
    let step = |n: u8| plan.steps.iter().find(|s| s.node == id(n)).unwrap();
    let text = |t: &str| InputSource::Value(Value::Text(t.into()));
    assert_eq!(step(4).inputs.first().unwrap().from, vec![text("a"), text("b"), text("c")]);
    // The single input of 5 reaches `plan` as one value, not a list.
    let jobs = step(5).jobs.clone().unwrap();
    assert_eq!(jobs, vec![JobSpec::Source { node: id(5), output: "out".into(), value: Value::List(vec![Value::Text("x".into())]) }]);
}

#[test]
fn unknown_upstream_kinds_still_lend_their_results() {
    let mut gone = relay(2, Some("kept"));
    gone.kind = "plugin.gone".into();
    let mut empty = relay(4, None);
    empty.kind = "plugin.gone".into();
    let p = project(vec![text(1, "a"), gone, relay(3, None), empty, relay(5, None)], &[(1, 2), (2, 3), (4, 5)]);
    let canvas = p.canvases().get(&C).unwrap();
    let kinds = kinds();
    let plan = ready(plan_run(canvas, &kinds, &RunMode::Selected, &ids(&[3]), &BTreeSet::new()).unwrap());
    assert_eq!(plan.steps.first().unwrap().inputs.first().unwrap().from, vec![InputSource::Value(Value::Text("kept".into()))]);
    // With no result, with-upstream pulls the unknown node in, and it blocks as itself.
    let Planned::Blocked(blockers) = plan_run(canvas, &kinds, &RunMode::WithUpstream, &ids(&[5]), &BTreeSet::new()).unwrap() else { panic!() };
    assert_eq!(blockers, vec![Blocker { node: id(4), port: None, upstream: None, reason: Reason::UnknownKind }]);
    assert_eq!(blockers.first().unwrap().message(canvas), "Can't run Relay: its node kind isn't available.");
    let named = Blocker { node: id(5), port: Some("in".into()), upstream: Some(id(4)), reason: Reason::UnknownKind };
    assert_eq!(named.message(canvas), "Can't run Relay: the node kind of Relay isn't available.");
}

#[test]
fn repeated_noodles_report_one_blocker_and_fan_in_is_capped() {
    let mut p = project(vec![relay(1, None), relay(2, None)], &[]);
    let changes = (0..3u16).map(|e| wire(e, (1, "out"), (2, "in"), u32::from(e))).collect();
    nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Blocked(blockers) = plan_run(canvas, &kinds(), &RunMode::Selected, &ids(&[2]), &BTreeSet::new()).unwrap() else { panic!() };
    assert_eq!(blockers.len(), 1);

    // More noodles than a list may hold.
    let mut p = project(vec![text(1, "a"), relay(2, None)], &[]);
    let count = u16::try_from(MAX_LIST_ITEMS + 1).unwrap();
    let changes = (0..count).map(|e| wire(e, (1, "out"), (2, "in"), u32::from(e))).collect();
    nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Blocked(blockers) = plan_run(canvas, &kinds(), &RunMode::Selected, &ids(&[2]), &BTreeSet::new()).unwrap() else { panic!() };
    assert!(matches!(blockers.as_slice(), [Blocker { reason: Reason::Invalid(m), .. }] if m.contains("items")));

    // A large value wired in many times hits the byte budget long before memory runs out.
    let big = "x".repeat(1024 * 1024);
    let mut p = project(vec![text(1, &big), relay(2, None)], &[]);
    let count = u16::try_from(MAX_STEP_INPUT_BYTES / big.len() + 1).unwrap();
    let changes = (0..count).map(|e| wire(e, (1, "out"), (2, "in"), u32::from(e))).collect();
    nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Blocked(blockers) = plan_run(canvas, &kinds(), &RunMode::Selected, &ids(&[2]), &BTreeSet::new()).unwrap() else { panic!() };
    assert!(matches!(blockers.as_slice(), [Blocker { reason: Reason::Invalid(m), .. }] if m.contains("too large")));
}

#[test]
fn a_deep_chain_plans_iteratively() {
    const N: u16 = 2000;
    let mut p = project(vec![], &[]);
    let node = |n: u16| {
        let mut bytes = [0u8; 16];
        bytes[..2].copy_from_slice(&n.to_le_bytes());
        let mut node = relay(1, None);
        node.id = NodeId::from_bytes(bytes);
        node
    };
    let mut changes: Vec<Change> = (0..N).map(|n| Change::AddNode { canvas: C, node: node(n) }).collect();
    changes.push(Change::AddNode { canvas: C, node: text(200, "seed") });
    changes.push(Change::Connect {
        canvas: C,
        edge: Edge {
            id: EdgeId::from_bytes([1; 16]),
            from: Endpoint { node: id(200), port: "out".into() },
            to: Endpoint { node: node(0).id, port: "in".into() },
            order: 0,
        },
    });
    for n in 1..N {
        let mut bytes = [0u8; 16];
        bytes[..2].copy_from_slice(&n.to_le_bytes());
        bytes[15] = 0xaa;
        changes.push(Change::Connect {
            canvas: C,
            edge: Edge {
                id: EdgeId::from_bytes(bytes),
                from: Endpoint { node: node(n - 1).id, port: "out".into() },
                to: Endpoint { node: node(n).id, port: "in".into() },
                order: 0,
            },
        });
    }
    nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let plan = ready(plan_run(canvas, &kinds(), &RunMode::WithUpstream, &[node(N - 1).id].into_iter().collect(), &BTreeSet::new()).unwrap());
    assert_eq!(plan.steps.len(), usize::from(N));
    assert_eq!(plan.steps.first().map(|s| s.node), Some(node(0).id));
    assert_eq!(plan.steps.iter().filter(|s| s.jobs.is_none()).count(), usize::from(N) - 1);
}

proptest! {
    #[test]
    fn all_stale_runs_every_stale_relay_with_what_it_needs((p, stale, _requested) in arb_graph()) {
        let canvas = p.canvases().get(&C).unwrap();
        match plan_run(canvas, &kinds(), &RunMode::AllStale, &BTreeSet::new(), &stale).unwrap() {
            Planned::Ready(plan) => {
                let steps: BTreeSet<NodeId> = plan.steps.iter().map(|s| s.node).collect();
                prop_assert!(stale.iter().all(|s| steps.contains(s)));
                for step in &plan.steps {
                    prop_assert!(step.after.iter().all(|a| steps.contains(a)));
                }
            }
            Planned::Blocked(blockers) => prop_assert!(blockers.iter().all(|b| b.reason == Reason::UnsupportedInputs), "{:?}", blockers),
        }
    }
}

#[test]
fn the_whole_plan_has_a_byte_budget() {
    // Many steps, each well under the per-step budget, sharing one large source.
    let big = "x".repeat(1024 * 1024);
    let per_step = 4u16;
    let steps = u8::try_from(MAX_PLAN_INPUT_BYTES / (big.len() * usize::from(per_step)) + 1).unwrap_or(u8::MAX);
    let mut nodes = vec![text(1, &big)];
    nodes.extend((2..steps.saturating_add(2)).map(|n| relay(n, None)));
    let mut p = project(nodes, &[]);
    let mut e = 0u16;
    let mut changes = Vec::new();
    for n in 2..steps.saturating_add(2) {
        for order in 0..per_step {
            changes.push(wire(e, (1, "out"), (n, "in"), u32::from(order)));
            e += 1;
        }
    }
    nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let all: BTreeSet<NodeId> = (2..steps.saturating_add(2)).map(id).collect();
    let Planned::Blocked(blockers) = plan_run(canvas, &kinds(), &RunMode::Selected, &all, &BTreeSet::new()).unwrap() else { panic!() };
    assert!(matches!(blockers.as_slice(), [Blocker { reason: Reason::Invalid(m), .. }] if m.contains("this run")));
}

#[test]
fn nested_list_fan_out_is_charged_before_it_is_copied() {
    // A stored list output holding one list of 4,093 empty texts (within the document's item
    // limit), wired 4,096 times into a list input: tiny as text, ~16.7M values once expanded.
    let inner = Value::List(vec![Value::Text(String::new()); 4093]);
    let mut p = project(vec![of_kind(1, "test.split", Some(Value::List(vec![inner]))), relay(2, None)], &[]);
    let changes = (0..4096u16).map(|e| wire(e, (1, "items"), (2, "in"), u32::from(e))).collect();
    nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let start = std::time::Instant::now();
    let Planned::Blocked(blockers) = plan_run(canvas, &kinds(), &RunMode::Selected, &ids(&[2]), &BTreeSet::new()).unwrap() else { panic!("must be blocked") };
    assert!(matches!(blockers.as_slice(), [Blocker { reason: Reason::Invalid(m), .. }] if m.contains("too large")), "{blockers:?}");
    // Refused before materialising the expansion (which would take seconds and ~0.5 GB).
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn fan_in_from_another_step_has_the_same_item_cap() {
    // 1 (text) → 2 (relay) ⇉ 3 (relay) over more noodles than a list may hold; both relays run.
    let mut p = project(vec![text(1, "a"), relay(2, None), relay(3, None)], &[]);
    let mut changes = vec![wire(60000, (1, "out"), (2, "in"), 0)];
    let count = u16::try_from(MAX_LIST_ITEMS + 1).unwrap();
    changes.extend((0..count).map(|e| wire(e, (2, "out"), (3, "in"), u32::from(e))));
    nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Blocked(blockers) = plan_run(canvas, &kinds(), &RunMode::Selected, &ids(&[2, 3]), &BTreeSet::new()).unwrap() else {
        panic!("must be blocked")
    };
    assert!(blockers.iter().any(|b| b.node == id(3) && matches!(&b.reason, Reason::Invalid(m) if m.contains("items"))), "{blockers:?}");
}

#[test]
fn an_unusable_active_result_is_rerun_not_waited_on() {
    // Relay 2's active generation recorded no outputs (a different port layout): with-upstream
    // schedules 2 again instead of blocking 3 on a result it can't use.
    let mut stale_layout = relay(2, None);
    let generation = GenerationId::from_bytes([2; 16]);
    stale_layout.history.push(Generation { id: generation, outputs: vec![], input_hash: None, items: Vec::new(), settings: Vec::new(), incomplete: None });
    stale_layout.active = Some(generation);
    let p = project(vec![text(1, "a"), stale_layout, relay(3, None)], &[(1, 2), (2, 3)]);
    let canvas = p.canvases().get(&C).unwrap();
    let plan = ready(plan_run(canvas, &kinds(), &RunMode::WithUpstream, &ids(&[3]), &BTreeSet::new()).unwrap());
    assert_eq!(order(&plan), vec![id(2), id(3)]);
}

// Batches (#92).

fn batch(n: u8, items: &str, pairing: &str) -> Node {
    let mut node = kinds().get(nodecraft_nodes::batch::ID).unwrap().instantiate_instance(id(n), Default::default(), &Default::default()).unwrap();
    node.params.insert("text".into(), Value::Text(items.into()));
    node.params.insert("pairing".into(), Value::Text(pairing.into()));
    node
}
fn wired(nodes: Vec<Node>, links: &[(u8, &str, u8, &str)]) -> Project {
    let mut p = project(nodes, &[]);
    // Single inputs take order 0; list inputs keep noodle order.
    let mut orders: BTreeMap<(u8, &str), u32> = BTreeMap::new();
    let changes = links
        .iter()
        .enumerate()
        .map(|(e, (from, out, to, port))| {
            let order = orders.entry((*to, port)).or_insert(0);
            let change = wire(u16::try_from(e).unwrap(), (*from, out), (*to, port), *order);
            *order += 1;
            change
        })
        .collect();
    nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
    p
}
fn plan_of(p: &Project, nodes: &[u8]) -> Planned {
    plan_run(p.canvases().get(&C).unwrap(), &kinds(), &RunMode::Selected, &ids(nodes), &BTreeSet::new()).unwrap()
}
fn blocked(planned: Planned) -> String {
    match planned {
        Planned::Blocked(blockers) => match blockers.as_slice() {
            [Blocker { reason: Reason::Invalid(message), .. }] => message.clone(),
            other => panic!("unexpected blockers: {other:?}"),
        },
        Planned::Ready(plan) => panic!("planned: {plan:?}"),
    }
}
fn single(n: u8) -> Node {
    of_kind(n, "test.single", None)
}
fn fanned_values(step: &Step) -> Vec<Vec<Value>> {
    step.fanout
        .as_ref()
        .unwrap()
        .cells
        .iter()
        .map(|cell| {
            cell.iter()
                .map(|input| match input.from.as_slice() {
                    [InputSource::Value(value)] => value.clone(),
                    other => panic!("unexpected sources {other:?}"),
                })
                .collect()
        })
        .collect()
}
fn texts(values: &[&str]) -> Vec<Value> {
    values.iter().map(|v| Value::Text((*v).into())).collect()
}

#[test]
fn a_batch_fans_a_single_input_out_once_per_item() {
    let p = wired(vec![batch(1, "fox\n  \nowl\nfox", "cross"), single(2)], &[(1, "item", 2, "in")]);
    let plan = ready(plan_of(&p, &[2]));
    assert_eq!(order(&plan), vec![id(2)], "the batch never runs");
    let step = plan.steps.first().unwrap();
    assert!(step.inputs.is_empty());
    assert_eq!(fanned_values(step), vec![texts(&["fox"]), texts(&["owl"]), texts(&["fox"])], "blank lines are skipped, repeats kept");
    let fanout = step.fanout.as_ref().unwrap();
    assert_eq!(fanout.jobs, vec![1, 1, 1]);
    assert_eq!(step.jobs.as_ref().unwrap().len(), 3);
    // Every item has its own id, repeats included, and ids are stable across plans.
    let tags: BTreeSet<_> = fanout.items.iter().collect();
    assert_eq!(tags.len(), 3);
    assert_eq!(ready(plan_of(&p, &[2])).steps.first().unwrap().fanout.as_ref().unwrap().items, fanout.items);

    // Running the batch on its own is an error: it has nothing to run.
    assert!(plan_run(p.canvases().get(&C).unwrap(), &kinds(), &RunMode::Selected, &ids(&[1]), &BTreeSet::new()).is_err());
}

#[test]
fn a_list_input_collects_every_item() {
    let p = wired(vec![batch(1, "a\nb", "cross"), relay(2, None)], &[(1, "item", 2, "in")]);
    let plan = ready(plan_of(&p, &[2]));
    let step = plan.steps.first().unwrap();
    assert!(step.fanout.is_none());
    assert_eq!(step.inputs.first().unwrap().from, vec![InputSource::Value(Value::Text("a".into())), InputSource::Value(Value::Text("b".into()))]);
}

#[test]
fn a_fanning_list_input_runs_once_per_item_with_its_other_wires() {
    // 3 (text) and 1 (batch) both feed 2's list input, in that order: each item takes the
    // batch's place, after the text.
    let p = wired(vec![batch(1, "a\nb", "cross"), of_kind(2, "test.fanning", None), text(3, "intro")], &[(3, "out", 2, "in"), (1, "item", 2, "in")]);
    let plan = ready(plan_of(&p, &[2]));
    let step = plan.steps.first().unwrap();
    let fanout = step.fanout.as_ref().unwrap();
    assert_eq!(fanout.jobs, vec![1, 1]);
    let sources: Vec<_> = fanout.cells.iter().map(|cell| cell.first().unwrap().from.clone()).collect();
    let value = |t: &str| InputSource::Value(Value::Text(t.into()));
    assert_eq!(sources, vec![vec![value("intro"), value("a")], vec![value("intro"), value("b")]]);

    // A result the batch fans out in this run fans it too, item by item.
    let p = wired(vec![batch(1, "a\nb", "cross"), single(4), of_kind(2, "test.fanning", None)], &[(1, "item", 4, "in"), (4, "out", 2, "in")]);
    let plan = ready(plan_of(&p, &[4, 2]));
    let last = plan.steps.iter().find(|s| s.node == id(2)).unwrap();
    let cells = &last.fanout.as_ref().unwrap().cells;
    for (index, cell) in cells.iter().enumerate() {
        assert_eq!(cell.first().unwrap().from, vec![InputSource::Item { node: id(4), port: "out".into(), index }]);
    }
    assert_eq!(cells.len(), 2);
}

#[test]
fn two_batches_pair_by_cross_or_zip() {
    let pair = |a: &str, b: &str, pa: &str, pb: &str| {
        let pair = of_kind(3, "test.pair", None);
        wired(vec![batch(1, a, pa), batch(2, b, pb), pair], &[(1, "item", 3, "a"), (2, "item", 3, "b")])
    };
    let plan = ready(plan_of(&pair("a\nb", "x\ny\nz", "cross", "cross"), &[3]));
    let values = fanned_values(plan.steps.first().unwrap());
    assert_eq!(values.len(), 6);
    assert_eq!(values.first().unwrap(), &texts(&["a", "x"]));
    assert_eq!(values.last().unwrap(), &texts(&["b", "z"]));

    let plan = ready(plan_of(&pair("a\nb", "x\ny", "zip", "zip"), &[3]));
    assert_eq!(fanned_values(plan.steps.first().unwrap()), vec![texts(&["a", "x"]), texts(&["b", "y"])]);
    // Each zipped item names both batches.
    assert!(plan.steps.first().unwrap().fanout.as_ref().unwrap().items.iter().all(|tag| tag.len() == 2));

    // Never a silent change of size: unequal zips and mixed settings are errors.
    let message = blocked(plan_of(&pair("a\nb", "x\ny\nz", "zip", "zip"), &[3]));
    assert!(message.contains("2 and 3 items"), "{message}");
    let message = blocked(plan_of(&pair("a\nb", "x\ny", "zip", "cross"), &[3]));
    assert!(message.contains("disagree"), "{message}");
}

#[test]
fn one_batch_reaching_a_node_twice_joins_on_its_items() {
    // 1 (batch) → 2, 3 (singles) → 4 (pair): one run per item, not every combination.
    let p = wired(
        vec![batch(1, "a\nb\nc", "cross"), single(2), single(3), of_kind(4, "test.pair", None)],
        &[(1, "item", 2, "in"), (1, "item", 3, "in"), (2, "out", 4, "a"), (3, "out", 4, "b")],
    );
    let plan = ready(plan_of(&p, &[2, 3, 4]));
    let last = plan.steps.iter().find(|s| s.node == id(4)).unwrap();
    let fanout = last.fanout.as_ref().unwrap();
    assert_eq!(fanout.items.len(), 3);
    assert!(last.jobs.is_none(), "its inputs come from this run");
    for (index, cell) in fanout.cells.iter().enumerate() {
        let sources: Vec<_> = cell.iter().flat_map(|input| input.from.clone()).collect();
        assert_eq!(sources, vec![InputSource::Item { node: id(2), port: "out".into(), index }, InputSource::Item { node: id(3), port: "out".into(), index }]);
    }
}

#[test]
fn batch_plans_that_cant_run_say_why() {
    let message = blocked(plan_of(&wired(vec![batch(1, "  \n", "cross"), single(2)], &[(1, "item", 2, "in")]), &[2]));
    assert!(message.contains("no items"), "{message}");

    let message =
        blocked(plan_of(&wired(vec![batch(1, "a", "cross"), batch(2, "b", "cross"), single(3)], &[(1, "item", 2, "in"), (2, "item", 3, "in")]), &[3]));
    assert!(message.contains("another batch"), "{message}");

    let message = blocked(plan_of(
        &wired(vec![text(4, "a"), single(1), batch(2, "b", "cross"), single(3)], &[(4, "out", 1, "in"), (1, "out", 2, "in"), (2, "item", 3, "in")]),
        &[1, 3],
    ));
    assert!(message.contains("run the nodes feeding it first"), "{message}");

    // The fan-out is bounded before anything is built.
    let many: String = (0..nodecraft_nodes::batch::MAX_ITEMS).map(|n| format!("{n}\n")).collect();
    let pair = of_kind(3, "test.pair", None);
    let message = blocked(plan_of(&wired(vec![batch(1, &many, "cross"), batch(2, "x\ny", "cross"), pair], &[(1, "item", 3, "a"), (2, "item", 3, "b")]), &[3]));
    assert!(message.contains("more than"), "{message}");

    let message = blocked(plan_of(&wired(vec![batch(1, "a", "bogus"), single(2)], &[(1, "item", 2, "in")]), &[2]));
    assert!(message.contains("Pairing"), "{message}");
}

#[test]
fn an_existing_fanned_result_fans_its_consumers_out() {
    let p = wired(vec![batch(1, "a\nb", "cross"), single(2), single(3)], &[(1, "item", 2, "in"), (2, "out", 3, "in")]);
    let plan = ready(plan_of(&p, &[2]));
    let items = plan.steps.first().unwrap().fanout.as_ref().unwrap().items.clone();
    // Record a fanned result on 2, as a run would.
    let canvas = p.canvases().get(&C).unwrap();
    let mut node = canvas.nodes().get(&id(2)).unwrap().clone();
    let generation = GenerationId::from_bytes([2; 16]);
    node.history.push(Generation {
        id: generation,
        outputs: vec![Value::List(texts(&["A", "B"]))],
        input_hash: None,
        items: items.clone(),
        settings: Vec::new(),
        incomplete: None,
    });
    node.active = Some(generation);
    let p = wired(vec![batch(1, "a\nb", "cross"), node, single(3)], &[(1, "item", 2, "in"), (2, "out", 3, "in")]);
    let plan = ready(plan_of(&p, &[3]));
    let step = plan.steps.first().unwrap();
    assert_eq!(fanned_values(step), vec![texts(&["A"]), texts(&["B"])]);
    assert_eq!(step.fanout.as_ref().unwrap().items, items, "consumers keep their source items");
}

#[test]
fn a_fanned_step_is_quoted_per_item_even_when_its_jobs_come_later() {
    let p = wired(vec![batch(1, "a\nb\nc", "cross"), single(2), single(3)], &[(1, "item", 2, "in"), (2, "out", 3, "in")]);
    let plan = ready(plan_of(&p, &[2, 3]));
    let catalog = nodecraft_catalog::Catalog::builtin().unwrap();
    let quote = crate::admission::quote(&plan, p.canvases().get(&C).unwrap(), &kinds(), &catalog).unwrap();
    let jobs = |n: u8| quote.steps.iter().find(|s| s.node == id(n)).map(|s| (s.jobs, s.jobs_known)).unwrap();
    assert_eq!(jobs(2), (3, true));
    assert_eq!(jobs(3), (3, false));
}

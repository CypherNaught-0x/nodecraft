use super::*;
use crate::planner::{Planned, RunMode, RunPlan, plan_expanded};
use crate::run::{Run, RunLimits, RunOptions, StepFailure, StepResult};
use crate::scheduler::RetryPolicy;
use nodecraft_graph::{Change, Changeset, Port, Project, Technique, TechniqueParam, TechniquePort};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
use nodecraft_types::{ErrorKind, Modality, ParamControl, ParamSchema, ParamSpec, PortType, ProjectId, TechniqueId, Value};

const TEXT: PortType = PortType { modality: Modality::Text, many: false };
const LIST: PortType = PortType { modality: Modality::Text, many: true };
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const V: CanvasId = CanvasId::from_bytes([2; 16]);
const W: CanvasId = CanvasId::from_bytes([3; 16]);
const SOURCE: NodeId = NodeId::from_bytes([10; 16]);
const T: NodeId = NodeId::from_bytes([11; 16]);
const BRIEF: NodeId = NodeId::from_bytes([20; 16]);
const SHOUT: NodeId = NodeId::from_bytes([21; 16]);

fn text_of(value: &Value) -> String {
    match value {
        Value::Text(text) => text.clone(),
        Value::List(items) => items.iter().map(text_of).collect::<Vec<_>>().join("+"),
        other => format!("{other:?}"),
    }
}

/// Text in, its letters upper-cased with `suffix` after, one job per output value; `letters`
/// gives them one by one.
struct Shout(NodeDescriptor);
impl NodeKind for Shout {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("shout").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let text = ctx.inputs.first().map(|input| text_of(&input.value)).unwrap_or_default();
        let suffix = ctx.node.params.get("suffix").map(text_of).unwrap_or_default();
        let mut jobs = vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: Value::Text(format!("{}{suffix}", text.to_uppercase())) }];
        jobs.extend(text.chars().map(|c| JobSpec::Source { node: ctx.node.id, output: "letters".into(), value: Value::Text(c.to_string()) }));
        Ok(jobs)
    }
}

fn kinds() -> Registry {
    let mut kinds = Registry::builtins().unwrap();
    let suffix = ParamSpec {
        name: "suffix".into(),
        label: "Suffix".into(),
        description: None,
        control: ParamControl::Text { multiline: false, max_bytes: 64 },
        required: false,
        default: Some(Value::Text(String::new())),
        visible_if: None,
    };
    let shout = NodeDescriptor {
        id: "test.shout".into(),
        label: "Shout".into(),
        category: Category::Generation,
        inputs: vec![Port { name: "in".into(), port_type: TEXT }],
        outputs: vec![Port { name: "out".into(), port_type: TEXT }, Port { name: "letters".into(), port_type: LIST }],
        params: ParamSchema { params: vec![suffix] },
        quick_add: None,
    };
    kinds.register(Box::new(Shout(shout))).unwrap();
    kinds
}

fn node(kinds: &Registry, id: NodeId, kind: &str, text: &str) -> Node {
    let mut node = kinds.get(kind).unwrap().instantiate_instance(id, Default::default(), &Default::default()).unwrap();
    if kind == "source.text" {
        node.params.insert("text".into(), Value::Text(text.into()));
    }
    node
}

fn edge(n: u8, from: (NodeId, &str), to: (NodeId, &str), order: u32) -> Edge {
    Edge { id: EdgeId::from_bytes([n; 16]), from: Endpoint { node: from.0, port: from.1.into() }, to: Endpoint { node: to.0, port: to.1.into() }, order }
}

fn port(name: &str, node: NodeId, port: &str, port_type: PortType) -> TechniquePort {
    TechniquePort { name: name.into(), description: String::new(), node, port: port.into(), port_type }
}

/// Version 1 of "Shouter": Brief (example "cat") → Shout, giving its text and its letters, its
/// suffix exposed as "Ending".
fn version(kinds: &Registry) -> Canvas {
    let technique = Technique {
        id: TechniqueId::from_bytes([7; 16]),
        version: 1,
        name: "Shouter".into(),
        description: String::new(),
        category: String::new(),
        tags: Vec::new(),
        inputs: vec![port("Words", BRIEF, "out", TEXT)],
        outputs: vec![port("Shouted", SHOUT, "out", TEXT), port("Letters", SHOUT, "letters", LIST)],
        params: vec![TechniqueParam { name: "Ending".into(), description: String::new(), node: SHOUT, param: "suffix".into() }],
        source: None,
        retired: false,
    };
    let nodes = vec![node(kinds, BRIEF, "source.text", "cat"), node(kinds, SHOUT, "test.shout", "")];
    Canvas::technique(V, "Shouter v1", technique, nodes, vec![edge(30, (BRIEF, "out"), (SHOUT, "in"), 0)]).unwrap()
}

/// A project holding the version and a canvas with a technique node placed from it, fed by
/// a text source holding `words` when it is `Some`, its ending set to `ending`.
fn project(words: Option<&str>, ending: &str) -> (Project, Registry) {
    let kinds = kinds();
    let version = version(&kinds);
    let interface = technique::interface_of(&version, &kinds).unwrap();
    let mut placed = kinds.get(technique::ID).unwrap().instantiate_instance(T, interface.config().unwrap(), &Default::default()).unwrap();
    placed.params.insert("ending".into(), Value::Text(ending.into()));
    let mut project = Project::new(ProjectId::from_bytes([4; 16]), "Techniques").unwrap();
    let mut changes =
        vec![Change::AddCanvas { canvas: version }, Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }, Change::AddNode { canvas: C, node: placed }];
    if let Some(words) = words {
        changes.push(Change::AddNode { canvas: C, node: node(&kinds, SOURCE, "source.text", words) });
        changes.push(Change::Connect { canvas: C, edge: edge(40, (SOURCE, "out"), (T, "words"), 0) });
    }
    Changeset::new(changes).apply(&mut project).unwrap();
    (project, kinds)
}

fn planned(project: &Project, kinds: &Registry, requested: &[NodeId]) -> Result<Planned, Error> {
    let expanded = expand(project.canvases(), project.canvases().get(&C).unwrap(), kinds)?;
    plan_expanded(
        &expanded,
        kinds,
        &RunMode::Selected,
        &requested.iter().copied().collect(),
        &BTreeSet::new(),
        &nodecraft_types::EMPTY_TARGET_SELECTION,
        &nodecraft_types::NoOptions,
    )
}

fn ready(planned: Planned) -> RunPlan {
    match planned {
        Planned::Ready(plan) => plan,
        Planned::Blocked(blockers) => panic!("blocked: {blockers:?}"),
    }
}

/// Runs the plan on the inlined canvas as the host does, failing the jobs of `fail`.
fn drive(project: &Project, kinds: &Registry, plan: &RunPlan, fail: Option<NodeId>) -> (Vec<StepResult>, Vec<StepFailure>) {
    let expanded = expand(project.canvases(), project.canvases().get(&C).unwrap(), kinds).unwrap();
    let options = RunOptions { id: [9; 16], first_job: 1, limits: RunLimits::default(), retry: RetryPolicy::NEVER };
    let (mut run, first) = Run::start(plan, C, &expanded.canvas, kinds, options).unwrap();
    let (mut finished, mut failed, mut queue) = (first.finished, first.failed, first.jobs);
    while let Some(job) = queue.pop() {
        let Some(JobSpec::Source { node, output, value }) = run.work(job.id).cloned() else { panic!("only source jobs here") };
        let progress = if Some(node) == fail { run.job_failed(kinds, job.id, "it said no") } else { run.job_done(kinds, job.id, vec![(output, value)]) };
        queue.extend(progress.jobs);
        finished.extend(progress.finished);
        failed.extend(progress.failed);
    }
    assert!(run.is_finished());
    (finished, failed)
}

fn outputs(results: &[StepResult]) -> Vec<(NodeId, Vec<String>)> {
    results.iter().map(|result| (result.node, result.generation.outputs.iter().map(text_of).collect())).collect()
}

#[test]
fn a_technique_node_runs_its_version_on_its_inputs_and_parameters() {
    let (project, kinds) = project(Some("fox"), "!");
    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    assert_eq!(plan.steps.len(), 2, "the copied Shout, then the technique node");
    assert_eq!(plan.steps.last().map(|step| step.node), Some(T));
    assert_eq!(plan.inner.values().copied().collect::<Vec<_>>(), vec![T]);
    let (finished, failed) = drive(&project, &kinds, &plan, None);
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(outputs(&finished), vec![(T, vec!["FOX!".to_owned(), "f+o+x".to_owned()])], "only the technique node's result is recorded");
}

#[test]
fn an_unfed_input_uses_the_versions_own_content() {
    let (project, kinds) = project(None, "?");
    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    let (finished, _) = drive(&project, &kinds, &plan, None);
    assert_eq!(outputs(&finished), vec![(T, vec!["CAT?".to_owned(), "c+a+t".to_owned()])]);
}

#[test]
fn an_inner_failure_is_the_technique_nodes_reported_once() {
    let (project, kinds) = project(Some("fox"), "");
    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    let inner = plan.inner.keys().next().copied().unwrap();
    let (finished, failed) = drive(&project, &kinds, &plan, Some(inner));
    assert!(finished.is_empty());
    assert_eq!(failed, vec![StepFailure { node: T, reason: "“test.shout”: it said no".into() }]);
}

#[test]
fn inlined_nodes_cant_be_requested_and_the_real_canvas_is_untouched() {
    let (project, kinds) = project(Some("fox"), "");
    let canvas = project.canvases().get(&C).unwrap();
    let expanded = expand(project.canvases(), canvas, &kinds).unwrap();
    let inner = expanded.inner.keys().next().copied().unwrap();
    assert_eq!(expanded.owner(inner), T);
    assert_eq!(expanded.canvas.nodes().get(&T), canvas.nodes().get(&T), "the technique node is inlined as it is");
    assert_eq!(planned(&project, &kinds, &[inner]).unwrap_err().kind, ErrorKind::NotFound);
    assert!(matches!(expand(project.canvases(), project.canvases().get(&V).unwrap(), &kinds).unwrap().canvas, Cow::Borrowed(_)));
}

#[test]
fn a_missing_removed_or_changed_version_blocks_only_its_node() {
    let (mut project, kinds) = project(Some("fox"), "");
    let retired = project.canvases().get(&V).unwrap().retired_technique().unwrap();
    Changeset::new(vec![Change::RemoveCanvas { id: V }, Change::AddCanvas { canvas: retired }]).apply(&mut project).unwrap();
    let Planned::Blocked(blockers) = planned(&project, &kinds, &[T]).unwrap() else { panic!("a removed version can't run") };
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers.first().map(|b| b.node), Some(T));
    assert!(blockers.first().unwrap().message(project.canvases().get(&C).unwrap()).contains("upgrade"));
    // Nothing else on the canvas is held up.
    assert!(matches!(planned(&project, &kinds, &[SOURCE]).unwrap(), Planned::Ready(_)));

    Changeset::new(vec![Change::RemoveCanvas { id: V }]).apply(&mut project).unwrap();
    assert!(matches!(planned(&project, &kinds, &[T]).unwrap(), Planned::Blocked(_)));
}

#[test]
fn techniques_nest() {
    let (mut project, kinds) = project(Some("fox"), "");
    // Version 1 of "Twice": a Words input into a Shouter node whose output it gives back.
    let outer_brief = NodeId::from_bytes([50; 16]);
    let nested = NodeId::from_bytes([51; 16]);
    let interface = technique::interface_of(project.canvases().get(&V).unwrap(), &kinds).unwrap();
    let mut shouter = kinds.get(technique::ID).unwrap().instantiate_instance(nested, interface.config().unwrap(), &Default::default()).unwrap();
    shouter.params.insert("ending".into(), Value::Text("!!".into()));
    let twice = Technique {
        id: TechniqueId::from_bytes([8; 16]),
        version: 1,
        name: "Twice".into(),
        description: String::new(),
        category: String::new(),
        tags: Vec::new(),
        inputs: vec![port("Words", outer_brief, "out", TEXT)],
        outputs: vec![port("Result", nested, "shouted", TEXT)],
        params: Vec::new(),
        source: None,
        retired: false,
    };
    let nodes = vec![node(&kinds, outer_brief, "source.text", "dog"), shouter];
    let version = Canvas::technique(W, "Twice v1", twice, nodes, vec![edge(60, (outer_brief, "out"), (nested, "words"), 0)]).unwrap();
    let interface = technique::interface_of(&version, &kinds).unwrap();
    let placed =
        kinds.get(technique::ID).unwrap().instantiate_instance(NodeId::from_bytes([12; 16]), interface.config().unwrap(), &Default::default()).unwrap();
    let changes = vec![
        Change::AddCanvas { canvas: version },
        Change::AddNode { canvas: C, node: placed },
        Change::Connect { canvas: C, edge: edge(61, (SOURCE, "out"), (NodeId::from_bytes([12; 16]), "words"), 0) },
    ];
    Changeset::new(changes).apply(&mut project).unwrap();
    let plan = ready(planned(&project, &kinds, &[NodeId::from_bytes([12; 16])]).unwrap());
    assert_eq!(plan.steps.len(), 3, "Shout, the nested Shouter, then Twice");
    assert!(plan.inner.values().all(|owner| *owner == NodeId::from_bytes([12; 16])));
    let (finished, failed) = drive(&project, &kinds, &plan, None);
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(outputs(&finished), vec![(NodeId::from_bytes([12; 16]), vec!["FOX!!".to_owned()])]);
}

/// Gives `items` from nothing, or what its many input is fed.
struct Items(NodeDescriptor);
impl NodeKind for Items {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("items").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: Value::List(Vec::new()) }])
    }
}

#[test]
fn a_technique_whose_wires_multiply_past_the_cap_blocks_only_its_node() {
    let (mut project, mut kinds) = project(Some("fox"), "");
    let items = NodeDescriptor {
        id: "test.items".into(),
        label: "Items".into(),
        category: Category::Generation,
        inputs: vec![Port { name: "in".into(), port_type: LIST }],
        outputs: vec![Port { name: "out".into(), port_type: LIST }],
        params: ParamSchema { params: Vec::new() },
        quick_add: None,
    };
    kinds.register(Box::new(Items(items))).unwrap();
    let id = |tag: u8, n: usize| {
        let mut bytes = [tag; 16];
        bytes[..8].copy_from_slice(&(n as u64).to_le_bytes());
        bytes
    };
    // A version whose many input feeds 150 nodes, placed with 70 wires into that input: inlining
    // would add 150 × 70 wires.
    // Each of them feeds the one output.
    let (input, collect) = (NodeId::from_bytes([90; 16]), NodeId::from_bytes([95; 16]));
    let mut nodes = vec![node(&kinds, input, "test.items", ""), node(&kinds, collect, "test.items", "")];
    let mut edges = Vec::new();
    for n in 0..150 {
        let to = NodeId::from_bytes(id(91, n));
        nodes.push(node(&kinds, to, "test.items", ""));
        edges.push(Edge {
            id: EdgeId::from_bytes(id(92, n)),
            from: Endpoint { node: input, port: "out".into() },
            to: Endpoint { node: to, port: "in".into() },
            order: 0,
        });
        let order = u32::try_from(n).unwrap();
        edges.push(Edge {
            id: EdgeId::from_bytes(id(96, n)),
            from: Endpoint { node: to, port: "out".into() },
            to: Endpoint { node: collect, port: "in".into() },
            order,
        });
    }
    let wide = Technique {
        id: TechniqueId::from_bytes([9; 16]),
        version: 1,
        name: "Wide".into(),
        description: String::new(),
        category: String::new(),
        tags: Vec::new(),
        inputs: vec![port("Items", input, "out", LIST)],
        outputs: vec![port("All", collect, "out", LIST)],
        params: Vec::new(),
        source: None,
        retired: false,
    };
    let version = Canvas::technique(CanvasId::from_bytes([5; 16]), "Wide v1", wide, nodes, edges).unwrap();
    let interface = technique::interface_of(&version, &kinds).unwrap();
    let placed_id = NodeId::from_bytes([13; 16]);
    let placed = kinds.get(technique::ID).unwrap().instantiate_instance(placed_id, interface.config().unwrap(), &Default::default()).unwrap();
    let mut changes = vec![Change::AddCanvas { canvas: version }, Change::AddNode { canvas: C, node: placed }];
    for n in 0..70 {
        let from = NodeId::from_bytes(id(93, n));
        changes.push(Change::AddNode { canvas: C, node: node(&kinds, from, "source.text", "a") });
        let edge = Edge {
            id: EdgeId::from_bytes(id(94, n)),
            from: Endpoint { node: from, port: "out".into() },
            to: Endpoint { node: placed_id, port: "items".into() },
            order: u32::try_from(n).unwrap(),
        };
        changes.push(Change::Connect { canvas: C, edge });
    }
    Changeset::new(changes).apply(&mut project).unwrap();

    let Planned::Blocked(blockers) = planned(&project, &kinds, &[placed_id]).unwrap() else { panic!("10 500 wires can't be inlined") };
    assert_eq!(blockers.iter().map(|b| b.node).collect::<Vec<_>>(), vec![placed_id]);
    assert!(blockers.first().unwrap().message(project.canvases().get(&C).unwrap()).contains("too many wires"));
    // The other technique node on the canvas still runs.
    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    assert_eq!(plan.steps.last().map(|step| step.node), Some(T));
}

#[test]
fn a_cancelled_inner_job_reports_its_reason_as_it_is_under_its_owner() {
    let (project, kinds) = project(Some("fox"), "");
    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    let inner = plan.inner.keys().next().copied().unwrap();
    let expanded = expand(project.canvases(), project.canvases().get(&C).unwrap(), &kinds).unwrap();
    let options = RunOptions { id: [9; 16], first_job: 1, limits: RunLimits::default(), retry: RetryPolicy::NEVER };
    let (mut run, first) = Run::start(&plan, C, &expanded.canvas, &kinds, options).unwrap();
    assert_eq!((run.owner(inner), run.owner(T)), (T, T));
    let job = first.jobs.first().unwrap().id;
    assert_eq!(run.resume_recipe(&kinds, job), None, "an inlined step's result is never recorded on its own");
    let progress = run.job_cancelled(&kinds, job, "cancelled");
    assert_eq!(progress.failed, vec![StepFailure { node: T, reason: "cancelled".into() }], "the host still sees a cancellation");
}

const BATCH: NodeId = NodeId::from_bytes([14; 16]);

/// A batch on the canvas holding `items`, one per line.
fn batch_node(kinds: &Registry, id: NodeId, items: &str) -> Node {
    let mut batch = node(kinds, id, nodecraft_nodes::batch::ID, "");
    batch.params.insert("text".into(), Value::Text(items.into()));
    batch
}

#[test]
fn a_batch_into_a_technique_node_runs_its_version_once_per_item() {
    let (mut project, kinds) = project(None, "!");
    let changes = vec![
        Change::AddNode { canvas: C, node: batch_node(&kinds, BATCH, "fox\nowl") },
        Change::Connect { canvas: C, edge: edge(41, (BATCH, "item"), (T, "words"), 0) },
    ];
    Changeset::new(changes).apply(&mut project).unwrap();
    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    let step = plan.steps.last().unwrap();
    assert_eq!(step.node, T);
    assert_eq!(step.fanout.as_ref().map(|fanout| fanout.items.len()), Some(2), "{step:?}");
    let (finished, failed) = drive(&project, &kinds, &plan, None);
    assert!(failed.is_empty(), "{failed:?}");
    // One value per item, and a list output gives each item's list.
    assert_eq!(outputs(&finished), vec![(T, vec!["FOX!+OWL!".to_owned(), "f+o+x+o+w+l".to_owned()])]);
    let items = &finished.first().unwrap().generation.items;
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|tag| tag.len() == 1 && tag.first().is_some_and(|key| key.batch == BATCH)), "{items:?}");
}

#[test]
fn a_batch_its_version_collects_runs_the_technique_once() {
    let (mut project, mut kinds) = project(None, "");
    let items = NodeDescriptor {
        id: "test.items".into(),
        label: "Items".into(),
        category: Category::Generation,
        inputs: vec![Port { name: "in".into(), port_type: LIST }],
        outputs: vec![Port { name: "out".into(), port_type: LIST }],
        params: ParamSchema { params: Vec::new() },
        quick_add: None,
    };
    kinds.register(Box::new(Items(items))).unwrap();
    // "Gather": a Words input into one Items node, which takes every item in one run.
    let (brief, gather) = (NodeId::from_bytes([70; 16]), NodeId::from_bytes([71; 16]));
    let technique = Technique {
        id: TechniqueId::from_bytes([10; 16]),
        version: 1,
        name: "Gather".into(),
        description: String::new(),
        category: String::new(),
        tags: Vec::new(),
        inputs: vec![port("Words", brief, "out", TEXT)],
        outputs: vec![port("All", gather, "out", LIST)],
        params: Vec::new(),
        source: None,
        retired: false,
    };
    let nodes = vec![node(&kinds, brief, "source.text", "cat"), node(&kinds, gather, "test.items", "")];
    let version = Canvas::technique(CanvasId::from_bytes([6; 16]), "Gather v1", technique, nodes, vec![edge(72, (brief, "out"), (gather, "in"), 0)]).unwrap();
    let interface = technique::interface_of(&version, &kinds).unwrap();
    let placed_id = NodeId::from_bytes([15; 16]);
    let placed = kinds.get(technique::ID).unwrap().instantiate_instance(placed_id, interface.config().unwrap(), &Default::default()).unwrap();
    let changes = vec![
        Change::AddCanvas { canvas: version },
        Change::AddNode { canvas: C, node: placed },
        Change::AddNode { canvas: C, node: batch_node(&kinds, BATCH, "fox\nowl\nhare") },
        Change::Connect { canvas: C, edge: edge(73, (BATCH, "item"), (placed_id, "words"), 0) },
    ];
    Changeset::new(changes).apply(&mut project).unwrap();
    let plan = ready(planned(&project, &kinds, &[placed_id]).unwrap());
    assert_eq!(plan.steps.len(), 2, "{plan:?}");
    assert!(plan.steps.iter().all(|step| step.fanout.is_none()), "nothing fans: the version takes the items in one run: {plan:?}");
    let (finished, failed) = drive(&project, &kinds, &plan, None);
    assert!(failed.is_empty(), "{failed:?}");
    assert!(finished.first().is_some_and(|result| result.node == placed_id && result.generation.items.is_empty()), "{finished:?}");
}

#[test]
fn a_batch_inside_a_version_still_blocks_its_technique_node() {
    let (mut project, kinds) = project(Some("fox"), "");
    // "Split": a Words input through a batch of its own into a Shout.
    let (brief, inner_batch, shout) = (NodeId::from_bytes([80; 16]), NodeId::from_bytes([81; 16]), NodeId::from_bytes([82; 16]));
    let technique = Technique {
        id: TechniqueId::from_bytes([11; 16]),
        version: 1,
        name: "Split".into(),
        description: String::new(),
        category: String::new(),
        tags: Vec::new(),
        inputs: vec![port("Words", brief, "out", TEXT)],
        outputs: vec![port("Shouted", shout, "out", TEXT)],
        params: Vec::new(),
        source: None,
        retired: false,
    };
    let nodes = vec![node(&kinds, brief, "source.text", "cat"), batch_node(&kinds, inner_batch, "a\nb"), node(&kinds, shout, "test.shout", "")];
    let edges = vec![edge(83, (brief, "out"), (inner_batch, "in"), 0), edge(84, (inner_batch, "item"), (shout, "in"), 0)];
    let version = Canvas::technique(CanvasId::from_bytes([7; 16]), "Split v1", technique, nodes, edges).unwrap();
    let interface = technique::interface_of(&version, &kinds).unwrap();
    let placed_id = NodeId::from_bytes([16; 16]);
    let placed = kinds.get(technique::ID).unwrap().instantiate_instance(placed_id, interface.config().unwrap(), &Default::default()).unwrap();
    let changes = vec![
        Change::AddCanvas { canvas: version },
        Change::AddNode { canvas: C, node: placed },
        Change::Connect { canvas: C, edge: edge(85, (SOURCE, "out"), (placed_id, "words"), 0) },
    ];
    Changeset::new(changes).apply(&mut project).unwrap();
    let Planned::Blocked(blockers) = planned(&project, &kinds, &[placed_id]).unwrap() else { panic!("a batch inside a version can't fan it out (#93)") };
    assert_eq!(blockers.iter().map(|b| b.node).collect::<Vec<_>>(), vec![placed_id]);
    assert!(blockers.first().unwrap().message(project.canvases().get(&C).unwrap()).contains("batch inside"), "{blockers:?}");
}

/// Runs `plan` like [`drive`], failing the jobs whose text is `fail`; also returns the jobs run.
fn drive_failing(project: &Project, kinds: &Registry, plan: &RunPlan, fail: &str) -> (Vec<StepResult>, Vec<StepFailure>, Vec<JobSpec>) {
    let expanded = expand(project.canvases(), project.canvases().get(&C).unwrap(), kinds).unwrap();
    let options = RunOptions { id: [9; 16], first_job: 1, limits: RunLimits::default(), retry: RetryPolicy::NEVER };
    let (mut run, first) = Run::start(plan, C, &expanded.canvas, kinds, options).unwrap();
    let (mut finished, mut failed, mut queue, mut ran) = (first.finished, first.failed, first.jobs, Vec::new());
    while let Some(job) = queue.pop() {
        let Some(work) = run.work(job.id).cloned() else { panic!("no work for {job:?}") };
        let JobSpec::Source { output, value, .. } = work.clone() else { panic!("only source jobs here") };
        let progress =
            if value == Value::Text(fail.into()) { run.job_failed(kinds, job.id, "it said no") } else { run.job_done(kinds, job.id, vec![(output, value)]) };
        ran.push(work);
        queue.extend(progress.jobs);
        finished.extend(progress.finished);
        failed.extend(progress.failed);
    }
    assert!(run.is_finished());
    (finished, failed, ran)
}

/// An item failing inside a technique fanned out by a batch leaves the technique node a partial
/// result with the items that finished (#536); running it again runs only the missing item.
#[test]
fn a_failed_item_inside_a_fanned_technique_is_kept_out_and_the_only_one_run_again() {
    let (mut project, kinds) = project(None, "!");
    let changes = vec![
        Change::AddNode { canvas: C, node: batch_node(&kinds, BATCH, "fox\nowl") },
        Change::Connect { canvas: C, edge: edge(41, (BATCH, "item"), (T, "words"), 0) },
    ];
    Changeset::new(changes).apply(&mut project).unwrap();
    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    let (finished, failed, _) = drive_failing(&project, &kinds, &plan, "OWL!");
    let [partial] = finished.as_slice() else { panic!("only the technique node records: {finished:?}") };
    assert_eq!(partial.node, T);
    let fox = Value::List(["f", "o", "x"].map(|c| Value::Text(c.into())).to_vec());
    assert_eq!(
        partial.generation.outputs,
        vec![Value::List(vec![Value::Text("FOX!".into()), Value::List(Vec::new())]), Value::List(vec![fox, Value::List(Vec::new())])]
    );
    let missing = &partial.generation.incomplete.as_ref().unwrap().missing;
    assert_eq!(missing, &vec![nodecraft_graph::MissingItem { item: 1, why: nodecraft_graph::Missing::Failed }]);
    assert_eq!(partial.generation.items.len(), 2);
    let [failure] = failed.as_slice() else { panic!("reported once, on the technique node: {failed:?}") };
    assert_eq!(failure.node, T);
    assert!(failure.reason.starts_with("1 of 2 items didn't finish: “") && failure.reason.ends_with("”: it said no"), "names the inner node: {failure:?}");
    let node = project.canvases().get(&C).unwrap().nodes().get(&T).unwrap().clone();
    Changeset::new(crate::run::record_changes(C, &node, partial.generation.clone()).unwrap()).apply(&mut project).unwrap();

    // Again: the inlined Shout plans only the owl, and the fox is copied from the partial result.
    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    let shout = plan.steps.iter().find(|s| plan.inner.contains_key(&s.node) && s.kind == "test.shout").unwrap();
    assert_eq!(shout.fanout.as_ref().map(|f| f.jobs.clone()), Some(vec![0, 4]), "{shout:?}");
    let owner = plan.steps.iter().find(|s| s.node == T).unwrap();
    assert_eq!(owner.fanout.as_ref().and_then(|f| f.resume.as_ref()).and_then(|r| r.generation), Some(partial.generation.id));
    let (finished, failed, ran) = drive_failing(&project, &kinds, &plan, "");
    assert!(failed.is_empty(), "{failed:?}");
    assert!(!ran.iter().any(|job| matches!(job, JobSpec::Source { value, .. } if *value == Value::Text("FOX!".into()))), "the fox doesn't run again: {ran:?}");
    let [whole] = finished.as_slice() else { panic!("{finished:?}") };
    assert!(whole.generation.incomplete.is_none(), "{whole:?}");
    assert_eq!(outputs(&finished), vec![(T, vec!["FOX!+OWL!".to_owned(), "f+o+x+o+w+l".to_owned()])]);
}

/// When every item fails inside it, the technique node fails as a whole and records nothing.
#[test]
fn every_item_failing_inside_a_fanned_technique_fails_its_node() {
    let (mut project, kinds) = project(None, "!");
    let changes = vec![
        Change::AddNode { canvas: C, node: batch_node(&kinds, BATCH, "fox\nowl") },
        Change::Connect { canvas: C, edge: edge(41, (BATCH, "item"), (T, "words"), 0) },
    ];
    Changeset::new(changes).apply(&mut project).unwrap();
    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    let expanded = expand(project.canvases(), project.canvases().get(&C).unwrap(), &kinds).unwrap();
    let options = RunOptions { id: [9; 16], first_job: 1, limits: RunLimits::default(), retry: RetryPolicy::NEVER };
    let (mut run, first) = Run::start(&plan, C, &expanded.canvas, &kinds, options).unwrap();
    let (mut finished, mut failed, mut queue) = (first.finished, first.failed, first.jobs);
    while let Some(job) = queue.pop() {
        let progress = run.job_failed(&kinds, job.id, "it said no");
        queue.extend(progress.jobs);
        finished.extend(progress.finished);
        failed.extend(progress.failed);
    }
    assert!(run.is_finished());
    assert!(finished.is_empty(), "nothing finished, nothing is recorded: {finished:?}");
    assert_eq!(failed.iter().map(|f| f.node).collect::<Vec<_>>(), vec![T], "{failed:?}");
}

/// Places a technique with a Words input on `brief` at `T`, fanned out by a batch of `items`.
fn placed_version(kinds: Registry, brief: NodeId, nodes: Vec<Node>, edges: Vec<Edge>, outputs: Vec<TechniquePort>, items: &str) -> (Project, Registry) {
    let technique = Technique {
        id: TechniqueId::from_bytes([12; 16]),
        version: 1,
        name: "Inner".into(),
        description: String::new(),
        category: String::new(),
        tags: Vec::new(),
        inputs: vec![port("Words", brief, "out", TEXT)],
        outputs,
        params: Vec::new(),
        source: None,
        retired: false,
    };
    let version = Canvas::technique(V, "Inner v1", technique, nodes, edges).unwrap();
    let interface = technique::interface_of(&version, &kinds).unwrap();
    let placed = kinds.get(technique::ID).unwrap().instantiate_instance(T, interface.config().unwrap(), &Default::default()).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([4; 16]), "Techniques").unwrap();
    let changes = vec![
        Change::AddCanvas { canvas: version },
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") },
        Change::AddNode { canvas: C, node: placed },
        Change::AddNode { canvas: C, node: batch_node(&kinds, BATCH, items) },
        Change::Connect { canvas: C, edge: edge(41, (BATCH, "item"), (T, "words"), 0) },
    ];
    Changeset::new(changes).apply(&mut project).unwrap();
    (project, kinds)
}

/// Records `result` as `T`'s new result, under an id of its own (every run here has the same).
fn record(project: &mut Project, result: &StepResult) {
    let node = project.canvases().get(&C).unwrap().nodes().get(&T).unwrap().clone();
    let mut generation = result.generation.clone();
    generation.id = nodecraft_types::GenerationId::from_bytes([u8::try_from(node.history.len()).unwrap().saturating_add(50); 16]);
    Changeset::new(crate::run::record_changes(C, &node, generation).unwrap()).apply(project).unwrap();
}

/// An item failing in the first of two fanned inner steps is carried past the second, named
/// once (#536); a re-run that fails it again keeps the finished items, and the next completes.
#[test]
fn an_item_failing_early_in_a_chain_inside_a_technique_is_kept_until_it_finishes() {
    let kinds = kinds();
    let (brief, loud, louder) = (NodeId::from_bytes([90; 16]), NodeId::from_bytes([91; 16]), NodeId::from_bytes([92; 16]));
    let mut first = node(&kinds, loud, "test.shout", "");
    first.title = "Loud".into();
    let mut second = node(&kinds, louder, "test.shout", "");
    second.title = "Louder".into();
    let nodes = vec![node(&kinds, brief, "source.text", "cat"), first, second];
    let edges = vec![edge(93, (brief, "out"), (loud, "in"), 0), edge(94, (loud, "out"), (louder, "in"), 0)];
    let (mut project, kinds) = placed_version(kinds, brief, nodes, edges, vec![port("Shouted", louder, "out", TEXT)], "fox\nowl");

    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    let (finished, failed, ran) = drive_failing(&project, &kinds, &plan, "OWL");
    let [partial] = finished.as_slice() else { panic!("{finished:?} {failed:?}") };
    let [failure] = failed.as_slice() else { panic!("{failed:?}") };
    assert_eq!(failure.reason, "1 of 2 items didn't finish: “Loud”: it said no", "named once, where it failed");
    assert_eq!(ran.iter().filter(|job| matches!(job, JobSpec::Source { value, .. } if *value == Value::Text("OWL".into()))).count(), 1, "Louder never runs it");
    record(&mut project, partial);

    // Failing again: the fox is kept, the owl is missing again.
    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    let (finished, failed, ran) = drive_failing(&project, &kinds, &plan, "OWL");
    let [again] = finished.as_slice() else { panic!("{finished:?} {failed:?}") };
    assert!(!ran.iter().any(|job| matches!(job, JobSpec::Source { value, .. } if *value == Value::Text("FOX".into()))), "the fox doesn't run again: {ran:?}");
    assert_eq!(again.generation.outputs, vec![Value::List(vec![Value::Text("FOX".into()), Value::List(Vec::new())])]);
    assert_eq!(again.generation.incomplete.as_ref().map(|i| i.missing.len()), Some(1));
    record(&mut project, again);

    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    let (finished, failed, ran) = drive_failing(&project, &kinds, &plan, "");
    assert!(failed.is_empty(), "{failed:?}");
    assert!(!ran.iter().any(|job| matches!(job, JobSpec::Source { value, .. } if *value == Value::Text("FOX".into()))), "{ran:?}");
    assert_eq!(outputs(&finished), vec![(T, vec!["FOX+OWL".to_owned()])]);
}

/// Text in, with every value of its `all` list after it.
struct Gather(NodeDescriptor);
impl NodeKind for Gather {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("gather").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let of = |port: &str| ctx.inputs.iter().filter(|input| input.port == port).map(|input| text_of(&input.value)).collect::<Vec<_>>().join("+");
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: Value::Text(format!("{}|{}", of("in"), of("all"))) }])
    }
}

/// An inner step that reads every item of the one before it can't be resumed item by item: the
/// technique node runs in full again (#536).
#[test]
fn a_technique_whose_items_read_each_other_runs_again_in_full() {
    let mut kinds = kinds();
    let gather = NodeDescriptor {
        id: "test.gather".into(),
        label: "Gather".into(),
        category: Category::Action,
        inputs: vec![Port { name: "in".into(), port_type: TEXT }, Port { name: "all".into(), port_type: LIST }],
        outputs: vec![Port { name: "out".into(), port_type: TEXT }],
        params: ParamSchema { params: Vec::new() },
        quick_add: None,
    };
    kinds.register(Box::new(Gather(gather))).unwrap();
    let (brief, shout, gathered) = (NodeId::from_bytes([95; 16]), NodeId::from_bytes([96; 16]), NodeId::from_bytes([97; 16]));
    let nodes = vec![node(&kinds, brief, "source.text", "cat"), node(&kinds, shout, "test.shout", ""), node(&kinds, gathered, "test.gather", "")];
    let edges =
        vec![edge(98, (brief, "out"), (shout, "in"), 0), edge(99, (shout, "out"), (gathered, "in"), 0), edge(100, (shout, "out"), (gathered, "all"), 0)];
    let (mut project, kinds) = placed_version(kinds, brief, nodes, edges, vec![port("Gathered", gathered, "out", TEXT)], "fox\nowl");

    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    let (finished, failed, _) = drive_failing(&project, &kinds, &plan, "OWL|FOX+OWL");
    let [partial] = finished.as_slice() else { panic!("{finished:?} {failed:?}") };
    assert_eq!(partial.generation.incomplete.as_ref().map(|i| i.missing.len()), Some(1), "{partial:?}");
    record(&mut project, partial);

    let plan = ready(planned(&project, &kinds, &[T]).unwrap());
    assert!(plan.steps.iter().all(|s| s.fanout.as_ref().is_none_or(|f| f.resume.is_none())), "no resume: {plan:?}");
    let (finished, failed, ran) = drive_failing(&project, &kinds, &plan, "");
    assert!(failed.is_empty(), "{failed:?}");
    assert!(ran.iter().any(|job| matches!(job, JobSpec::Source { value, .. } if *value == Value::Text("FOX".into()))), "the fox runs again: {ran:?}");
    assert_eq!(outputs(&finished), vec![(T, vec!["FOX|FOX+OWL+OWL|FOX+OWL".to_owned()])]);
}

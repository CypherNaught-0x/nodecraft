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
    plan_expanded(&expanded, kinds, &RunMode::Selected, &requested.iter().copied().collect(), &BTreeSet::new(), &nodecraft_types::EMPTY_TARGET_SELECTION)
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

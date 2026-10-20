//! Planning consults the option lists the host has fetched (#162): a value off a *known* list
//! blocks its node before any job is admitted, quoted or charged, while an unfetched list, a
//! disabled parameter and a scoped list whose scope has no value refuse nothing. The same rules the
//! edit, describe and `node.options` paths already apply, seen from the run side — plus that one
//! off-list node holds up a run that only asked for something downstream of it.

use super::plan_with_options;
use super::*;
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Port, Project};
use nodecraft_nodes::{NodeContract, NodeDescriptor, NodeKind};
use nodecraft_types::{
    CanvasId, Choice, Condition, EdgeId, IssueCode, Modality, OptionKey, OptionLists, OptionSource, ParamControl, ParamRule, ParamSchema, ParamSpec, ProjectId,
};

const C: CanvasId = CanvasId::from_bytes([7; 16]);
const KIND: &str = "test.listed";
const CHECKPOINTS: &str = "fixture.checkpoints";
const LORAS: &str = "fixture.loras";
const OFF_LIST: &str = "removed.safetensors";

/// `checkpoint` (and the `lora` scoped by it) must come from a list the host fetches, and
/// `checkpoint` is only editable while `mode` is fine.
struct Listed(NodeDescriptor);

impl NodeKind for Listed {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn contract(&self, descriptor: &NodeDescriptor) -> NodeContract {
        let mut contract = NodeContract::derive(descriptor);
        let fine = Condition::Equals { param: "mode".into(), value: Value::Text("fine".into()) };
        contract.params.rules = vec![
            ParamRule { options: Some(OptionSource { id: CHECKPOINTS.into(), scope: None }), enabled_when: Some(fine), ..ParamRule::implied("checkpoint") },
            ParamRule { options: Some(OptionSource { id: LORAS.into(), scope: Some("checkpoint".into()) }), ..ParamRule::implied("lora") },
        ];
        contract
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("listed").ok()
    }
    /// Echoes what it would submit, so a test can see that a blocked step never reaches a job.
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let sent = self.validate_instance(ctx.node, true)?.into_visible_values();
        let echo = sent.iter().map(|(name, value)| format!("{name}={value:?}")).collect::<Vec<_>>().join(";");
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: Value::Text(echo) }])
    }
}

fn spec(name: &str, control: ParamControl) -> ParamSpec {
    ParamSpec { name: name.into(), label: name.into(), description: None, control, required: false, default: None, visible_if: None }
}

fn kinds() -> Registry {
    let mut kinds = Registry::builtins().unwrap();
    let text = || ParamControl::Text { multiline: false, max_bytes: 128 };
    let mode = ParamControl::Choice { options: ["plain", "fine"].iter().map(|v| Choice { value: (*v).into(), label: (*v).into() }).collect() };
    let descriptor = NodeDescriptor {
        id: KIND.into(),
        label: "Listed".into(),
        category: Category::Generation,
        inputs: vec![Port { name: "in".into(), port_type: PortType { modality: Modality::Text, many: false } }],
        outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } }],
        params: ParamSchema { params: vec![spec("mode", mode), spec("checkpoint", text()), spec("lora", text())] },
        quick_add: None,
    };
    kinds.register(Box::new(Listed(descriptor))).unwrap();
    kinds
}

/// What the host has fetched: two checkpoints, and one LoRA for `flux.safetensors` alone. The same
/// source id also has an unscoped list, which a scoped parameter is never checked against.
fn fetched() -> OptionLists {
    let mut lists = OptionLists::default();
    let set = |lists: &mut OptionLists, key: OptionKey<'_>, values: &[&str]| {
        lists.set(key, values.iter().map(|v| Choice { value: (*v).into(), label: (*v).into() }).collect()).unwrap()
    };
    set(&mut lists, OptionKey { owner: KIND, source: CHECKPOINTS, scope: None }, &["flux.safetensors", "juggernaut.safetensors"]);
    set(&mut lists, OptionKey { owner: KIND, source: LORAS, scope: Some("flux.safetensors") }, &["ink.safetensors"]);
    set(&mut lists, OptionKey { owner: KIND, source: LORAS, scope: None }, &["unscoped.safetensors"]);
    lists
}

fn node(id: u8, values: &[(&str, &str)]) -> Node {
    let supplied = values.iter().map(|(name, value)| ((*name).to_owned(), Value::Text((*value).to_owned()))).collect();
    kinds().get(KIND).unwrap().instantiate_instance(NodeId::from_bytes([id; 16]), BTreeMap::new(), &supplied).unwrap()
}

/// A canvas holding `nodes`, with each `(up, down)` wired `out` into `down`'s `in` port.
fn canvas(nodes: &[Node], wires: &[(NodeId, NodeId)]) -> Canvas {
    let mut project = Project::new(ProjectId::from_bytes([8; 16]), "Lists").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for node in nodes {
        Change::AddNode { canvas: C, node: node.clone() }.apply(&mut project).unwrap();
    }
    for (n, (from, to)) in wires.iter().enumerate() {
        let byte = n.checked_add(20).and_then(|n| u8::try_from(n).ok()).unwrap();
        let edge = Edge {
            id: EdgeId::from_bytes([byte; 16]),
            from: Endpoint { node: *from, port: "out".into() },
            to: Endpoint { node: *to, port: "in".into() },
            order: 0,
        };
        Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    }
    project.canvases().get(&C).unwrap().clone()
}

fn plan(
    nodes: &[Node],
    wires: &[(NodeId, NodeId)],
    mode: &RunMode,
    requested: &[NodeId],
    stale: &[NodeId],
    options: &dyn nodecraft_types::OptionLookup,
) -> Planned {
    let canvas = canvas(nodes, wires);
    plan_with_options(
        &canvas,
        &kinds(),
        mode,
        &requested.iter().copied().collect(),
        &stale.iter().copied().collect(),
        &nodecraft_types::EMPTY_TARGET_SELECTION,
        options,
    )
    .unwrap()
}

fn planned(node: &Node, options: &dyn nodecraft_types::OptionLookup) -> Planned {
    plan(std::slice::from_ref(node), &[], &RunMode::Selected, &[node.id], &[], options)
}

/// The step's echoed submission, proving the node was planned and not merely unblocked.
fn sent(node: &Node, options: &dyn nodecraft_types::OptionLookup) -> String {
    let Planned::Ready(plan) = planned(node, options) else { panic!("planned: {:?}", planned(node, options)) };
    match plan.steps.first().and_then(|step| step.jobs.as_ref()).and_then(|jobs| jobs.first()) {
        Some(JobSpec::Source { value: Value::Text(echo), .. }) => echo.clone(),
        other => panic!("{other:?}"),
    }
}

fn blocked(node: &Node, options: &dyn nodecraft_types::OptionLookup) -> Vec<Blocker> {
    match planned(node, options) {
        Planned::Blocked(blockers) => blockers,
        Planned::Ready(plan) => panic!("planned a run that should be blocked: {plan:?}"),
    }
}

/// Which parameters the contract itself reports as off a known list. A blocker deliberately
/// carries no parameter (no contract string may reach a run's blocked report), so this is where
/// the plan's decision is pinned to a field: planning blocks on exactly these parameters, and
/// `node.describe`/`node.options` are what name them to a person.
fn off_list_params(node: &Node, options: &dyn nodecraft_types::OptionLookup) -> Vec<String> {
    let contract = kinds().get(KIND).unwrap().instance_contract(node).unwrap();
    let resolution = contract.resolve_with(&node.params, true, options).unwrap();
    resolution.issues.iter().filter(|issue| issue.code == IssueCode::NotAnOption).filter_map(|issue| issue.param.clone()).collect()
}

#[test]
fn a_value_off_a_fetched_list_blocks_the_run_before_any_job_exists() {
    let good = node(1, &[("mode", "fine"), ("checkpoint", "flux.safetensors")]);
    assert!(sent(&good, &fetched()).contains("flux.safetensors"), "a value on the list runs");
    let gone = node(2, &[("mode", "fine"), ("checkpoint", OFF_LIST)]);
    let blockers = blocked(&gone, &fetched());
    assert_eq!(blockers.iter().map(|b| &b.reason).collect::<Vec<_>>(), vec![&Reason::NotAnOption], "the node blocks on its options");
    assert_eq!(blockers[0].node, gone.id);
    // The notice carries no contract or value string; describe and node.options do.
    let message = blockers[0].message(&Canvas::new(C, "Canvas"));
    assert!(message.contains("not one of the available options"), "{message}");
    assert!(!message.contains(OFF_LIST) && !message.contains("checkpoint"), "public-safe: {message}");
    assert_eq!(off_list_params(&gone, &fetched()), vec!["checkpoint".to_owned()], "the contract names the field");
}

#[test]
fn an_unfetched_or_cleared_list_refuses_nothing() {
    let gone = node(3, &[("mode", "fine"), ("checkpoint", OFF_LIST)]);
    // Never fetched, and fetched-then-cleared, are both unknown rather than empty.
    assert!(sent(&gone, &OptionLists::default()).contains(OFF_LIST), "an unfetched list sends the value on");
    let mut cleared = fetched();
    cleared.clear(OptionKey { owner: KIND, source: CHECKPOINTS, scope: None });
    assert!(sent(&gone, &cleared).contains(OFF_LIST), "a cleared list is unknown again");
    // And with no host lists at all, which is what plan_with_targets passes.
    assert!(sent(&gone, &nodecraft_types::NoOptions).contains(OFF_LIST), "an offline host still runs the node");
}

#[test]
fn a_disabled_parameter_draws_from_no_list() {
    // `checkpoint` is only enabled while `mode` is fine; the stored value stays on the node.
    let off = node(4, &[("mode", "plain"), ("checkpoint", OFF_LIST)]);
    assert_eq!(off.params.get("checkpoint"), Some(&Value::Text(OFF_LIST.into())), "editors keep it");
    assert_eq!(sent(&off, &fetched()), r#"mode=Text("plain")"#, "a disabled value is neither checked nor submitted");
    assert_eq!(off_list_params(&off, &fetched()), Vec::<String>::new(), "and it draws from no list");
    let on = node(5, &[("mode", "fine"), ("checkpoint", OFF_LIST)]);
    assert_eq!(blocked(&on, &fetched()).iter().map(|b| &b.reason).collect::<Vec<_>>(), vec![&Reason::NotAnOption]);
    assert_eq!(off_list_params(&on, &fetched()), vec!["checkpoint".to_owned()]);
}

#[test]
fn a_scoped_list_is_the_one_its_scope_value_selects() {
    // ink.safetensors is on the LoRA list for flux.safetensors, and unscoped is on the list of the
    // same source id with no scope: the scoped parameter is checked against the first only.
    let scoped = node(6, &[("mode", "fine"), ("checkpoint", "flux.safetensors"), ("lora", "ink.safetensors")]);
    assert!(sent(&scoped, &fetched()).contains("ink.safetensors"));
    let off = node(7, &[("mode", "fine"), ("checkpoint", "flux.safetensors"), ("lora", "unscoped.safetensors")]);
    assert_eq!(blocked(&off, &fetched()).iter().map(|b| &b.reason).collect::<Vec<_>>(), vec![&Reason::NotAnOption]);
    assert_eq!(off_list_params(&off, &fetched()), vec!["lora".to_owned()], "the scoped list is the one it is off");
    // juggernaut has no fetched LoRA list, so its LoRA is unknown rather than empty; and with no
    // checkpoint there is no scope value, so the unscoped list of that id is not offered.
    let unknown_scope = node(8, &[("mode", "fine"), ("checkpoint", "juggernaut.safetensors"), ("lora", "anything.safetensors")]);
    assert!(sent(&unknown_scope, &fetched()).contains("anything.safetensors"));
    let no_scope = node(9, &[("mode", "plain"), ("lora", "anything.safetensors")]);
    assert!(sent(&no_scope, &fetched()).contains("anything.safetensors"), "no scope value, so nothing is refused");
}

#[test]
fn a_step_without_an_option_source_is_planned_exactly_as_before() {
    // A local source job: no parameter of it names a list, so a host full of fetched lists changes
    // nothing about how it plans, quotes and runs.
    let source = kinds()
        .get("source.text")
        .unwrap()
        .instantiate_instance(NodeId::from_bytes([10; 16]), BTreeMap::new(), &[("text".into(), Value::Text("A lighthouse".into()))].into())
        .unwrap();
    let Planned::Ready(plan) = planned(&source, &fetched()) else { panic!("a local job must not be blocked by option lists") };
    assert_eq!(plan.steps.len(), 1);
    let echo = match plan.steps.first().and_then(|step| step.jobs.as_ref()).and_then(|jobs| jobs.first()) {
        Some(JobSpec::Source { value: Value::Text(text), .. }) => text.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(echo, "A lighthouse");
}

#[test]
fn one_off_list_node_stops_a_run_that_only_asked_for_something_downstream_of_it() {
    // `up` is off a known list; `down` is clean and is all the person asked to run. The run still
    // needs `up`, because it has no result, so the whole run is refused rather than skipped.
    let up = node(11, &[("mode", "fine"), ("checkpoint", OFF_LIST)]);
    let down = node(12, &[("mode", "plain")]);
    let wires = [(up.id, down.id)];
    let nodes = [up.clone(), down.clone()];
    let clean: Vec<NodeId> = Vec::new();
    for (mode, requested, stale) in [
        (RunMode::WithUpstream, vec![down.id], clean.clone()),
        (RunMode::AllStale, clean.clone(), vec![down.id]),
        (RunMode::AllStale, clean.clone(), vec![up.id, down.id]),
    ] {
        let Planned::Blocked(blockers) = plan(&nodes, &wires, &mode, &requested, &stale, &fetched()) else {
            panic!("an upstream node off a known list must block the {mode:?} run");
        };
        assert_eq!(
            blockers.iter().map(|b| (b.node, &b.reason)).collect::<Vec<_>>(),
            vec![(up.id, &Reason::NotAnOption)],
            "one off-list node blocks the whole {mode:?} run and names itself"
        );
    }
    // The same request plans as soon as the upstream value is on the list again.
    let fixed = node(13, &[("mode", "fine"), ("checkpoint", "flux.safetensors")]);
    let Planned::Ready(plan) = plan(&[fixed, down.clone()], &[(NodeId::from_bytes([13; 16]), down.id)], &RunMode::WithUpstream, &[down.id], &[], &fetched())
    else {
        panic!("an on-list upstream step must plan");
    };
    assert_eq!(plan.steps.len(), 2, "the upstream step and the requested one both run");
}

use super::*;
use crate::planner::{Planned, PlannedInput, RunMode, RunPlan};
use nodecraft_graph::{Changeset, Node, Port, Project};
use nodecraft_nodes::{Category, GenerateOutput, ModeId, NodeDescriptor, NodeKind};
use nodecraft_types::{Modality, ParamSchema, PortType, ProjectId};
use std::{borrow::Cow, collections::BTreeSet};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const UP: NodeId = NodeId::from_bytes([2; 16]);
const TARGET: NodeId = NodeId::from_bytes([3; 16]);

struct Generator {
    descriptor: NodeDescriptor,
    jobs: Vec<Vec<GenerateOutput>>,
    dynamic: bool,
    owner: Option<NodeId>,
}
impl NodeKind for Generator {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn instance<'a>(&'a self, _: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        if self.dynamic {
            let mut descriptor = self.descriptor.clone();
            descriptor.outputs.first_mut().unwrap().port_type.modality = Modality::Image;
            Ok(Cow::Owned(descriptor))
        } else {
            Ok(Cow::Borrowed(&self.descriptor))
        }
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("generate").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(self.jobs.iter().map(|outputs| generate(self.owner.unwrap_or(ctx.node.id), outputs.clone())).collect())
    }
}
fn route(port: &str, modality: Modality) -> GenerateOutput {
    GenerateOutput { port: port.into(), modality }
}
fn generate(node: NodeId, outputs: Vec<GenerateOutput>) -> JobSpec {
    JobSpec::Generate { instance: None, node, outputs, provider: "fixture".into(), model: "fixture".into(), params: Default::default(), inputs: vec![] }
}
fn fixture(jobs: Vec<Vec<GenerateOutput>>, dynamic: bool) -> (Project, Registry) {
    fixture_owner(jobs, dynamic, None)
}
fn fixture_owner(jobs: Vec<Vec<GenerateOutput>>, dynamic: bool, owner: Option<NodeId>) -> (Project, Registry) {
    let text = PortType { modality: Modality::Text, many: false };
    let descriptor = NodeDescriptor {
        id: "test.output_contract".into(),
        label: "Output contract".into(),
        category: Category::Generation,
        inputs: vec![Port { name: "in".into(), port_type: text }],
        outputs: vec![
            Port { name: "out".into(), port_type: PortType { many: true, ..text } },
            Port { name: "image".into(), port_type: PortType { modality: Modality::Image, many: false } },
        ],
        params: ParamSchema {
            params: vec![nodecraft_types::ParamSpec {
                name: "model".into(),
                label: "Model".into(),
                description: None,
                control: nodecraft_types::ParamControl::Model,
                required: false,
                default: Some(Value::Text("fixture".into())),
                visible_if: None,
            }],
        },
        quick_add: None,
    };
    let mut kinds = Registry::builtins().unwrap();
    kinds.register(Box::new(Generator { descriptor, jobs, dynamic, owner })).unwrap();
    let target = kinds.get("test.output_contract").unwrap().instantiate_instance(TARGET, Default::default(), &Default::default()).unwrap();
    let source =
        kinds.get("source.text").unwrap().instantiate_instance(UP, Default::default(), &[("text".into(), Value::Text("prompt".into()))].into()).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([4; 16]), "Output contract").unwrap();
    Changeset::new(vec![
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") },
        Change::AddNode { canvas: C, node: source },
        Change::AddNode { canvas: C, node: target },
    ])
    .apply(&mut project)
    .unwrap();
    (project, kinds)
}
fn step(node: NodeId, kind: &str, jobs: Option<Vec<JobSpec>>) -> Step {
    Step { node, kind: kind.into(), mode: ModeId::new("generate").unwrap(), after: vec![], inputs: vec![], jobs, fanout: None }
}
fn options() -> RunOptions {
    RunOptions { id: [7; 16], first_job: 40, limits: RunLimits::default(), retry: RetryPolicy::NEVER }
}
fn start_manual(project: &Project, kinds: &Registry, jobs: Vec<JobSpec>) -> (Run, Progress) {
    let canvas = project.canvases().get(&C).unwrap();
    if jobs.iter().any(|j| matches!(j, JobSpec::Generate { .. }))
        && crate::generation_contract::validate_outputs(canvas.nodes().get(&TARGET).unwrap(), &jobs).is_ok()
    {
        let catalog = crate::admission::tests::free_catalog("fixture", "fixture", "fixture");
        let (admitted, _) = crate::admission::admit_default_fixture(
            RunPlan { steps: vec![step(TARGET, "test.output_contract", Some(jobs))], edges: vec![] },
            C,
            canvas.clone(),
            kinds,
            &catalog,
            Default::default(),
        )
        .unwrap();
        return Run::start_admitted(admitted, kinds, options()).unwrap();
    }
    Run::start(
        &RunPlan { steps: vec![step(TARGET, "test.output_contract", Some(jobs))], edges: vec![] },
        C,
        project.canvases().get(&C).unwrap(),
        kinds,
        options(),
    )
    .unwrap()
}
fn no_partial_dispatch(run: &Run, progress: &Progress) {
    assert!(progress.jobs.is_empty(), "no invalid step work reaches scheduler/provider registration");
    assert_eq!(run.next_job, 40, "no job ids allocated to a rejected vector");
    assert!(run.jobs.is_empty(), "no JobState prefix installed");
    assert_eq!(progress.failed.len(), 1);
    assert_eq!(progress.failed[0].node, TARGET);
}
#[test]
fn eager_planner_blocks_a_route_with_the_wrong_port_modality() {
    let (project, kinds) = fixture(vec![vec![route("out", Modality::Image)]], false);
    let result = plan(project.canvases().get(&C).unwrap(), &kinds, &RunMode::Selected, &BTreeSet::from([TARGET]), &BTreeSet::new()).unwrap();
    assert!(
        matches!(result, Planned::Blocked(ref blockers) if blockers.iter().any(|b| b.node == TARGET && matches!(b.reason, crate::planner::Reason::Invalid(_))))
    );
}
#[test]
fn manual_plan_cannot_bypass_unknown_port_validation() {
    let (project, kinds) = fixture(vec![], false);
    let (run, progress) = start_manual(&project, &kinds, vec![generate(TARGET, vec![route("missing", Modality::Text)])]);
    no_partial_dispatch(&run, &progress);
}
#[test]
fn a_valid_first_job_and_invalid_second_job_publish_no_prefix() {
    let (project, kinds) = fixture(vec![], false);
    let (run, progress) =
        start_manual(&project, &kinds, vec![generate(TARGET, vec![route("out", Modality::Text)]), generate(TARGET, vec![route("out", Modality::Image)])]);
    no_partial_dispatch(&run, &progress);
}
#[test]
fn deferred_materialisation_checks_routes_before_dispatch() {
    let (project, kinds) = fixture(vec![vec![route("out", Modality::Image)]], false);
    let mut deferred = step(TARGET, "test.output_contract", None);
    deferred.after = vec![UP];
    deferred.inputs = vec![PlannedInput {
        port: "in".into(),
        port_type: PortType { modality: Modality::Text, many: false },
        from: vec![InputSource::Step { node: UP, port: "out".into(), list: false }],
    }];
    let source = JobSpec::Source { node: UP, output: "out".into(), value: Value::Text("prompt".into()) };
    let plan = RunPlan { steps: vec![step(UP, "source.text", Some(vec![source])), deferred], edges: vec![(UP, TARGET)] };
    let (mut run, first) = Run::start(&plan, C, project.canvases().get(&C).unwrap(), &kinds, options()).unwrap();
    assert_eq!(first.jobs.len(), 1);
    let progress = run.job_done(&kinds, first.jobs[0].id, vec![("out".into(), Value::Text("prompt".into()))]);
    assert!(progress.jobs.is_empty(), "upstream completed, but invalid generation must never dispatch");
    assert_eq!(run.next_job, 41);
    assert_eq!(run.jobs.len(), 1);
    assert!(progress.failed.iter().any(|f| f.node == TARGET));
}
#[test]
fn mixed_modalities_lists_and_duplicate_valid_routes_still_dispatch() {
    let routes = vec![route("out", Modality::Text), route("image", Modality::Image), route("out", Modality::Text)];
    let (project, kinds) = fixture(vec![routes.clone()], false);
    let (run, progress) = start_manual(&project, &kinds, vec![generate(TARGET, routes)]);
    assert_eq!(progress.jobs.len(), 1);
    assert!(progress.failed.is_empty());
    assert_eq!(run.jobs.len(), 1);
}
#[test]
fn dynamic_instance_outputs_are_authoritative_instead_of_static_palette_ports() {
    let routes = vec![route("out", Modality::Image)];
    let (project, kinds) = fixture(vec![routes.clone()], true);
    let planned = plan(project.canvases().get(&C).unwrap(), &kinds, &RunMode::Selected, &BTreeSet::from([TARGET]), &BTreeSet::new()).unwrap();
    let Planned::Ready(plan) = planned else { panic!("valid dynamic output") };
    let catalog = crate::admission::tests::free_catalog("fixture", "fixture", "fixture");
    let (admitted, _) =
        crate::admission::admit_default_fixture(plan, C, project.canvases().get(&C).unwrap().clone(), &kinds, &catalog, Default::default()).unwrap();
    let (_, progress) = Run::start_admitted(admitted, &kinds, options()).unwrap();
    assert_eq!(progress.jobs.len(), 1);
    assert!(progress.failed.is_empty());
}
#[test]
fn source_work_is_unchanged_by_the_generate_contract() {
    let (project, kinds) = fixture(vec![], false);
    let (_, progress) = start_manual(&project, &kinds, vec![JobSpec::Source { node: TARGET, output: "unchanged".into(), value: Value::Text("source".into()) }]);
    assert_eq!(progress.jobs.len(), 1);
    assert!(progress.failed.is_empty());
}

#[test]
fn hostile_unknown_route_has_a_bounded_static_diagnostic() {
    let (project, kinds) = fixture(vec![], false);
    let hostile = "private-provider-secret".repeat(65_536);
    let (run, progress) = start_manual(&project, &kinds, vec![generate(TARGET, vec![route(&hostile, Modality::Text)])]);
    no_partial_dispatch(&run, &progress);
    assert_eq!(progress.failed[0].reason, "a generated output targets an unknown output port");
}

#[test]
fn eager_planner_rejects_a_foreign_generate_owner() {
    let (project, kinds) = fixture_owner(vec![vec![route("out", Modality::Text)]], false, Some(UP));
    let result = plan(project.canvases().get(&C).unwrap(), &kinds, &RunMode::Selected, &BTreeSet::from([TARGET]), &BTreeSet::new()).unwrap();
    assert!(
        matches!(result, Planned::Blocked(ref blockers) if blockers.iter().any(|b| b.node == TARGET && matches!(b.reason, crate::planner::Reason::ProviderTarget(crate::planner::TargetReason::Mismatch))))
    );
}
#[test]
fn manual_plan_rejects_foreign_owner_even_without_routes() {
    let (project, kinds) = fixture(vec![], false);
    let (run, progress) = start_manual(&project, &kinds, vec![generate(UP, vec![])]);
    no_partial_dispatch(&run, &progress);
    assert_eq!(progress.failed[0].reason, "a generated job does not belong to its step node");
}
#[test]
fn a_valid_first_job_and_foreign_second_job_publish_no_prefix() {
    let (project, kinds) = fixture(vec![], false);
    let (run, progress) =
        start_manual(&project, &kinds, vec![generate(TARGET, vec![route("out", Modality::Text)]), generate(UP, vec![route("out", Modality::Text)])]);
    no_partial_dispatch(&run, &progress);
}
#[test]
fn deferred_materialisation_rejects_foreign_owner_before_dispatch() {
    let (project, kinds) = fixture_owner(vec![vec![route("out", Modality::Text)]], false, Some(UP));
    let mut deferred = step(TARGET, "test.output_contract", None);
    deferred.after = vec![UP];
    deferred.inputs = vec![PlannedInput {
        port: "in".into(),
        port_type: PortType { modality: Modality::Text, many: false },
        from: vec![InputSource::Step { node: UP, port: "out".into(), list: false }],
    }];
    let source = JobSpec::Source { node: UP, output: "out".into(), value: Value::Text("prompt".into()) };
    let plan = RunPlan { steps: vec![step(UP, "source.text", Some(vec![source])), deferred], edges: vec![(UP, TARGET)] };
    let (mut run, first) = Run::start(&plan, C, project.canvases().get(&C).unwrap(), &kinds, options()).unwrap();
    assert_eq!(first.jobs.len(), 1);
    let progress = run.job_done(&kinds, first.jobs[0].id, vec![("out".into(), Value::Text("prompt".into()))]);
    assert!(progress.jobs.is_empty(), "upstream completed, but invalid generation must never dispatch");
    assert_eq!(run.next_job, 41);
    assert_eq!(run.jobs.len(), 1);
    assert!(progress.failed.iter().any(|f| f.node == TARGET));
}

fn plan(canvas: &Canvas, kinds: &Registry, mode: &RunMode, requested: &BTreeSet<NodeId>, stale: &BTreeSet<NodeId>) -> Result<crate::planner::Planned, Error> {
    let mut targets = nodecraft_types::TargetSelection::empty();
    for node in canvas.nodes().keys() {
        targets.insert(*node, &nodecraft_types::ProviderTarget::default_instance("fixture")?)?;
    }
    crate::planner::plan_with_targets(canvas, kinds, mode, requested, stale, &targets)
}

//! The actual main converter and Run boundary, independent of any optional adapter feature.
use nodecraft_engine::generate::{produced, provider_job};
use nodecraft_exec::{
    planner::{RunPlan, Step},
    run::{Run, RunLimits, RunOptions},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Port, Project};
use nodecraft_nodes::{Category, GenerateInput, GenerateOutput, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, Registry};
use nodecraft_providers::{Completed, Output};
use nodecraft_types::{CanvasId, Error, Modality, NodeId, ParamSchema, PortType, ProjectId, Value};
use std::sync::Arc;
struct Kind(NodeDescriptor);
impl NodeKind for Kind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("fixture").ok()
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![])
    }
}
fn image() -> Output {
    Output::Bytes { mime: "image/jpeg".into(), bytes: Arc::from(include_bytes!("../../providers/tests/fixtures/gemini_images/original-square.jpg").as_slice()) }
}
fn route(modality: Modality) -> GenerateOutput {
    GenerateOutput { port: if modality == Modality::Text { "text" } else { "image" }.into(), modality }
}
fn assemble(done: &Completed, text: bool, many: bool) -> nodecraft_exec::run::Progress {
    let cid = CanvasId::from_bytes([1; 16]);
    let nid = NodeId::from_bytes([2; 16]);
    let mut outputs = vec![Port { name: "image".into(), port_type: PortType { modality: Modality::Image, many } }];
    let mut routes = vec![route(Modality::Image)];
    if text {
        outputs.push(Port { name: "text".into(), port_type: PortType { modality: Modality::Text, many: true } });
        routes.push(route(Modality::Text));
    }
    let descriptor = NodeDescriptor {
        id: "test.result".into(),
        label: "Result fixture".into(),
        category: Category::Generation,
        inputs: vec![],
        outputs,
        params: ParamSchema::default(),
        quick_add: None,
    };
    let node = descriptor.instantiate(nid, &Default::default()).unwrap();
    let mut kinds = Registry::builtins().unwrap();
    kinds.register(Box::new(Kind(descriptor))).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([3; 16]), "Fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(cid, "Fixture") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: cid, node }.apply(&mut project).unwrap();
    let plan = RunPlan {
        steps: vec![Step {
            node: nid,
            kind: "test.result".into(),
            mode: ModeId::new("fixture").unwrap(),
            after: vec![],
            inputs: vec![],
            jobs: Some(vec![JobSpec::Generate {
                node: nid,
                outputs: routes.clone(),
                provider: "gemini-images".into(),
                model: "gemini-3.1-flash-lite-image".into(),
                params: Default::default(),
                inputs: vec![],
            }]),
        }],
        edges: vec![],
    };
    let (mut run, start) = Run::start(
        &plan,
        cid,
        project.canvases().get(&cid).unwrap(),
        &kinds,
        RunOptions { id: [4; 16], first_job: 1, limits: RunLimits::default(), retry: RetryPolicy::NEVER },
    )
    .unwrap();
    let converted = produced(&routes, done).unwrap();
    run.job_done(&kinds, start.jobs[0].id, converted.outputs)
}
#[test]
fn mixed_refusal_preserves_original_paid_result_and_explicit_list_ports_keep_every_item() {
    let done = Completed::new(vec![Output::Text("first".into()), image(), Output::Text("second".into()), image()]);
    let original = done.clone();
    assert!(produced(&[route(Modality::Image)], &done).is_err());
    assert_eq!(done, original);
    let p = assemble(&done, true, true);
    assert!(p.failed.is_empty());
    let values = &p.finished[0].generation.outputs;
    assert!(matches!(&values[0],Value::List(v)if v.len()==2));
    assert_eq!(values[1], Value::List(vec![Value::Text("first".into()), Value::Text("second".into())]));
    assert_eq!(done, original);
}
#[test]
fn scalar_multiple_refuses_without_consuming_host_owned_completed() {
    let done = Completed::new(vec![image(), image()]);
    let original = done.clone();
    let p = assemble(&done, false, false);
    assert!(p.finished.is_empty());
    assert_eq!(p.failed.len(), 1);
    assert!(p.failed[0].reason.contains("several values"));
    assert_eq!(done, original);
    let p = assemble(&Completed::new(vec![image()]), false, false);
    assert_eq!(p.finished.len(), 1);
    assert!(p.failed.is_empty());
}
#[test]
fn graph_media_converter_still_requires_an_asset_resolving_host() {
    let done = Completed::new(vec![image()]);
    let converted = produced(&[route(Modality::Image)], &done).unwrap();
    let spec = JobSpec::Generate {
        node: NodeId::from_bytes([7; 16]),
        outputs: vec![route(Modality::Image)],
        provider: "gemini-images".into(),
        model: "gemini-3.1-flash-lite-image".into(),
        params: Default::default(),
        inputs: vec![GenerateInput { role: "image".into(), value: converted.outputs[0].1.clone() }],
    };
    assert!(provider_job(&spec, "fixture").is_err());
    assert_eq!(done.outputs.len(), 1);
}

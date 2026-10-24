//! Every deferred fanout cell is checked, with native-hook provenance preserved.
use nodecraft_exec::planner::{self, Planned, Reason, RunMode};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Port, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeContract, NodeDescriptor, NodeKind, PlanCtx, PreflightCtx, Registry};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, Modality, NodeId, ParamSchema, PortType, ProjectId, Value};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

struct NativeKind {
    descriptor: NodeDescriptor,
    checked: Arc<Mutex<Vec<String>>>,
}
impl NodeKind for NativeKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("text-to-text").ok()
    }
    fn contract(&self, descriptor: &NodeDescriptor) -> NodeContract {
        let mut contract = NodeContract::derive(descriptor);
        for input in &mut contract.inputs {
            if input.port == "prompt" {
                input.fans_out = true;
            }
        }
        contract
    }
    fn check_known_inputs(&self, ctx: &PreflightCtx<'_>) -> Result<(), Error> {
        for input in ctx.inputs.iter().filter(|input| input.port == "prompt") {
            for value in &input.known {
                if let Value::Text(text) = value {
                    self.checked.lock().unwrap().push(text.clone());
                    if text == "bad" {
                        return Err(Error::new(ErrorKind::InvalidInput, "PRIVATE_NATIVE_PREFLIGHT_DETAIL"));
                    }
                }
            }
        }
        Ok(())
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: Value::Text("later".into()) }])
    }
}
fn descriptor(id: &str, inputs: Vec<Port>) -> NodeDescriptor {
    NodeDescriptor {
        id: id.into(),
        label: "Native test kind".into(),
        category: Category::Generation,
        inputs,
        outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } }],
        params: ParamSchema::default(),
        quick_add: None,
    }
}
#[test]
fn a_later_deferred_fanout_cell_cannot_hide_behind_the_first_and_native_details_stay_private() {
    let checked = Arc::new(Mutex::new(Vec::new()));
    let mut kinds = Registry::builtins().unwrap();
    kinds.register(Box::new(NativeKind { descriptor: descriptor("fixture.upstream", vec![]), checked: checked.clone() })).unwrap();
    kinds
        .register(Box::new(NativeKind {
            descriptor: descriptor(
                "fixture.private",
                vec![
                    Port { name: "prompt".into(), port_type: PortType { modality: Modality::Text, many: true } },
                    Port { name: "other".into(), port_type: PortType { modality: Modality::Text, many: false } },
                ],
            ),
            checked: checked.clone(),
        }))
        .unwrap();
    let c = CanvasId::from_bytes([95; 16]);
    let batch = NodeId::from_bytes([1; 16]);
    let upstream = NodeId::from_bytes([2; 16]);
    let target = NodeId::from_bytes([3; 16]);
    let mut project = Project::new(ProjectId::from_bytes([95; 16]), "Known input fanout").unwrap();
    Change::AddCanvas { canvas: Canvas::new(c, "Canvas") }.apply(&mut project).unwrap();
    for (id, kind, params) in [
        (batch, nodecraft_nodes::batch::ID, [("text".into(), Value::Text("good\nbad".into()))].into()),
        (upstream, "fixture.upstream", Default::default()),
        (target, "fixture.private", Default::default()),
    ] {
        let node = kinds.get(kind).unwrap().instantiate_instance(id, Default::default(), &params).unwrap();
        Change::AddNode { canvas: c, node }.apply(&mut project).unwrap();
    }
    for (n, from, out, port) in [(10, batch, "item", "prompt"), (11, upstream, "out", "other")] {
        Change::Connect {
            canvas: c,
            edge: Edge {
                id: EdgeId::from_bytes([n; 16]),
                from: Endpoint { node: from, port: out.into() },
                to: Endpoint { node: target, port: port.into() },
                order: 0,
            },
        }
        .apply(&mut project)
        .unwrap();
    }
    let planned = planner::plan(project.canvases().get(&c).unwrap(), &kinds, &RunMode::WithUpstream, &[target].into(), &BTreeSet::new()).unwrap();
    let Planned::Blocked(blockers) = planned else { panic!("later fanout cell must block: {planned:?}") };
    assert_eq!(*checked.lock().unwrap(), ["good", "bad"]);
    let blocker = blockers.iter().find(|blocker| blocker.node == target).unwrap();
    assert!(matches!(&blocker.reason, Reason::Invalid(message) if message == "PRIVATE_NATIVE_PREFLIGHT_DETAIL"));
    assert!(!blocker.reason.validation_message().contains("PRIVATE_NATIVE"));
}

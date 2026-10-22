//! Model requests too large to send (#307): parameters and inputs that each pass graph
//! validation can still exceed what one model call admits together. `run.quote` shows the run
//! as blocked before its cost is accepted, and `run.start` refuses it, even with a quote taken
//! before. No live calls.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    provider_routing::ProviderRouting,
    run_cmds::{BudgetState, RunCatalog},
};
use nodecraft_exec::cost::Budgets;
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Port, Project};
use nodecraft_nodes::{Category, GenerateOutput, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
use nodecraft_providers::mock::MockProvider;
use nodecraft_types::{CanvasId, EdgeId, Error, Modality, NodeId, ParamControl, ParamSchema, ParamSpec, PortType, ProjectId, Value};
use serde_json::json;
use std::sync::Arc;

const C: CanvasId = CanvasId::from_bytes([70; 16]);
const NARROW: NodeId = NodeId::from_bytes([1; 16]);
const WIDE: NodeId = NodeId::from_bytes([2; 16]);
const DOWNSTREAM: NodeId = NodeId::from_bytes([3; 16]);
const HALF: usize = 2048;

/// A model card sending one half-limit list, or two when `wide` is set: each passes alone,
/// together they exceed the 4096 values one request admits.
struct Wide(NodeDescriptor);
impl NodeKind for Wide {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("generate").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let list = || Value::List(vec![Value::Bool(true); HALF]);
        let mut params = std::collections::BTreeMap::new();
        params.insert("a".to_owned(), list());
        if ctx.node.params.get("wide") == Some(&Value::Bool(true)) {
            params.insert("b".to_owned(), list());
        }
        let inputs = ctx.inputs.iter().map(|input| nodecraft_nodes::GenerateInput { role: input.port.clone(), value: input.value.clone() }).collect();
        Ok(vec![JobSpec::Generate {
            instance: None,
            node: ctx.node.id,
            outputs: vec![GenerateOutput { port: "text".into(), modality: Modality::Text, style_family: None }],
            provider: "mock".into(),
            model: "mock-text".into(),
            params: params.into_iter().collect(),
            inputs,
        }])
    }
}

fn session() -> Session {
    let text = PortType { modality: Modality::Text, many: false };
    let descriptor = NodeDescriptor {
        id: "fixture.wide".into(),
        label: "Wide".into(),
        category: Category::Generation,
        inputs: vec![Port { name: "prompt".into(), port_type: text }],
        outputs: vec![Port { name: "text".into(), port_type: text }],
        params: ParamSchema {
            params: vec![ParamSpec {
                name: "wide".into(),
                label: "Wide".into(),
                description: None,
                control: ParamControl::Toggle,
                required: false,
                default: Some(Value::Bool(false)),
                visible_if: None,
            }],
        },
        quick_add: None,
    };
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([71; 16]), "Request size").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for (id, wide) in [(NARROW, false), (WIDE, true), (DOWNSTREAM, false)] {
        let node = descriptor.instantiate(id, &[("wide".into(), Value::Bool(wide))].into()).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let edge = Edge {
        id: EdgeId::from_bytes([72; 16]),
        from: Endpoint { node: NARROW, port: "text".into() },
        to: Endpoint { node: DOWNSTREAM, port: "prompt".into() },
        order: 0,
    };
    Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    kinds.register(Box::new(Wide(descriptor))).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    resources.insert(BudgetState { budgets: Budgets::default(), spent: Default::default() });
    let mut services = Services::default();
    services.register_run_host(|_| Ok(json!({"accepted":true}))).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(MockProvider::standard())).unwrap();
    for id in [NARROW, WIDE, DOWNSTREAM] {
        routing.select(id, "mock", binding.target()).unwrap();
    }
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    s.set_provider_routing(routing).unwrap();
    s
}

fn catalog() -> Catalog {
    Catalog::from_toml(
        r#"[[model]]
id = "fixture.wide"
provider = "mock"
provider_model = "mock-text"
label = "Fixture"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.01 USD" }
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#,
    )
    .unwrap()
}

fn request(nodes: &[NodeId]) -> serde_json::Value {
    json!({"canvas":C,"mode":"selected","nodes":nodes})
}

#[test]
fn a_request_over_the_combined_limit_is_blocked_in_the_quote_and_refused_at_start() {
    let mut s = session();
    let q = s.dispatch("run.quote", &request(&[NARROW])).unwrap();
    let requests = q["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1, "{q}");
    assert_eq!(requests[0]["node"], json!(NARROW));
    assert_eq!(requests[0]["values"], json!(HALF + 1));
    assert_eq!(requests[0]["value_limit"], json!(4096));
    assert_eq!(requests[0]["refused"], json!(null));
    assert!(!q.to_string().contains("request_refused"), "{q}");

    let q = s.dispatch("run.quote", &request(&[WIDE])).unwrap();
    assert_eq!(q["blocked"].as_array().map(Vec::len), Some(1), "{q}");
    assert_eq!(q["allowed"], false, "{q}");
    assert!(q["requests"][0]["refused"].is_string(), "{q}");
    assert!(q.to_string().contains("[request_refused]"), "{q}");
    let mut start = request(&[WIDE]);
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":{"USD":10_000}});
    let refused = s.dispatch("run.start", &start).unwrap_err();
    assert!(refused.message.contains("[request_refused]"), "{refused:?}");
}

#[test]
fn a_quote_taken_before_an_edit_cannot_start_the_larger_request() {
    let mut s = session();
    let q = s.dispatch("run.quote", &request(&[NARROW])).unwrap();
    assert_eq!(q["requests"][0]["refused"], json!(null), "{q}");
    s.dispatch("node.param_set", &json!({"canvas":C,"id":NARROW,"name":"wide","value":{"type":"bool","value":true}})).unwrap();
    let mut start = request(&[NARROW]);
    start["accept"] = json!({"token":q["token"],"partial":false,"unpriced_jobs":0,"amounts":{"USD":10_000}});
    let refused = s.dispatch("run.start", &start).unwrap_err();
    assert!(refused.message.contains("changed since quotation"), "{refused:?}");
    let q = s.dispatch("run.quote", &request(&[NARROW])).unwrap();
    assert!(q["blocked"][0].as_str().is_some_and(|reason| reason.contains("[request_refused]")), "{q}");
    assert_eq!(q["requests_complete"], true, "{q}");
    // Starting with the fresh token reaches the request check itself.
    start["accept"]["token"] = q["token"].clone();
    let refused = s.dispatch("run.start", &start).unwrap_err();
    assert!(refused.message.contains("[request_refused]"), "{refused:?}");
}

#[test]
fn a_step_waiting_on_upstream_results_is_checked_when_it_is_converted() {
    let mut s = session();
    let q = s.dispatch("run.quote", &request(&[NARROW, DOWNSTREAM])).unwrap();
    let nodes: Vec<_> = q["requests"].as_array().unwrap().iter().map(|r| r["node"].clone()).collect();
    assert_eq!(nodes, vec![json!(NARROW)], "the downstream job is not known until its upstream has run: {q}");
}

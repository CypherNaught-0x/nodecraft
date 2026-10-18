use super::*;
use crate::Session;
use crate::commands::{Registry, Resources, Services};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Port, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, ParamControl, ParamSchema, ParamSpec, PlanCtx};
use nodecraft_types::{Currency, EdgeId, GenerationId, Modality, PortType, ProjectId, Unsigned, Value as V};
use std::sync::Mutex;

const C: CanvasId = CanvasId::from_bytes([1; 16]);

const CATALOG: &str = r#"
[[model]]
id = "test.img"
provider = "test"
provider_model = "img-v1"
label = "Test Image"
description = "Priced per image."
latency_s = 10
quality = 4
pricing = { type = "per_unit", unit = "image", price = "0.04 USD", count = "num_images" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.params.params]]
name = "num_images"
label = "Images"
control = { type = "unsigned", min = "1", max = "4" }
default = { type = "unsigned", value = "1" }
"#;

/// A generator with a model picker and an image count.
struct Gen(NodeDescriptor);
impl NodeKind for Gen {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("gen").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: V::Text("generated".into()) }])
    }
}

fn kinds() -> nodecraft_nodes::Registry {
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let spec =
        |name: &str, control| ParamSpec { name: name.into(), label: name.into(), description: None, control, required: false, default: None, visible_if: None };
    kinds
        .register(Box::new(Gen(NodeDescriptor {
            id: "test.gen".into(),
            label: "Gen".into(),
            category: Category::Generation,
            inputs: vec![Port { name: "in".into(), port_type: PortType { modality: Modality::Text, many: false } }],
            outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Image, many: false } }],
            params: ParamSchema {
                params: vec![
                    spec("model", ParamControl::Model),
                    spec("num_images", ParamControl::Unsigned { min: Some(Unsigned(1)), max: Some(Unsigned(4)), step: Unsigned(1) }),
                ],
            },
            quick_add: None,
        })))
        .unwrap();
    kinds
}

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn usd(micros: u64) -> Money {
    Money::new(Currency::USD, micros)
}

/// Text 1 → generator 2 (test.img × 3 = 0.12 USD), and a generator 3 with no model.
fn project() -> Project {
    let kinds = kinds();
    let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Run").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    let text =
        kinds.get("source.text").unwrap().descriptor().instantiate(id(1), &[("text".to_owned(), V::Text("a fox".into()))].into_iter().collect()).unwrap();
    let priced = [("model".to_owned(), V::Text("test.img".into())), ("num_images".to_owned(), V::Unsigned(Unsigned(3)))].into_iter().collect();
    let generator = kinds.get("test.gen").unwrap().descriptor().instantiate(id(2), &priced).unwrap();
    let unpriced = kinds.get("test.gen").unwrap().descriptor().instantiate(id(3), &Default::default()).unwrap();
    let wire = |e: u8, to: u8| Edge {
        id: EdgeId::from_bytes([e; 16]),
        from: Endpoint { node: id(1), port: "out".into() },
        to: Endpoint { node: id(to), port: "in".into() },
        order: 0,
    };
    nodecraft_graph::Changeset::new(vec![
        Change::AddNode { canvas: C, node: text },
        Change::AddNode { canvas: C, node: generator },
        Change::AddNode { canvas: C, node: unpriced },
        Change::Connect { canvas: C, edge: wire(10, 2) },
        Change::Connect { canvas: C, edge: wire(11, 3) },
    ])
    .apply(&mut p)
    .unwrap();
    p
}

type Started = Arc<Mutex<Vec<Value>>>;

fn session(budgets: Budgets, with_service: bool) -> (Session, Started) {
    let mut resources = Resources::default();
    resources.insert(RunCatalog(Arc::new(Catalog::from_toml(CATALOG).unwrap())));
    resources.insert(BudgetState { budgets, spent: Spent::default() });
    resources.insert(UsageSnapshot(Arc::new(UsageLog::default())));
    let started: Started = Arc::default();
    let mut services = Services::default();
    if with_service {
        let log = Arc::clone(&started);
        services
            .register(RUN_SERVICE, move |request| {
                log.lock().unwrap().push(request.clone());
                Ok(json!({"run": "accepted"}))
            })
            .unwrap();
    }
    let session = Session::with_registries(project(), Registry::builtins().unwrap(), kinds()).unwrap().with_resources(resources).with_services(services);
    (session, started)
}

fn request(nodes: &[u8]) -> Value {
    json!({"canvas": C, "mode": "selected", "nodes": nodes.iter().map(|n| id(*n)).collect::<Vec<_>>()})
}

fn with_accept(nodes: &[u8], accept: Value) -> Value {
    let mut v = request(nodes);
    v["accept"] = accept;
    v
}

#[test]
fn a_quote_prices_each_step_read_only() {
    let (mut s, _) = session(Budgets::default(), true);
    let revision = s.revision();
    let q = s.dispatch("run.quote", &request(&[2, 3])).unwrap();
    assert_eq!(s.revision(), revision);
    assert!(!s.can_undo());
    assert_eq!(q["totals"]["USD"]["micros"], 120_000);
    assert_eq!(q["totals"]["USD"]["text"], "0.12 USD");
    assert_eq!(q["complete"], false);
    assert_eq!(q["allowed"], true);
    let steps = q["steps"].as_array().unwrap();
    let step = |n: u8| steps.iter().find(|s| s["node"] == json!(id(n))).unwrap();
    assert_eq!(step(2)["price"]["micros"], 120_000);
    assert_eq!(step(3)["unpriced"], "no model is chosen");
    // A request that can't run yet reports why.
    let mut lone = project();
    Change::AddNode { canvas: C, node: nodecraft_graph::Node::new(id(7), "missing.kind") }.apply(&mut lone).unwrap();
    let mut blocked = Session::with_registries(lone, Registry::builtins().unwrap(), kinds()).unwrap();
    let mut resources = Resources::default();
    resources.insert(RunCatalog(Arc::new(Catalog::from_toml(CATALOG).unwrap())));
    resources.insert(BudgetState { budgets: Budgets::default(), spent: Spent::default() });
    blocked = blocked.with_resources(resources);
    let q = blocked.dispatch("run.quote", &request(&[7])).unwrap();
    assert!(q["blocked"].as_array().is_some_and(|b| !b.is_empty()), "{q}");
}

#[test]
fn a_run_starts_only_with_an_accepted_price_within_the_budgets() {
    let (mut s, started) = session(Budgets::default(), true);
    // Nothing accepted, too little, or a partial price without saying so: refused.
    assert_eq!(s.dispatch("run.start", &with_accept(&[2], json!({}))).unwrap_err().kind, ErrorKind::InvalidInput);
    assert!(s.dispatch("run.start", &with_accept(&[2], json!({"amounts": {"USD": 100_000}}))).is_err());
    assert!(s.dispatch("run.start", &with_accept(&[2, 3], json!({"amounts": {"USD": 120_000}}))).is_err(), "3 has no price");
    assert!(started.lock().unwrap().is_empty());
    // Accepted: the host service gets the request and the quote.
    let out = s.dispatch("run.start", &with_accept(&[2], json!({"amounts": {"USD": 120_000}}))).unwrap();
    assert_eq!(out, json!({"run": "accepted"}));
    assert!(s.dispatch("run.start", &with_accept(&[2, 3], json!({"amounts": {"USD": 200_000}, "partial": true}))).is_ok());
    let requests = started.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["quote"]["totals"]["USD"]["micros"], 120_000);
    assert_eq!(requests[0]["mode"], "selected");
    // Over a session cap: refused whatever was accepted.
    let (mut capped, started) = session(Budgets { session: Some(usd(100_000)), ..Budgets::default() }, true);
    let error = capped.dispatch("run.start", &with_accept(&[2], json!({"amounts": {"USD": 1_000_000}}))).unwrap_err();
    assert!(error.message.contains("session budget"), "{}", error.message);
    assert!(started.lock().unwrap().is_empty());
}

#[test]
fn without_resources_or_a_run_service_the_commands_are_unsupported() {
    let mut bare = Session::with_registries(project(), Registry::builtins().unwrap(), kinds()).unwrap();
    assert_eq!(bare.dispatch("run.quote", &request(&[2])).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(bare.dispatch("usage.export_csv", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
    let (mut no_service, _) = session(Budgets::default(), false);
    assert!(no_service.dispatch("run.quote", &request(&[2])).is_ok());
    assert_eq!(no_service.dispatch("run.start", &with_accept(&[2], json!({"amounts": {"USD": 120_000}}))).unwrap_err().kind, ErrorKind::Unsupported);
    // Field mode needs a field; unknown fields are refused.
    assert!(no_service.dispatch("run.quote", &json!({"canvas": C, "mode": "field"})).is_err());
    assert!(no_service.dispatch("run.quote", &json!({"canvas": C, "mode": "selected", "extra": 1})).is_err());
}

#[test]
fn results_are_recorded_in_one_undo_step_and_usage_exports() {
    let (mut s, _) = session(Budgets::default(), true);
    let generation = |n: u8| json!({"id": GenerationId::from_bytes([n; 16]), "outputs": [{"type": "text", "value": "x"}]});
    let out = s
        .dispatch("run.record", &json!({"canvas": C, "results": [{"node": id(2), "generation": generation(2)}, {"node": id(3), "generation": generation(3)}]}))
        .unwrap();
    assert_eq!(out["recorded"], 2);
    let node = |s: &Session, n: u8| s.document().canvases().get(&C).unwrap().nodes().get(&id(n)).unwrap().clone();
    assert_eq!(node(&s, 2).active, Some(GenerationId::from_bytes([2; 16])));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(node(&s, 2).history.is_empty() && node(&s, 3).history.is_empty(), "one undo step for the whole run");
    // The same node twice, no results, or an unknown node: refused.
    assert!(
        s.dispatch(
            "run.record",
            &json!({"canvas": C, "results": [{"node": id(2), "generation": generation(2)}, {"node": id(2), "generation": generation(4)}]})
        )
        .is_err()
    );
    assert!(s.dispatch("run.record", &json!({"canvas": C, "results": []})).is_err());
    assert!(s.dispatch("run.record", &json!({"canvas": C, "results": [{"node": id(9), "generation": generation(9)}]})).is_err());
    let csv = s.dispatch("usage.export_csv", &json!({})).unwrap();
    assert!(csv["csv"].as_str().unwrap().starts_with("time_ms,"));
}

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
    // A free text step: text in, text out.
    kinds
        .register(Box::new(Gen(NodeDescriptor {
            id: "test.write".into(),
            label: "Write".into(),
            category: Category::Action,
            inputs: vec![Port { name: "in".into(), port_type: PortType { modality: Modality::Text, many: false } }],
            outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } }],
            params: ParamSchema::default(),
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

/// Quotes `nodes`, then builds `run.start` params accepting that quote with `amounts`.
fn start_params(s: &mut Session, nodes: &[u8], amounts: Value, partial: bool) -> Value {
    let quoted = s.dispatch("run.quote", &request(nodes)).unwrap();
    with_accept(nodes, json!({"token": quoted["token"], "amounts": amounts, "partial": partial}))
}

#[test]
fn a_run_starts_only_with_an_accepted_price_within_the_budgets() {
    let (mut s, started) = session(Budgets::default(), true);
    // No token, too little, or a partial price without saying so: refused.
    assert_eq!(s.dispatch("run.start", &with_accept(&[2], json!({"token": "x", "amounts": {"USD": 1_000_000}}))).unwrap_err().kind, ErrorKind::InvalidInput);
    let low = start_params(&mut s, &[2], json!({"USD": 100_000}), false);
    assert!(s.dispatch("run.start", &low).is_err());
    let partial = start_params(&mut s, &[2, 3], json!({"USD": 120_000}), false);
    assert!(s.dispatch("run.start", &partial).is_err(), "3 has no price");
    assert!(started.lock().unwrap().is_empty());
    // Accepted: the host service gets the run, the revision, the token and what was accepted.
    let ok = start_params(&mut s, &[2], json!({"USD": 120_000}), false);
    assert_eq!(s.dispatch("run.start", &ok).unwrap(), json!({"run": "accepted"}));
    let partial = start_params(&mut s, &[2, 3], json!({"USD": 200_000}), true);
    assert!(s.dispatch("run.start", &partial).is_ok());
    let requests = started.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["run"]["mode"], "selected");
    assert_eq!(requests[0]["revision"], json!(s.revision()));
    assert_eq!(requests[0]["accept"]["amounts"]["USD"], 120_000);
    // The host verifies the token against the document before running.
    let catalog = Catalog::from_toml(CATALOG).unwrap();
    let budget = BudgetState { budgets: Budgets::default(), spent: Spent::default() };
    let again = price(s.document(), s.revision(), s.kinds(), &catalog, &budget, &requests[0]["run"]).unwrap();
    assert_eq!(json!(again.token), requests[0]["token"]);
    // Over a session cap: refused whatever was accepted.
    let (mut capped, started) = session(Budgets { session: Some(usd(100_000)), ..Budgets::default() }, true);
    let params = start_params(&mut capped, &[2], json!({"USD": 1_000_000}), true);
    let error = capped.dispatch("run.start", &params).unwrap_err();
    assert!(error.message.contains("session budget"), "{}", error.message);
    assert!(started.lock().unwrap().is_empty());
}

#[test]
fn an_edit_after_the_quote_invalidates_it() {
    let (mut s, started) = session(Budgets::default(), true);
    let params = start_params(&mut s, &[2], json!({"USD": 1_000_000}), false);
    // Four images instead of three: the price the caller saw no longer holds.
    s.dispatch("node.param_set", &json!({"canvas": C, "id": id(2), "name": "num_images", "value": {"type": "unsigned", "value": "4"}})).unwrap();
    let error = s.dispatch("run.start", &params).unwrap_err();
    assert!(error.message.contains("quote it again"), "{}", error.message);
    assert!(started.lock().unwrap().is_empty());
}

#[test]
fn job_counts_known_only_at_run_time_need_a_partial_acceptance() {
    // A free text step (4) feeds generator 2, so 2's jobs are planned only when 4 has run.
    let (mut s, started) = session(Budgets::default(), true);
    let writer = s.kinds().get("test.write").unwrap().descriptor().instantiate(id(4), &Default::default()).unwrap();
    s.dispatch("node.add", &json!({"canvas": C, "node": writer})).unwrap();
    s.dispatch("edge.disconnect", &json!({"canvas": C, "id": EdgeId::from_bytes([10; 16])})).unwrap();
    let wire = Edge {
        id: EdgeId::from_bytes([12; 16]),
        from: Endpoint { node: id(1), port: "out".into() },
        to: Endpoint { node: id(4), port: "in".into() },
        order: 0,
    };
    s.dispatch("edge.connect", &json!({"canvas": C, "edge": wire})).unwrap();
    let wire = Edge {
        id: EdgeId::from_bytes([13; 16]),
        from: Endpoint { node: id(4), port: "out".into() },
        to: Endpoint { node: id(2), port: "in".into() },
        order: 0,
    };
    s.dispatch("edge.connect", &json!({"canvas": C, "edge": wire})).unwrap();
    let q = s.dispatch("run.quote", &request(&[4, 2])).unwrap();
    assert_eq!(q["complete"], false, "{q}");
    assert_eq!(q["estimated"], json!([id(2)]));
    let strict = start_params(&mut s, &[4, 2], json!({"USD": 1_000_000}), false);
    assert!(s.dispatch("run.start", &strict).is_err(), "one job's price was quoted for an unknown count");
    let partial = start_params(&mut s, &[4, 2], json!({"USD": 1_000_000}), true);
    assert!(s.dispatch("run.start", &partial).is_ok());
    assert_eq!(started.lock().unwrap().len(), 1);
}

#[test]
fn without_resources_or_a_run_service_the_commands_are_unsupported() {
    let mut bare = Session::with_registries(project(), Registry::builtins().unwrap(), kinds()).unwrap();
    assert_eq!(bare.dispatch("run.quote", &request(&[2])).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(bare.dispatch("usage.export_csv", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
    let (mut no_service, _) = session(Budgets::default(), false);
    assert!(no_service.dispatch("run.quote", &request(&[2])).is_ok());
    assert_eq!(
        no_service.dispatch("run.start", &with_accept(&[2], json!({"token": "x", "amounts": {"USD": 120_000}}))).unwrap_err().kind,
        ErrorKind::Unsupported
    );
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
    assert_eq!((csv["total"].clone(), csv["next_offset"].clone()), (json!(0), Value::Null));
}

#[test]
fn usage_exports_in_pages() {
    let mut log = UsageLog::default();
    for n in 0..(MAX_EXPORT_ROWS + 5) {
        let entry = nodecraft_exec::cost::UsageEntry {
            at_ms: 0,
            session: "s".into(),
            node: id(2),
            provider: "test".into(),
            model: "test.img".into(),
            cost: usd(1),
            idempotency_key: format!("k{n}"),
            status: nodecraft_exec::cost::Charge::Actual,
        };
        log.record(entry).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(UsageSnapshot(Arc::new(log)));
    let mut s = Session::with_registries(project(), Registry::builtins().unwrap(), kinds()).unwrap().with_resources(resources);
    let first = s.dispatch("usage.export_csv", &json!({})).unwrap();
    assert_eq!(first["csv"].as_str().unwrap().lines().count(), MAX_EXPORT_ROWS + 1, "header and a full page");
    assert_eq!(first["next_offset"], json!(MAX_EXPORT_ROWS));
    let rest = s.dispatch("usage.export_csv", &json!({"offset": MAX_EXPORT_ROWS})).unwrap();
    assert_eq!(rest["csv"].as_str().unwrap().lines().count(), 6);
    assert_eq!(rest["next_offset"], Value::Null);
}

#[test]
fn usage_snapshot_pagination_observes_finalized_actuals_and_conservative_budget_constructor() {
    use nodecraft_exec::cost::{Charge, UsageMetadata};
    let mut log = UsageLog::default();
    let metadata = |key| UsageMetadata { at_ms: 1, session: "s", node: id(2), provider: "test", model: "m", idempotency_key: key };
    let mut first = log.reserve_metadata(metadata("first")).unwrap();
    let mut second = log.reserve_metadata(metadata("second")).unwrap();
    let _held = log.reserve_metadata(metadata("held")).unwrap();
    log.finalize_metadata(&mut second, usd(3)).unwrap();
    log.finalize_metadata(&mut first, usd(2)).unwrap();
    assert_eq!(log.metadata_statistics().finalized, 2);
    let budget = BudgetState::from_usage(Budgets::default(), &log, "s");
    assert_eq!(budget.spent.session.get(&Currency::USD), Some(&usd(5)));
    assert_eq!(log.charge("test", "first").unwrap().status, Charge::Actual);
    let mut resources = Resources::default();
    resources.insert(UsageSnapshot(Arc::new(log)));
    let mut session = Session::with_registries(project(), Registry::builtins().unwrap(), kinds()).unwrap().with_resources(resources);
    let one = session.dispatch("usage.export_csv", &json!({"limit": 1})).unwrap();
    assert_eq!(one["total"], 2);
    assert_eq!(one["next_offset"], 1);
    assert!(one["csv"].as_str().unwrap().contains(",actual,first"));
    let two = session.dispatch("usage.export_csv", &json!({"offset": 1, "limit": 1})).unwrap();
    assert_eq!(two["next_offset"], Value::Null);
    assert!(two["csv"].as_str().unwrap().contains(",actual,second"));

    let uncertain: UsageLog = serde_json::from_str(include_str!("../../../exec/src/cost/fixtures/legacy-folded-open-reused-actual.json")).unwrap();
    let state = BudgetState::from_usage(Budgets::default(), &uncertain, "a");
    assert_eq!(state.spent.session, uncertain.budget_view("a").admission.session);
    assert_eq!(state.spent.project, uncertain.budget_view("a").admission.project);
    assert_ne!(state.spent.session, uncertain.reported_spent("a").session);
    assert_eq!(state.spent.project, uncertain.reported_spent("a").project);
}

#[test]
fn run_quote_consumes_conservative_usage_state_without_changing_reporting() {
    let uncertain: UsageLog = serde_json::from_str(include_str!("../../../exec/src/cost/fixtures/legacy-folded-open-reused-actual.json")).unwrap();
    // 120000 quoted + raw5 fits120007; conservative10 does not.
    let mut resources = Resources::default();
    resources.insert(RunCatalog(Arc::new(Catalog::from_toml(CATALOG).unwrap())));
    resources.insert(BudgetState::from_usage(Budgets { session: Some(usd(120007)), ..Budgets::default() }, &uncertain, "a"));
    let before = serde_json::to_string(&uncertain).unwrap();
    let retained = Arc::new(uncertain);
    resources.insert(UsageSnapshot(Arc::clone(&retained)));
    let mut session = Session::with_registries(project(), Registry::builtins().unwrap(), kinds()).unwrap().with_resources(resources);
    let quote = session.dispatch("run.quote", &request(&[2])).unwrap();
    assert_eq!(quote["allowed"], false, "{quote}");
    assert_eq!(serde_json::to_string(retained.as_ref()).unwrap(), before);
}

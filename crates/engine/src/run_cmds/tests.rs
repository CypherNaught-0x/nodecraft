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
    resources.insert(RunCatalog::new(Catalog::from_toml(CATALOG).unwrap()).unwrap());
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
    resources.insert(RunCatalog::new(Catalog::from_toml(CATALOG).unwrap()).unwrap());
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
    let again = price(&s, &requests[0]["run"]).unwrap();
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

fn install_run_resources(s: &mut Session, catalog: RunCatalog) {
    s.swap_resource(Some(catalog));
    s.swap_resource(Some(BudgetState { budgets: Budgets::default(), spent: Spent::default() }));
}
fn consent(token: &str) -> Value {
    with_accept(&[2], json!({"token": token, "amounts": {"USD": 120_000}, "partial": false}))
}
#[test]
fn launch_authority_rejects_copied_projects_and_equal_revision_inputs_before_service() {
    let (s, _) = session(Budgets::default(), false);
    let token = price(&s, &request(&[2])).unwrap().token;
    let catalog = s.context(crate::commands::Phase::Preview).resources.require::<RunCatalog>().unwrap().clone();
    for variant in 0..5 {
        let mut doc = project();
        if variant == 1 {
            let mut n = doc.canvases().get(&C).unwrap().nodes().get(&id(1)).unwrap().clone();
            n.params.insert("text".into(), V::Text("different prompt".into()));
            Change::SetNode { canvas: C, node: n }.apply(&mut doc).unwrap();
        }
        if variant == 2 {
            let mut n = doc.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap().clone();
            n.config.insert("runtime".into(), V::Text("different config".into()));
            Change::SetNode { canvas: C, node: n }.apply(&mut doc).unwrap();
        }
        if variant == 3 {
            let mut n = doc.canvases().get(&C).unwrap().nodes().get(&id(1)).unwrap().clone();
            let generation = Generation { id: GenerationId::from_bytes([71; 16]), outputs: vec![], input_hash: None };
            n.active = Some(generation.id);
            n.history.push(generation);
            Change::SetNode { canvas: C, node: n }.apply(&mut doc).unwrap();
        }
        if variant == 4 {
            // Same graph IDs but a different project: repeat the original292 exploit.
            let mut other = Project::new(ProjectId::from_bytes([9; 16]), "Copied").unwrap();
            Change::AddCanvas { canvas: doc.canvases().get(&C).unwrap().clone() }.apply(&mut other).unwrap();
            doc = other;
        }
        let calls = Arc::new(Mutex::new(0));
        let counted = Arc::clone(&calls);
        let mut target = Session::with_registries(doc, Registry::builtins().unwrap(), kinds()).unwrap();
        install_run_resources(&mut target, catalog.clone());
        target
            .register_host_service(RUN_SERVICE, move |_| {
                *counted.lock().unwrap() += 1;
                Ok(Value::Null)
            })
            .unwrap();
        assert_eq!(target.revision(), s.revision());
        assert_ne!(price(&target, &request(&[2])).unwrap().token, token);
        assert!(target.dispatch("run.start", &consent(&token)).is_err());
        assert_eq!(*calls.lock().unwrap(), 0);
        assert!(!target.can_undo());
    }
}
#[test]
fn canonical_request_and_catalog_snapshot_identity_are_bound() {
    let (mut s, _) = session(Budgets::default(), false);
    let original = price(&s, &request(&[2, 3])).unwrap().token;
    assert_eq!(original, price(&s, &request(&[3, 2, 2])).unwrap().token);
    let catalog = s.swap_resource::<RunCatalog>(None).unwrap();
    s.swap_resource(Some(catalog.clone()));
    assert_eq!(original, price(&s, &request(&[2, 3])).unwrap().token);
    let changed = CATALOG.replace("img-v1", "img-v2");
    s.swap_resource(Some(RunCatalog::new(Catalog::from_toml(&changed).unwrap()).unwrap()));
    let new = price(&s, &request(&[2, 3])).unwrap();
    assert_ne!(original, new.token);
    assert_eq!(new.totals.get("USD"), Some(&120_000));
    s.swap_resource(Some(RunCatalog::new(Catalog::from_toml(CATALOG).unwrap()).unwrap()));
    assert_ne!(original, price(&s, &request(&[2, 3])).unwrap().token, "new identical snapshot is new authority");
    s.swap_resource(Some(catalog));
    let mut unused = request(&[]);
    unused["mode"] = json!("all_stale");
    let a = price(&s, &unused).unwrap().token;
    unused["nodes"] = json!([id(2)]);
    unused["field"] = json!(nodecraft_types::FieldId::from_bytes([8; 16]));
    assert_eq!(a, price(&s, &unused).unwrap().token);
}
#[test]
fn imports_edits_undo_redo_and_live_budget_have_their_intended_identity_effects() {
    let (mut s, _) = session(Budgets::default(), false);
    let quote = price(&s, &request(&[2])).unwrap();
    s.import_asset(b"unreferenced cached bytes").unwrap();
    assert_eq!(s.revision(), 0);
    let imported = price(&s, &request(&[2])).unwrap().token;
    assert_ne!(quote.token, imported);
    let doc = s.document().clone();
    s.dispatch("node.move", &json!({"canvas": C, "id": id(1), "pos":{"x":25,"y":50}})).unwrap();
    let edited = price(&s, &request(&[2])).unwrap().token;
    assert_ne!(imported, edited);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &doc);
    let undone = price(&s, &request(&[2])).unwrap().token;
    assert_ne!(undone, imported);
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_ne!(undone, price(&s, &request(&[2])).unwrap().token);
    let before = price(&s, &request(&[2])).unwrap().token;
    s.swap_resource(Some(BudgetState { budgets: Budgets { project: Some(usd(1)), ..Budgets::default() }, spent: Spent::default() }));
    let blocked = price(&s, &request(&[2])).unwrap();
    assert_eq!(blocked.token, before);
    assert!(!blocked.allowed);
}
#[test]
fn queued_host_checks_actual_session_before_reserving_and_freezes_verified_inputs() {
    let (mut s, queued) = session(Budgets::default(), true);
    let params = start_params(&mut s, &[2], json!({"USD":120_000}), false);
    s.dispatch("run.start", &params).unwrap();
    let envelope = queued.lock().unwrap()[0].clone();
    let mut ledger = UsageLog::default();
    let verify = |s: &Session, ledger: &mut UsageLog| -> Result<(nodecraft_graph::Project, nodecraft_exec::planner::RunPlan, RunCatalog), Error> {
        let q = price(s, &envelope["run"])?;
        if !q.allowed || !q.complete || json!(q.token) != envelope["token"] {
            return Err(invalid("queued execution source changed"));
        }
        for (currency, cost) in &q.totals {
            if envelope["accept"]["amounts"][currency].as_u64().unwrap_or(0) < *cost {
                return Err(invalid("unaccepted price"));
            }
        }
        ledger.record(nodecraft_exec::cost::UsageEntry {
            at_ms: 0,
            session: "now".into(),
            node: id(2),
            provider: "test".into(),
            model: "test.img".into(),
            cost: usd(120_000),
            idempotency_key: "queued".into(),
            status: nodecraft_exec::cost::Charge::Estimate,
        })?;
        let canvas = s.document().canvases().get(&C).unwrap();
        let Planned::Ready(plan) = plan(canvas, s.kinds(), &RunMode::Selected, &[id(2)].into_iter().collect(), &BTreeSet::new())? else {
            return Err(invalid("verified run became blocked"));
        };
        let retained_catalog = s.context(crate::commands::Phase::Preview).resources.require::<RunCatalog>()?.clone();
        Ok((s.document().clone(), plan, retained_catalog))
    };
    let (frozen, frozen_plan, retained_catalog) = verify(&s, &mut ledger).unwrap();
    assert_eq!(ledger.entries().count(), 1);
    s.import_asset(b"host drift").unwrap();
    let mut untouched = UsageLog::default();
    assert!(verify(&s, &mut untouched).is_err());
    assert_eq!(untouched.entries().count(), 0);
    s.dispatch("node.param_set", &json!({"canvas":C,"id":id(1),"name":"text","value":V::Text("later prompt".into())})).unwrap();
    s.swap_resource(Some(RunCatalog::new(Catalog::from_toml(&CATALOG.replace("img-v1", "img-v2")).unwrap()).unwrap()));
    assert_eq!(retained_catalog.catalog().get("test.img").unwrap().provider_model, "img-v1");
    assert!(
        frozen_plan
            .steps
            .iter()
            .flat_map(|step| &step.inputs)
            .flat_map(|input| &input.from)
            .any(|source| matches!(source, nodecraft_exec::planner::InputSource::Value(V::Text(text)) if text == "a fox"))
    );
    assert_eq!(frozen.canvases().get(&C).unwrap().nodes().get(&id(1)).unwrap().params["text"], V::Text("a fox".into()));
}
#[test]
fn binding_byte_budget_and_entropy_failures_are_explicit() {
    let (s, _) = session(Budgets::default(), false);
    let c = s.context(crate::commands::Phase::Preview);
    let p: RunParams = params(&request(&[2])).unwrap();
    let catalog = c.resources.require::<RunCatalog>().unwrap();
    let exact = b"nodecraft.run.quote.v2".len() + 32 + 8 + 8 + 16 + 16 + 32 + 1 + 8 + 16;
    assert!(binding(&c, catalog, &p, exact).is_ok());
    assert!(binding(&c, catalog, &p, exact - 1).is_err());
    crate::execution_identity::without_entropy(|| {
        assert_eq!(RunCatalog::new(Catalog::from_toml(CATALOG).unwrap()).err().unwrap().kind, ErrorKind::Internal);
        assert_eq!(Session::new(project()).err().unwrap().kind, ErrorKind::Internal);
        // Existing authority needs no new entropy for quote, preview or start validation.
        assert!(price(&s, &request(&[2])).is_ok());
        assert!(s.enabled("run.quote", &request(&[2])).is_ok());
    });
}

#[derive(Default)]
struct AuthorityStorage {
    memory: nodecraft_format::MemoryStorage,
    calls: Arc<Mutex<usize>>,
}
impl nodecraft_format::ProjectStorage for AuthorityStorage {
    fn load(&mut self, location: &nodecraft_format::StorageLocation) -> Result<Vec<u8>, Error> {
        *self.calls.lock().unwrap() += 1;
        self.memory.load(location)
    }
    fn save(&mut self, location: &nodecraft_format::StorageLocation, bytes: &[u8]) -> Result<(), Error> {
        *self.calls.lock().unwrap() += 1;
        self.memory.save(location, bytes)
    }
    fn checkpoint(&mut self, bytes: &[u8]) -> Result<(), Error> {
        *self.calls.lock().unwrap() += 1;
        self.memory.checkpoint(bytes)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        *self.calls.lock().unwrap() += 1;
        self.memory.append(bytes)
    }
}
fn plain_snapshot() -> nodecraft_format::Snapshot {
    let mut doc = Project::new(ProjectId::from_bytes([2; 16]), "Plain").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut doc).unwrap();
    let text = nodecraft_nodes::Registry::builtins().unwrap().get("source.text").unwrap().descriptor().instantiate(id(1), &Default::default()).unwrap();
    Change::AddNode { canvas: C, node: text }.apply(&mut doc).unwrap();
    nodecraft_format::Snapshot { project: doc, assets: Default::default(), epoch: 0 }
}
#[test]
fn entropy_and_generation_exhaustion_precede_assets_and_all_storage_effects() {
    let calls = Arc::new(Mutex::new(0));
    crate::execution_identity::without_entropy(|| {
        let storage = AuthorityStorage { calls: Arc::clone(&calls), ..Default::default() };
        assert_eq!(Session::with_storage(plain_snapshot(), Box::new(storage)).err().unwrap().kind, ErrorKind::Internal);
    });
    assert_eq!(*calls.lock().unwrap(), 0, "entropy failure must precede initial checkpoint");
    let storage = AuthorityStorage { calls: Arc::clone(&calls), ..Default::default() };
    let mut s = Session::with_storage(plain_snapshot(), Box::new(storage)).unwrap();
    assert_eq!(*calls.lock().unwrap(), 1);
    s.exhaust_execution_generation();
    let doc = s.document().clone();
    let assets = s.assets().clone();
    let status = s.file_status();
    let before = *calls.lock().unwrap();
    for (command, args) in [
        ("node.create", json!({"canvas":C,"id":id(9),"kind":"source.text","pos":{"x":0,"y":0}})),
        ("file.save_as", json!({"location":"saved"})),
        ("file.open", json!({"location":"other","discard_dirty":true})),
    ] {
        assert_eq!(s.dispatch(command, &args).unwrap_err().kind, ErrorKind::Overflow, "{command}");
    }
    assert_eq!(s.import_asset(b"not admitted").unwrap_err().kind, ErrorKind::Overflow);
    assert_eq!(s.document(), &doc);
    assert_eq!(s.assets(), &assets);
    assert_eq!(s.file_status(), status);
    assert_eq!(s.revision(), 0);
    assert!(!s.can_undo());
    assert!(!s.can_redo());
    assert!(s.events_since(0).unwrap().is_empty());
    assert_eq!(*calls.lock().unwrap(), before, "no append/save/load/checkpoint on exhausted authority");
}
#[test]
fn successful_save_and_open_invalidate_source_without_relying_on_revision_alone() {
    let mut s = Session::with_storage(plain_snapshot(), Box::new(AuthorityStorage::default())).unwrap();
    install_run_resources(&mut s, RunCatalog::new(Catalog::from_toml(CATALOG).unwrap()).unwrap());
    let req = request(&[1]);
    let a = price(&s, &req).unwrap().token;
    s.dispatch("file.save_as", &json!({"location":"saved"})).unwrap();
    assert_eq!(s.revision(), 0);
    let b = price(&s, &req).unwrap().token;
    assert_ne!(a, b);
    s.dispatch("file.save", &json!({})).unwrap();
    let c = price(&s, &req).unwrap().token;
    assert_ne!(b, c);
    s.dispatch("file.open", &json!({"location":"saved"})).unwrap();
    assert_ne!(c, price(&s, &req).unwrap().token);
    let before_failure = price(&s, &req).unwrap().token;
    assert!(s.dispatch("file.open", &json!({"location":"missing","discard_dirty":true})).is_err());
    assert_eq!(before_failure, price(&s, &req).unwrap().token, "failed open does not revoke unchanged source");
    assert!(s.dispatch("node.move", &json!({"canvas":C,"id":id(99),"pos":{"x":1,"y":2}})).is_err());
    assert_eq!(before_failure, price(&s, &req).unwrap().token, "failed preparation does not revoke unchanged source");
}

#[test]
fn equal_descriptors_in_a_new_registry_do_not_reuse_live_authority() {
    struct Alternate(NodeDescriptor);
    impl NodeKind for Alternate {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            ModeId::new("gen").ok()
        }
        fn plan(&self, c: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
            Ok(vec![JobSpec::Source { node: c.node.id, output: "out".into(), value: V::Text("alternate implementation".into()) }])
        }
    }
    let (s, _) = session(Budgets::default(), false);
    let catalog = s.context(crate::commands::Phase::Preview).resources.require::<RunCatalog>().unwrap().clone();
    let original = kinds();
    let mut alternative = nodecraft_nodes::Registry::builtins().unwrap();
    for d in original.descriptors().filter(|d| d.id.starts_with("test.")) {
        alternative.register(Box::new(Alternate(d.clone()))).unwrap();
    }
    assert_eq!(original.descriptors().collect::<Vec<_>>(), alternative.descriptors().collect::<Vec<_>>());
    let mut other = Session::with_registries(s.document().clone(), Registry::builtins().unwrap(), alternative).unwrap();
    install_run_resources(&mut other, catalog);
    assert_ne!(price(&s, &request(&[2])).unwrap().token, price(&other, &request(&[2])).unwrap().token);
}
#[test]
fn oversized_requests_and_advisory_values_cannot_authorize_start() {
    let (mut s, calls) = session(Budgets::default(), true);
    let mut oversized = request(&[2]);
    oversized["nodes"] = json!(vec![id(2); MAX_REQUESTED + 1]);
    assert!(price(&s, &oversized).is_err());
    assert!(s.dispatch("run.quote", &oversized).is_err());
    let q = price(&s, &request(&[2])).unwrap();
    // A caller-assembled price summary without this live authority cannot launch.
    assert!(s.dispatch("run.start", &consent("120000 USD revision0 project2")).is_err());
    assert_eq!(calls.lock().unwrap().len(), 0);
    let before = q.token;
    for _ in 0..2 {
        assert!(s.enabled("run.quote", &request(&[2])).is_ok());
        assert_eq!(price(&s, &request(&[2])).unwrap().token, before);
    }
}

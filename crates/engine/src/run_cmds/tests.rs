use super::*;
use crate::Session;
use crate::commands::{Registry, Resources, Services};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Port, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, ParamControl, ParamSchema, ParamSpec, PlanCtx};
use nodecraft_types::{Currency, EdgeId, GenerationId, Modality, PortType, ProjectId, Unsigned, Value as V};
use std::sync::Mutex;

mod reasons655;

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

thread_local! {static PLAN_CALLS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
struct Gen(NodeDescriptor);
impl NodeKind for Gen {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("gen").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        PLAN_CALLS.with(|n| n.set(n.get() + 1));
        if self.0.category == Category::Action {
            return Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: V::Text("generated".into()) }]);
        }
        Ok(vec![JobSpec::Generate {
            instance: ctx.targets.get(ctx.node.id, "test").and_then(|t| t.instance()),
            node: ctx.node.id,
            provider: "test".into(),
            model: "img-v1".into(),
            params: ctx.node.params.iter().filter(|(name, _)| name.as_str() == "num_images").map(|(k, v)| (k.clone(), v.clone())).collect(),
            inputs: ctx.inputs.iter().map(|i| nodecraft_nodes::GenerateInput { role: "prompt".into(), value: i.value.clone() }).collect(),
            outputs: vec![nodecraft_nodes::GenerateOutput { port: "out".into(), modality: Modality::Image, style_family: None }],
        }])
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

type Started = Arc<Mutex<Vec<PreparedRun>>>;

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
            .register_run_host(move |request| {
                log.lock().unwrap().push(request);
                Ok(json!({"run": "accepted"}))
            })
            .unwrap();
        // Like the run host's inbox: a run handed over and not yet taken is waiting.
        let log = Arc::clone(&started);
        services.register_run_host_idle(move || log.lock().unwrap().is_empty()).unwrap();
    }
    let mut session = Session::with_registries(project(), Registry::builtins().unwrap(), kinds()).unwrap().with_resources(resources).with_services(services);
    let mut routing = crate::provider_routing::ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(CommandProvider)).unwrap();
    for n in session.document().canvases().get(&C).unwrap().nodes().keys() {
        routing.select(*n, "test", binding.target()).unwrap();
    }
    session.set_provider_routing(routing).unwrap();
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
    // Partial consent here also accepts a small count of unknown-price jobs (#366).
    let unpriced = if partial { json!(16) } else { Value::Null };
    with_accept(nodes, json!({"token": quoted["token"], "amounts": amounts, "partial": partial, "unpriced_jobs": unpriced}))
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
    assert!(s.dispatch("run.start", &partial).is_err(), "partial cannot override an unknown generation model");
    let mut requests = std::mem::take(&mut *started.lock().unwrap());
    assert_eq!(requests.len(), 1);
    assert!(matches!(requests[0].run.mode, ModeParam::Selected));
    assert_eq!(requests[0].accept.amounts.get("USD"), Some(&120_000));
    assert_eq!(price(&s, &request(&[2])).unwrap().token, requests[0].accept.token);
    let (run, progress) = s.authorize_prepared_run(requests.pop().unwrap(), run_options()).unwrap();
    let (_, job) = crate::generate::provider_job(run.work(progress.jobs[0].id).unwrap(), "fixture").unwrap();
    assert_eq!(job.model, "img-v1");
    assert_eq!(job.params.get("num_images"), Some(&json!(3)));
    // Over a session cap: refused whatever was accepted.
    let (mut capped, started) = session(Budgets { session: Some(usd(100_000)), ..Budgets::default() }, true);
    let params = start_params(&mut capped, &[2], json!({"USD": 1_000_000}), true);
    let error = capped.dispatch("run.start", &params).unwrap_err();
    assert!(error.message.contains("budget"), "{}", error.message);
    assert!(started.lock().unwrap().is_empty());
}

#[test]
fn the_accepted_amounts_become_the_runs_ceiling() {
    let (mut s, started) = session(Budgets::default(), true);
    let bad = start_params(&mut s, &[2], json!({"USD": 120_000, "usd": 5}), false);
    assert_eq!(s.dispatch("run.start", &bad).unwrap_err().kind, ErrorKind::InvalidInput, "an invalid currency is refused, not ignored");
    let ok = start_params(&mut s, &[2], json!({"USD": 150_000}), false);
    s.dispatch("run.start", &ok).unwrap();
    let prepared = started.lock().unwrap().pop().unwrap();
    let (run, progress) = s.authorize_prepared_run(prepared, run_options()).unwrap();
    assert_eq!(run.ceiling(nodecraft_types::Currency::USD), (150_000, 0));
    assert_eq!(run.ceiling(nodecraft_types::Currency::EUR), (0, 0), "nothing accepted in EUR");
    let queued = run.queued_job(progress.jobs[0].id, "fixture").unwrap();
    let price = queued.price.unwrap();
    assert_eq!(price.estimate, Some(usd(120_000)));
    assert!(price.ceiling.is_some(), "every priced job carries the run's ceiling");
}

#[test]
fn an_edit_after_the_quote_invalidates_it() {
    let (mut s, started) = session(Budgets::default(), true);
    let params = start_params(&mut s, &[2], json!({"USD": 1_000_000}), false);
    // Four images instead of three: the price the caller saw no longer holds.
    s.dispatch("node.param_set", &json!({"canvas": C, "id": id(2), "name": "num_images", "value": {"type": "unsigned", "value": "4"}})).unwrap();
    let error = s.dispatch("run.start", &params).unwrap_err();
    assert!(error.message.contains("quote again"), "{}", error.message);
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
    assert_eq!(q["resumable"], json!([]), "2 holds no partial result to finish");
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
            instance: None,
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
    let metadata = |key| UsageMetadata { instance: None, at_ms: 1, session: "s", node: id(2), provider: "test", model: "m", idempotency_key: key };
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
    resources.insert(RunCatalog::new(Catalog::from_toml(CATALOG).unwrap()).unwrap());
    resources.insert(BudgetState::from_usage(Budgets { session: Some(usd(120007)), ..Budgets::default() }, &uncertain, "a"));
    let before = serde_json::to_string(&uncertain).unwrap();
    let retained = Arc::new(uncertain);
    resources.insert(UsageSnapshot(Arc::clone(&retained)));
    let mut session = Session::with_registries(project(), Registry::builtins().unwrap(), kinds()).unwrap().with_resources(resources);
    let quote = session.dispatch("run.quote", &request(&[2])).unwrap();
    assert_eq!(quote["allowed"], false, "{quote}");
    assert_eq!(serde_json::to_string(retained.as_ref()).unwrap(), before);
}

fn hostile_usage_session() -> (Session, String) {
    let mut log = UsageLog::default();
    let text = "\"\\\n\r\t\u{0000}é".repeat(50);
    for n in 0..MAX_EXPORT_ROWS {
        log.record(nodecraft_exec::cost::UsageEntry {
            instance: None,
            at_ms: n as u64,
            session: text.clone(),
            node: id(2),
            provider: text.clone(),
            model: text.clone(),
            cost: usd(1),
            idempotency_key: format!("{n:04}{}", text),
            status: nodecraft_exec::cost::Charge::Actual,
        })
        .unwrap();
    }
    let expected = scoped_csv(&log);
    let mut resources = Resources::default();
    resources.insert(UsageSnapshot(Arc::new(log)));
    (Session::with_registries(project(), Registry::builtins().unwrap(), kinds()).unwrap().with_resources(resources), expected)
}

#[test]
fn usage_export_hostile_pages_fit_encoded_result_and_preserve_every_row() {
    let (mut session, expected) = hostile_usage_session();
    let header_end = expected.find('\n').unwrap() + 1;
    let mut joined = expected[..header_end].to_owned();
    let mut offset = 0;
    loop {
        let page = session.dispatch("usage.export_csv", &json!({"offset":offset})).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= 768 * 1024);
        crate::commands::validate_json(&page).unwrap();
        assert_eq!(page["total"], json!(MAX_EXPORT_ROWS));
        let csv = page["csv"].as_str().unwrap();
        assert!(csv.starts_with(&expected[..header_end]));
        joined.push_str(&csv[header_end..]);
        match page["next_offset"].as_u64() {
            Some(next) => {
                assert!(next > offset);
                offset = next;
            }
            None => break,
        }
    }
    assert!(joined == expected, "all scoped CSV rows must roundtrip across bounded pages");
}

#[test]
fn usage_export_zero_limit_is_rejected_by_enabled_and_dispatch() {
    let (mut session, _) = hostile_usage_session();
    let p = json!({"limit":0});
    assert_eq!(session.enabled("usage.export_csv", &p).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(session.dispatch("usage.export_csv", &p).unwrap_err().kind, ErrorKind::InvalidInput);
    let page = session.dispatch("usage.export_csv", &json!({"offset":usize::MAX})).unwrap();
    assert_eq!(page["next_offset"], Value::Null);
}

#[test]
fn csv_escape_accounting_matches_serde_for_all_controls_and_unicode() {
    let text = (0_u8..=31).map(char::from).collect::<String>() + "\"\\é🦀";
    assert_eq!(escaped_csv_bytes(text.as_bytes()).unwrap() + 2, serde_json::to_vec(&text).unwrap().len());
}

#[test]
fn csv_small_injected_ceiling_admits_exact_rows_and_never_stalls() {
    let mut log = UsageLog::default();
    for n in 0..3 {
        log.record(nodecraft_exec::cost::UsageEntry {
            instance: None,
            at_ms: n,
            session: "s".into(),
            node: id(2),
            provider: "p".into(),
            model: "m".into(),
            cost: usd(1),
            idempotency_key: format!("k{n}"),
            status: nodecraft_exec::cost::Charge::Actual,
        })
        .unwrap();
    }
    let params = ExportParams { offset: 0, limit: Some(1) };
    let mut row = Vec::new();
    log.write_scoped_csv_rows(&mut row, 0, 1).unwrap();
    let envelope = serde_json::to_vec(&json!({"csv":"", "total":3,"next_offset":usize::MAX})).unwrap().len();
    let ceiling = escaped_csv_bytes(&row).unwrap() + envelope;
    let page = csv_page(&log, &params, ceiling).unwrap();
    assert_eq!(page["csv"], String::from_utf8(row).unwrap());
    assert_eq!(page["next_offset"], 1);
    assert!(serde_json::to_vec(&page).unwrap().len() <= ceiling);
    assert_eq!(csv_page(&log, &params, ceiling - 1).unwrap_err().kind, ErrorKind::InvalidInput);
    let terminal = csv_page(&log, &ExportParams { offset: usize::MAX, limit: Some(1) }, ceiling).unwrap();
    assert_eq!(terminal["next_offset"], Value::Null);
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
            let generation = Generation {
                id: GenerationId::from_bytes([71; 16]),
                outputs: vec![],
                input_hash: None,
                items: Vec::new(),
                settings: Vec::new(),
                source_versions: Vec::new(),
                incomplete: None,
            };
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
            .register_run_host(move |_| {
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
    s.dispatch("run.start", &params).unwrap();
    let first = queued.lock().unwrap().pop().unwrap();
    let stale = queued.lock().unwrap().pop().unwrap();
    let (run, progress) = s.authorize_prepared_run(first, run_options()).unwrap();
    s.import_asset(b"host drift").unwrap();
    let untouched = UsageLog::default();
    assert!(s.authorize_prepared_run(stale, run_options()).is_err());
    assert_eq!(untouched.entries().count(), 0);
    s.dispatch("node.param_set", &json!({"canvas":C,"id":id(1),"name":"text","value":V::Text("later prompt".into())})).unwrap();
    s.swap_resource(Some(RunCatalog::new(Catalog::from_toml(&CATALOG.replace("img-v1", "img-v2")).unwrap()).unwrap()));
    let (_, job) = crate::generate::provider_job(run.work(progress.jobs[0].id).unwrap(), "queued").unwrap();
    assert_eq!(job.model, "img-v1");
    assert!(job.inputs.iter().any(|i| matches!(i,nodecraft_providers::JobInput::Text{text,..} if text=="a fox")));
}
#[test]
fn binding_byte_budget_and_entropy_failures_are_explicit() {
    let (s, _) = session(Budgets::default(), false);
    let c = s.context(crate::commands::Phase::Preview);
    let p: RunParams = params(&request(&[2])).unwrap();
    let catalog = c.resources.require::<RunCatalog>().unwrap();
    let exact = b"nodecraft.run.quote.v3".len() + 32 + 8 + 8 + 16 + 16 + 32 + 32 + 8 + 1 + 8 + 16;
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

#[test]
fn byte_bounded_command_pages_merge_actual_overlays_without_exposing_held_shells() {
    use nodecraft_exec::cost::{Charge, UsageEntry, UsageMetadata};
    let mut log = UsageLog::default();
    let heavy = "\u{1}".repeat(500);
    let add = |log: &mut UsageLog, n: usize| {
        log.record(UsageEntry {
            instance: None,
            at_ms: n as u64,
            session: heavy.clone(),
            node: id(2),
            provider: heavy.clone(),
            model: heavy.clone(),
            cost: usd(1),
            idempotency_key: format!("{n:03}{heavy}"),
            status: Charge::Actual,
        })
        .unwrap();
    };
    for n in 0..100 {
        add(&mut log, n)
    }
    let metadata = |key| UsageMetadata { instance: None, at_ms: 101, session: "s", node: id(2), provider: "overlay", model: "m", idempotency_key: key };
    let mut first = log.reserve_metadata(metadata("first-overlay")).unwrap();
    let mut second = log.reserve_metadata(metadata("second-overlay")).unwrap();
    let _held = log.reserve_metadata(metadata("held-not-listed")).unwrap();
    for n in 100..200 {
        add(&mut log, n)
    }
    log.finalize_metadata(&mut second, usd(3)).unwrap();
    log.finalize_metadata(&mut first, usd(2)).unwrap();
    assert_eq!(log.metadata_statistics().held, 1);
    assert_eq!(log.metadata_statistics().finalized, 2);
    let expected = scoped_csv(&log);
    let before = serde_json::to_string(&log).unwrap();
    let retained = Arc::new(log);
    let mut resources = Resources::default();
    resources.insert(UsageSnapshot(retained.clone()));
    let mut session = Session::with_registries(project(), Registry::builtins().unwrap(), kinds()).unwrap().with_resources(resources);
    let header_end = expected.find('\n').unwrap() + 1;
    let mut joined = expected[..header_end].to_owned();
    let mut offset = 0;
    let mut pages = 0;
    loop {
        let page = session.dispatch("usage.export_csv", &json!({"offset":offset})).unwrap();
        assert_eq!(page["total"], 202);
        assert!(serde_json::to_vec(&page).unwrap().len() <= 768 * 1024);
        let csv = page["csv"].as_str().unwrap();
        joined.push_str(&csv[header_end..]);
        pages += 1;
        match page["next_offset"].as_u64() {
            Some(next) => {
                assert!(next > offset);
                offset = next
            }
            None => break,
        }
    }
    assert!(pages > 1, "actual encoded byte ceiling must split this fixture");
    assert!(joined == expected, "all scoped CSV rows must roundtrip across bounded pages");
    assert!(!joined.contains("held-not-listed"));
    assert!(joined.find("first-overlay").unwrap() < joined.find("second-overlay").unwrap());
    assert_eq!(serde_json::to_string(retained.as_ref()).unwrap(), before);
    assert_eq!(retained.metadata_statistics().finalized, 2, "read-only export must not flush overlays");
}

fn run_options() -> nodecraft_exec::run::RunOptions {
    nodecraft_exec::run::RunOptions { id: [7; 16], first_job: 1, limits: Default::default(), retry: nodecraft_exec::scheduler::RetryPolicy::NEVER }
}
#[test]
fn typed_queue_rechecks_live_authority_and_budget_without_replanning() {
    for change in 0..3 {
        let (mut s, queued) = session(Budgets::default(), true);
        let params = start_params(&mut s, &[2], json!({"USD":120_000}), false);
        s.enabled("run.start", &params).unwrap();
        assert!(queued.lock().unwrap().is_empty());
        s.dispatch("run.start", &params).unwrap();
        let prepared = queued.lock().unwrap().pop().unwrap();
        match change {
            0 => {
                s.dispatch("node.param_set", &json!({"canvas":C,"id":id(2),"name":"num_images","value":{"type":"unsigned","value":"4"}})).unwrap();
            }
            1 => {
                s.swap_resource(Some(RunCatalog::new(Catalog::from_toml(CATALOG).unwrap()).unwrap()));
            }
            _ => {
                s.swap_resource(Some(BudgetState { budgets: Budgets { session: Some(usd(1)), ..Default::default() }, spent: Default::default() }));
            }
        }
        assert!(s.authorize_prepared_run(prepared, run_options()).is_err());
    }
    let mut services = Services::default();
    assert!(services.register(RUN_SERVICE, |_| Ok(Value::Null)).is_err());
}

#[test]
fn preview_has_zero_snapshot_copies_and_queued_authorization_has_zero_plan_replays() {
    let (mut s, queued) = session(Budgets::default(), true);
    let params = start_params(&mut s, &[2], json!({"USD":120_000}), false);
    PREPARED_COPIES.with(|n| n.set(0));
    PLAN_CALLS.with(|n| n.set(0));
    s.enabled("run.start", &params).unwrap();
    assert_eq!(PREPARED_COPIES.with(|n| n.get()), 0);
    assert_eq!(PLAN_CALLS.with(|n| n.get()), 0);
    s.dispatch("run.start", &params).unwrap();
    assert_eq!(PLAN_CALLS.with(|n| n.get()), 1);
    assert_eq!(PREPARED_COPIES.with(|n| n.get()), 1);
    let p = queued.lock().unwrap().pop().unwrap();
    s.authorize_prepared_run(p, run_options()).unwrap();
    assert_eq!(PLAN_CALLS.with(|n| n.get()), 1);
}

#[test]
fn retained_request_metadata_counts_each_owned_token_and_map_entry() {
    let value = with_accept(&[2], json!({"token":"a".repeat(64),"amounts":{"USD":20},"partial":false}));
    let base = start_metadata(&value).unwrap();
    let mut duplicate = value.clone();
    duplicate["accept"]["token"] = json!("b".repeat(128));
    assert_eq!(start_metadata(&duplicate).unwrap(), base + 64);
    duplicate["accept"]["amounts"]["EUR"] = json!(1);
    assert_eq!(start_metadata(&duplicate).unwrap(), base + 64 + std::mem::size_of::<(String, u64)>() + 48 + 3);
    let mut hostile = value;
    hostile["accept"]["token"] = json!("x".repeat(nodecraft_exec::admission::MAX_SCRATCH_BYTES));
    assert!(start_metadata(&hostile).is_err());
}

#[test]
fn unknown_model_quote_diagnostic_does_not_reflect_the_raw_selection() {
    let (mut s, _) = session(Budgets::default(), true);
    let marker = "private-model-selection-marker";
    s.dispatch("node.param_set", &json!({"canvas":C,"id":id(2),"name":"model","value":V::Text(marker.into())})).unwrap();
    let q = s.dispatch("run.quote", &request(&[2])).unwrap();
    assert!(!q.to_string().contains(marker));
    let params = start_params(&mut s, &[2], json!({"USD":1_000_000}), true);
    let error = s.dispatch("run.start", &params).unwrap_err();
    assert!(!error.message.contains(marker));
    assert!(error.message.len() < 256);
}

#[test]
fn independent_review275_all_unknown_selections_must_be_redacted() {
    let mut descriptor = kinds().get("test.gen").unwrap().descriptor().clone();
    descriptor.params.params.push(ParamSpec {
        name: "model2".into(),
        label: "Second model".into(),
        description: None,
        control: ParamControl::Model,
        required: false,
        default: None,
        visible_if: None,
    });
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    kinds.register(Box::new(Gen(descriptor))).unwrap();
    let mut document = project();
    let marker = "private-model-selection-marker";
    Change::SetParam { canvas: C, id: id(2), name: "model".into(), value: Some(V::Text(marker.into())) }.apply(&mut document).unwrap();
    let mut resources = Resources::default();
    resources.insert(RunCatalog::new(Catalog::from_toml(CATALOG).unwrap()).unwrap());
    resources.insert(BudgetState { budgets: Budgets::default(), spent: Spent::default() });
    let mut session = Session::with_registries(document, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    install_test_routing(&mut session);
    let quote = session.dispatch("run.quote", &request(&[2])).unwrap();
    println!("actual public quote: {quote}");
    assert!(!quote.to_string().contains(marker), "an unrelated later unpriced reason must not expose an earlier unknown selection");
}

fn multi_model_session(selections: &[Option<&str>]) -> (Session, Arc<Mutex<usize>>) {
    let mut descriptor = kinds().get("test.gen").unwrap().descriptor().clone();
    for index in 1..selections.len() {
        descriptor.params.params.push(ParamSpec {
            name: format!("model{index}"),
            label: "Additional model".into(),
            description: None,
            control: ParamControl::Model,
            required: false,
            default: None,
            visible_if: None,
        });
    }
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    kinds.register(Box::new(Gen(descriptor))).unwrap();
    let mut document = project();
    for (index, selected) in selections.iter().enumerate() {
        let name = if index == 0 { "model".to_owned() } else { format!("model{index}") };
        Change::SetParam { canvas: C, id: id(2), name, value: selected.map(|id| V::Text(id.into())) }.apply(&mut document).unwrap();
    }
    let mut catalog = Catalog::from_toml(CATALOG).unwrap();
    let mut free = catalog.get("test.img").unwrap().clone();
    free.id = "test.free".into();
    free.pricing = nodecraft_catalog::Pricing::Free;
    let mut usage = free.clone();
    usage.id = "test.usage".into();
    usage.pricing = nodecraft_catalog::Pricing::ByUsage { currency: Currency::USD };
    catalog.merge(Catalog::from_models([free, usage]).unwrap()).unwrap();
    let mut resources = Resources::default();
    resources.insert(RunCatalog::new(catalog).unwrap());
    resources.insert(BudgetState { budgets: Default::default(), spent: Default::default() });
    let mut session = Session::with_registries(document, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    install_test_routing(&mut session);
    let called = Arc::new(Mutex::new(0));
    let count = called.clone();
    session
        .register_run_host(move |_| {
            *count.lock().unwrap() += 1;
            Ok(Value::Null)
        })
        .unwrap();
    (session, called)
}
#[test]
fn every_unknown_model_stays_out_of_public_reports_regardless_of_later_reasons() {
    let private = "private-first-model-marker";
    let second = "private-second-model-marker";
    for selections in [
        vec![Some(private), None],
        vec![Some(private), Some(second)],
        vec![Some(private), Some("test.usage")],
        vec![Some(private), Some("test.free")],
        vec![Some("test.img"), Some(private), Some("test.usage")],
    ] {
        let (mut s, called) = multi_model_session(&selections);
        let q = s.dispatch("run.quote", &request(&[2])).unwrap();
        let text = q.to_string();
        assert!(!text.contains(private));
        assert!(!text.contains(second));
        let expected: Vec<_> = selections.iter().flatten().filter(|id| matches!(**id, "test.img" | "test.free" | "test.usage")).copied().collect();
        assert_eq!(q["steps"][0]["models"], json!(expected));
        assert!(!q["complete"].as_bool().unwrap());
        let params = with_accept(&[2], json!({"token":q["token"],"amounts":{"USD":1_000_000},"partial":true}));
        PLAN_CALLS.with(|n| n.set(0));
        let error = s.dispatch("run.start", &params).unwrap_err();
        assert!(!error.message.contains(private));
        assert!(!error.message.contains(second));
        assert!(error.message.len() < 256);
        assert_eq!(*called.lock().unwrap(), 0);
        assert_eq!(PLAN_CALLS.with(|n| n.get()), 1);
    }
}
#[test]
fn validated_multi_model_identities_sum_and_usage_partial_policy_are_preserved() {
    let (mut s, called) = multi_model_session(&[Some("test.img"), Some("test.free")]);
    let q = s.dispatch("run.quote", &request(&[2])).unwrap();
    assert_eq!(q["steps"][0]["models"], json!(["test.img", "test.free"]));
    assert_eq!(q["totals"]["USD"]["micros"], 120_000);
    assert_eq!(q["complete"], true);
    let params = with_accept(&[2], json!({"token":q["token"],"amounts":{"USD":120_000}}));
    s.dispatch("run.start", &params).unwrap();
    assert_eq!(*called.lock().unwrap(), 1);
    let (mut s, called) = multi_model_session(&[Some("test.usage"), Some("test.free")]);
    let q = s.dispatch("run.quote", &request(&[2])).unwrap();
    assert_eq!(q["steps"][0]["models"], json!(["test.usage", "test.free"]));
    assert_eq!(q["complete"], false);
    let missing = with_accept(&[2], json!({"token":q["token"],"amounts":{},"partial":true}));
    assert!(s.dispatch("run.start", &missing).unwrap_err().message.contains("unpriced_jobs"), "partial consent alone is not a count");
    let params = with_accept(&[2], json!({"token":q["token"],"amounts":{},"partial":true,"unpriced_jobs":1}));
    s.dispatch("run.start", &params).unwrap();
    assert_eq!(*called.lock().unwrap(), 1);
}

struct CommandProvider;
impl nodecraft_providers::Provider for CommandProvider {
    fn id(&self) -> &str {
        "test"
    }
    fn submit<'a>(
        &'a self,
        _: &'a nodecraft_providers::ProviderJob,
    ) -> nodecraft_providers::BoxFuture<'a, Result<nodecraft_providers::Submission, nodecraft_providers::ProviderError>> {
        Box::pin(async { Err(nodecraft_providers::ProviderError::new(nodecraft_providers::ErrorKind::UnsupportedInput, "fixture has no transport")) })
    }
    fn poll<'a>(
        &'a self,
        _: &'a nodecraft_providers::JobHandle,
    ) -> nodecraft_providers::BoxFuture<'a, Result<nodecraft_providers::JobProgress, nodecraft_providers::ProviderError>> {
        Box::pin(async { Err(nodecraft_providers::ProviderError::new(nodecraft_providers::ErrorKind::NotFound, "fixture has no handle")) })
    }
    fn cancel<'a>(&'a self, _: &'a nodecraft_providers::JobHandle) -> nodecraft_providers::BoxFuture<'a, Result<(), nodecraft_providers::ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn endpoint_authorization_captures_named_binding_and_rotation_invalidates_queued_work() {
    use crate::provider_routing::ProviderRouting;
    use nodecraft_types::ProviderInstanceId;
    let (mut s, started) = session(Budgets::default(), true);
    let mut routing = ProviderRouting::new();
    let selected = routing.bind_named(Arc::new(CommandProvider), ProviderInstanceId::from_bytes([0; 16])).unwrap();
    routing.select(id(2), "test", selected.target()).unwrap();
    s.set_provider_routing(routing).unwrap();
    PLAN_CALLS.with(|n| n.set(0));
    let params = start_params(&mut s, &[2], json!({"USD":120_000}), false);
    let quote_calls = PLAN_CALLS.with(|n| n.get());
    s.dispatch("run.start", &params).unwrap();
    assert_eq!(PLAN_CALLS.with(|n| n.get()), quote_calls + 1, "first eager hook runs exactly once during preparation");
    let prepared = started.lock().unwrap().pop().unwrap();
    // A generic resource with the same Rust type has no authority over the Session snapshot.
    s.swap_resource(Some(ProviderRouting::new()));
    let (run, progress) = s.authorize_prepared_run(prepared, run_options()).unwrap();
    assert!(run.binding(progress.jobs[0].id).unwrap().same_binding(&selected));
    let queued = run.queued_job(progress.jobs[0].id, "captured-endpoint").unwrap();
    assert!(queued.provider.same_binding(&selected));
    let params = start_params(&mut s, &[2], json!({"USD":120_000}), false);
    s.dispatch("run.start", &params).unwrap();
    let stale = started.lock().unwrap().pop().unwrap();
    let mut replacement = ProviderRouting::new();
    let new = replacement.bind_named(Arc::new(CommandProvider), ProviderInstanceId::from_bytes([0; 16])).unwrap();
    replacement.select(id(2), "test", new.target()).unwrap();
    s.set_provider_routing(replacement).unwrap();
    assert!(s.authorize_prepared_run(stale, run_options()).is_err());
    assert!(run.binding(progress.jobs[0].id).unwrap().same_binding(&selected), "active work retains the old physical binding");
    assert!(!new.same_binding(&selected));
}
#[test]
fn preparation_retains_selected_binding_only_not_large_routing_root() {
    use crate::provider_routing::ProviderRouting;
    use nodecraft_types::ProviderInstanceId;
    let (mut s, started) = session(Budgets::default(), true);
    let selected = Arc::new(CommandProvider);
    let unused = Arc::new(CommandProvider);
    let selected_weak = Arc::downgrade(&selected);
    let unused_weak = Arc::downgrade(&unused);
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(selected.clone()).unwrap();
    routing.select(id(2), "test", binding.target()).unwrap();
    drop(binding);
    for n in 0_u64..10_000 {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        routing.bind_named(unused.clone(), ProviderInstanceId::from_bytes(bytes)).unwrap();
    }
    drop(selected);
    drop(unused);
    s.set_provider_routing(routing).unwrap();
    let params = start_params(&mut s, &[2], json!({"USD":120_000}), false);
    s.dispatch("run.start", &params).unwrap();
    let prepared = started.lock().unwrap().pop().unwrap();
    s.set_provider_routing(ProviderRouting::new()).unwrap();
    assert!(unused_weak.upgrade().is_none(), "no external root view or selected binding may pin unrelated providers");
    assert!(selected_weak.upgrade().is_some());
    drop(prepared);
    assert!(selected_weak.upgrade().is_none());
}
#[test]
fn endpoint_selection_is_explicit_even_for_registered_defaults() {
    use crate::provider_routing::ProviderRouting;
    let (mut s, _) = session(Budgets::default(), true);
    let mut routing = ProviderRouting::new();
    routing.bind_default(Arc::new(CommandProvider)).unwrap();
    s.set_provider_routing(routing).unwrap();
    let quote = s.dispatch("run.quote", &request(&[2])).unwrap();
    assert_eq!(quote["allowed"], false);
    assert!(quote["blocked"].as_array().is_some_and(|b| !b.is_empty()));
    assert!(quote["blocked"][0].as_str().unwrap().contains("endpoint_missing"));
    let params = with_accept(&[2], json!({"token":quote["token"],"amounts":{"USD":120_000}}));
    assert!(s.dispatch("run.start", &params).unwrap_err().message.contains("endpoint_missing"));
}

fn scoped_csv(log: &UsageLog) -> String {
    let mut out = Vec::new();
    log.write_scoped_csv_rows(&mut out, 0, usize::MAX).unwrap();
    String::from_utf8(out).unwrap()
}
fn install_test_routing(session: &mut Session) {
    let mut routing = crate::provider_routing::ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(CommandProvider)).unwrap();
    for node in session.document().canvases().get(&C).unwrap().nodes().keys() {
        routing.select(*node, "test", binding.target()).unwrap();
    }
    session.set_provider_routing(routing).unwrap();
}
#[test]
fn scoped_csv_paging_preserves_identity_beyond_first_page() {
    use nodecraft_exec::cost::{Charge, UsageEntry};
    use nodecraft_types::ProviderInstanceId;
    let mut log = UsageLog::default();
    for n in 0..=MAX_EXPORT_ROWS {
        log.record(UsageEntry {
            at_ms: n as u64,
            session: "s".into(),
            node: id(2),
            provider: "same".into(),
            instance: if n == MAX_EXPORT_ROWS { Some(ProviderInstanceId::from_bytes([0; 16])) } else { None },
            model: "m".into(),
            cost: usd(0),
            idempotency_key: format!("k{n}"),
            status: Charge::Actual,
        })
        .unwrap();
    }
    let first = csv_page(&log, &ExportParams { offset: 0, limit: None }, MAX_EXPORT_JSON_BYTES).unwrap();
    assert_eq!(first["next_offset"], MAX_EXPORT_ROWS);
    assert!(first["csv"].as_str().unwrap().contains("provider_instance"));
    let next = csv_page(&log, &ExportParams { offset: MAX_EXPORT_ROWS, limit: None }, MAX_EXPORT_JSON_BYTES).unwrap();
    assert!(next["csv"].as_str().unwrap().contains(&ProviderInstanceId::from_bytes([0; 16]).to_string()));
    assert_eq!(next["next_offset"], Value::Null);
}

#[test]
fn selected_binding_retention_overflow_refuses_before_any_binding_clone() {
    let (s, _) = session(Budgets::default(), true);
    let c = s.context(crate::commands::Phase::Dispatch);
    let canvas = s.document().canvases().get(&C).unwrap();
    let catalog = c.resources.require::<RunCatalog>().unwrap();
    let Planned::Ready(plan) =
        nodecraft_exec::planner::plan_with_targets(canvas, s.kinds(), &RunMode::Selected, &[id(2)].into(), &Default::default(), c.routing.routing.targets())
            .unwrap()
    else {
        panic!()
    };
    let (mut admitted, _) = nodecraft_exec::admission::admit_run_with_targets(
        plan,
        C,
        canvas.clone(),
        s.kinds(),
        catalog.catalog(),
        Default::default(),
        c.routing.routing.targets(),
    )
    .unwrap();
    let remaining = admitted.remaining_metadata_bytes();
    admitted.reserve_metadata(remaining).unwrap();
    BINDING_COPIES.with(|n| n.set(0));
    assert!(capture_bindings(&mut admitted, &c.routing.routing).is_err());
    assert_eq!(BINDING_COPIES.with(|n| n.get()), 0);
}

struct CapturedProvider(Arc<std::sync::atomic::AtomicUsize>);
impl nodecraft_providers::Provider for CapturedProvider {
    fn id(&self) -> &str {
        "test"
    }
    fn submit<'a>(
        &'a self,
        _: &'a nodecraft_providers::ProviderJob,
    ) -> nodecraft_providers::BoxFuture<'a, Result<nodecraft_providers::Submission, nodecraft_providers::ProviderError>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {
            let mut done = nodecraft_providers::Completed::new(vec![]);
            done.usage.cost = Some(Money::new(Currency::USD, 20));
            Ok(nodecraft_providers::Submission::Finished(done))
        })
    }
    fn poll<'a>(
        &'a self,
        _: &'a nodecraft_providers::JobHandle,
    ) -> nodecraft_providers::BoxFuture<'a, Result<nodecraft_providers::JobProgress, nodecraft_providers::ProviderError>> {
        Box::pin(async { Err(nodecraft_providers::ProviderError::new(nodecraft_providers::ErrorKind::NotFound, "no asynchronous handle")) })
    }
    fn cancel<'a>(&'a self, _: &'a nodecraft_providers::JobHandle) -> nodecraft_providers::BoxFuture<'a, Result<(), nodecraft_providers::ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}
struct EndpointRuntime;
impl nodecraft_exec::driver::Runtime for EndpointRuntime {
    fn spawn(&self, task: nodecraft_exec::driver::TaskFuture<()>) {
        futures::executor::block_on(task);
    }
    fn sleep(&self, _: u64) -> nodecraft_exec::driver::TaskFuture<()> {
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        0
    }
}
#[test]
fn actual_authorized_run_queues_captured_endpoint_into_provider_runner_after_rotation() {
    use crate::provider_routing::ProviderRouting;
    use nodecraft_exec::driver::JobRunner;
    use nodecraft_types::ProviderInstanceId;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (mut s, started) = session(Budgets::default(), true);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_named(Arc::new(CapturedProvider(calls.clone())), ProviderInstanceId::from_bytes([0; 16])).unwrap();
    routing.select(id(2), "test", binding.target()).unwrap();
    s.set_provider_routing(routing).unwrap();
    let params = start_params(&mut s, &[2], json!({"USD":120_000}), false);
    s.dispatch("run.start", &params).unwrap();
    let prepared = started.lock().unwrap().pop().unwrap();
    let (run, progress) = s.authorize_prepared_run(prepared, run_options()).unwrap();
    s.set_provider_routing(ProviderRouting::new()).unwrap();
    let log = Arc::new(Mutex::new(UsageLog::default()));
    let budget =
        crate::jobs::JobBudget { log: log.clone(), budgets: Arc::new(Mutex::new(Budgets::default())), session: "actual".into(), wall_clock_ms: Arc::new(|| 1) };
    let delivered = Arc::new(AtomicUsize::new(0));
    let delivered_sink = delivered.clone();
    let runner = crate::jobs::ProviderRunner::new(
        Arc::new(EndpointRuntime),
        Default::default(),
        Arc::new(move |_, done| {
            assert!(matches!(done.accounting, crate::jobs::CompletionAccounting::Recorded));
            delivered_sink.fetch_add(1, Ordering::SeqCst);
        }),
        Arc::new(|_, sent, _| {
            assert_eq!(sent.target.instance(), Some(ProviderInstanceId::from_bytes([0; 16])));
        }),
    )
    .with_budget(budget);
    let spec = &progress.jobs[0];
    let queued = run.queued_job(spec.id, &spec.idempotency_key).unwrap();
    let admitted_price = queued.price.as_ref().expect("paid admission cannot become unbudgeted");
    assert_eq!(admitted_price.node, id(2));
    assert_eq!(admitted_price.model, "test.img");
    assert_eq!(admitted_price.estimate, Some(usd(120_000)));
    assert!(queued.provider.same_binding(&binding));
    runner.register(spec.id, queued);
    let outcome = futures::executor::block_on(runner.run(
        nodecraft_exec::scheduler::Start { job: spec.id, lane: spec.lane.clone(), attempt: 1, idempotency_key: spec.idempotency_key.clone() },
        Default::default(),
    ));
    assert_eq!(outcome, nodecraft_exec::scheduler::Outcome::Done);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(delivered.load(Ordering::SeqCst), 1);
    let log = log.lock().unwrap();
    let charge = log.charge_target(binding.target(), &spec.idempotency_key).unwrap();
    assert_eq!(charge.cost, usd(20));
    assert_eq!(charge.instance, binding.target().instance());
}

struct SelectionSource(NodeDescriptor, Arc<std::sync::atomic::AtomicUsize>);
impl NodeKind for SelectionSource {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("source").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        assert_eq!(ctx.targets.get(id(2), "test").and_then(|t| t.instance()), Some(nodecraft_types::ProviderInstanceId::from_bytes([0; 16])));
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: V::Text("target-before-source".into()) }])
    }
}
#[test]
fn first_inline_source_hook_sees_authoritative_selection_without_preparation_replay() {
    use crate::provider_routing::ProviderRouting;
    use nodecraft_types::ProviderInstanceId;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let descriptor = NodeDescriptor {
        id: "fixture.selected_source".into(),
        label: "Source".into(),
        category: Category::Source,
        inputs: vec![],
        outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } }],
        params: Default::default(),
        quick_add: None,
    };
    let source = descriptor.instantiate(id(1), &Default::default()).unwrap();
    let mut kinds = kinds();
    kinds.register(Box::new(SelectionSource(descriptor, calls.clone()))).unwrap();
    let mut document = project();
    Change::SetNode { canvas: C, node: source }.apply(&mut document).unwrap();
    let mut s = Session::with_registries(document, Registry::builtins().unwrap(), kinds).unwrap();
    install_run_resources(&mut s, RunCatalog::new(Catalog::from_toml(CATALOG).unwrap()).unwrap());
    let started: Started = Arc::default();
    let queue = started.clone();
    s.register_run_host(move |p| {
        queue.lock().unwrap().push(p);
        Ok(Value::Null)
    })
    .unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_named(Arc::new(CommandProvider), ProviderInstanceId::from_bytes([0; 16])).unwrap();
    routing.select(id(2), "test", binding.target()).unwrap();
    s.set_provider_routing(routing).unwrap();
    let params = start_params(&mut s, &[2], json!({"USD":120_000}), false);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    s.dispatch("run.start", &params).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let prepared = started.lock().unwrap().pop().unwrap();
    let (run, progress) = s.authorize_prepared_run(prepared, run_options()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let (_, job) = crate::generate::provider_job(run.work(progress.jobs[0].id).unwrap(), "source-selection").unwrap();
    assert!(job.inputs.iter().any(|i| matches!(i,nodecraft_providers::JobInput::Text {text,..} if text=="target-before-source")));
}

#[test]
fn authorized_quote_preserves_known_unknown_and_free_prices_without_host_input() {
    for pricing in [nodecraft_catalog::Pricing::Free, nodecraft_catalog::Pricing::ByUsage { currency: Currency::USD }] {
        let (session, started) = session(Budgets::default(), true);
        let mut model = Catalog::from_toml(CATALOG).unwrap().get("test.img").unwrap().clone();
        model.pricing = pricing.clone();
        let mut resources = Resources::default();
        resources.insert(RunCatalog::new(Catalog::from_models([model]).unwrap()).unwrap());
        resources.insert(BudgetState { budgets: Default::default(), spent: Default::default() });
        let mut s = session.with_resources(resources);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut routing = crate::provider_routing::ProviderRouting::new();
        let binding = routing.bind_default(Arc::new(CapturedProvider(calls.clone()))).unwrap();
        routing.select(id(2), "test", binding.target()).unwrap();
        s.set_provider_routing(routing).unwrap();
        let params = start_params(&mut s, &[2], json!({}), true);
        s.dispatch("run.start", &params).unwrap();
        let prepared = started.lock().unwrap().pop().unwrap();
        let (run, progress) = s.authorize_prepared_run(prepared, run_options()).unwrap();
        let spec = &progress.jobs[0];
        let queued = run.queued_job(spec.id, &spec.idempotency_key).unwrap();
        if pricing == nodecraft_catalog::Pricing::Free {
            assert!(queued.price.is_none());
        } else {
            let price = queued.price.as_ref().unwrap();
            assert_eq!(price.node, id(2));
            assert_eq!(price.model, "test.img");
            assert_eq!(price.estimate, None, "unknown paid work must stay budgeted");
        }
        assert!(run.queued_job(nodecraft_exec::scheduler::JobId(u64::MAX), "missing").is_err());
        let runner = crate::jobs::ProviderRunner::new(Arc::new(EndpointRuntime), Default::default(), Arc::new(|_, _| {}), Arc::new(|_, _, _| {})).with_budget(
            crate::jobs::JobBudget {
                log: Arc::new(Mutex::new(UsageLog::default())),
                budgets: Arc::new(Mutex::new(Budgets { per_call: Some(usd(0)), ..Default::default() })),
                session: "price-bound".into(),
                wall_clock_ms: Arc::new(|| 0),
            },
        );
        runner.register(spec.id, queued);
        use nodecraft_exec::driver::JobRunner;
        let outcome = futures::executor::block_on(runner.run(
            nodecraft_exec::scheduler::Start { job: spec.id, lane: spec.lane.clone(), attempt: 1, idempotency_key: spec.idempotency_key.clone() },
            Default::default(),
        ));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), usize::from(pricing == nodecraft_catalog::Pricing::Free));
        assert_eq!(matches!(outcome, nodecraft_exec::scheduler::Outcome::Done), pricing == nodecraft_catalog::Pricing::Free);
    }
}

#[test]
fn bounded_static_blockers_preserve_action_and_never_echo_native_text() {
    use nodecraft_exec::planner::{Blocker, Reason, TargetReason};
    let blockers: Vec<_> =
        (0..101).map(|_| Blocker { node: id(2), port: None, upstream: None, reason: Reason::Invalid("private-hook-secret".repeat(1000)) }).collect();
    let output = blocked_reasons(&blockers);
    assert_eq!(output.len(), 100, "omission trailer stays inside the existing hundred-record cap");
    assert!(output.last().unwrap().contains("2 additional"));
    let text = output.join("; ");
    assert!(text.len() <= BLOCKED_BYTES);
    assert!(!text.contains("private-hook"));
    assert!(text.contains("node_validation"));
    for reason in [TargetReason::MissingSelection, TargetReason::Mismatch] {
        let text = blocked_reasons(&[Blocker { node: id(2), port: None, upstream: None, reason: Reason::ProviderTarget(reason) }]);
        assert!(text[0].contains(if reason == TargetReason::MissingSelection { "endpoint_missing" } else { "endpoint_mismatch" }));
    }
    assert!(admission_reason(&Error::new(ErrorKind::Budget, "private-error")).contains("admission_limit"));
    assert!(admission_reason(&Error::new(ErrorKind::InvalidInput, "private-error")).contains("catalog_mismatch"));
}

// Cached results a run reuses (#91).

fn node_of(s: &Session, n: u8) -> nodecraft_graph::Node {
    s.document().canvases().get(&C).unwrap().nodes().get(&id(n)).unwrap().clone()
}

/// Records a result for node `n` made from its inputs as they are now.
fn record_current(s: &mut Session, n: u8, generation: u8) {
    let kinds = kinds();
    let canvas = s.document().canvases().get(&C).unwrap();
    let hash = nodecraft_exec::staleness::InputHasher::new(canvas, &kinds, nodecraft_exec::staleness::MAX_QUERY_BYTES)
        .unwrap()
        .hash(canvas, &kinds, id(n), &BTreeMap::new())
        .unwrap()
        .unwrap();
    let generation = Generation {
        id: GenerationId::from_bytes([generation; 16]),
        outputs: vec![V::Text(format!("result {generation}"))],
        input_hash: Some(hash),
        items: Vec::new(),
        settings: Vec::new(),
        incomplete: None,
    };
    s.dispatch("run.record", &json!({"canvas": C, "results": [{"node": id(n), "generation": generation}]})).unwrap();
}

fn text(s: &mut Session, value: &str) {
    s.dispatch("node.param_set", &json!({"canvas": C, "id": id(1), "name": "text", "value": V::Text(value.into())})).unwrap();
}

fn images(s: &mut Session, n: u8) {
    s.dispatch("node.param_set", &json!({"canvas": C, "id": id(2), "name": "num_images", "value": {"type": "unsigned", "value": n.to_string()}})).unwrap();
}

/// Text 1 → write 4 → generator 2: 4 holds a result for the current text in its history, but
/// another one is active.
fn chain_with_a_cached_write() -> (Session, Started) {
    let (mut s, started) = session(Budgets::default(), true);
    s.dispatch("node.create", &json!({"canvas": C, "id": id(4), "kind": "test.write", "pos": {"x": 0, "y": 0}})).unwrap();
    s.dispatch("edge.disconnect", &json!({"canvas": C, "id": EdgeId::from_bytes([10; 16])})).unwrap();
    let wire = |s: &mut Session, e: u8, from: u8, to: u8| {
        s.dispatch(
            "edge.connect",
            &json!({"canvas": C, "edge": {"id": EdgeId::from_bytes([e; 16]), "from": {"node": id(from), "port": "out"}, "to": {"node": id(to), "port": "in"}, "order": 0}}),
        )
        .unwrap();
    };
    wire(&mut s, 12, 1, 4);
    wire(&mut s, 13, 4, 2);
    record_current(&mut s, 4, 40);
    text(&mut s, "a wolf");
    record_current(&mut s, 4, 41);
    text(&mut s, "a fox");
    (s, started)
}

#[test]
fn a_node_whose_result_is_cached_is_neither_quoted_nor_run() {
    let (mut s, started) = session(Budgets::default(), true);
    s.dispatch("node.cache_set", &json!({"canvas": C, "id": id(2), "enabled": true})).unwrap();
    record_current(&mut s, 2, 20);
    images(&mut s, 4);
    record_current(&mut s, 2, 21);
    images(&mut s, 3);
    let revision = s.revision();
    let quote = s.dispatch("run.quote", &request(&[2])).unwrap();
    assert_eq!(quote["reused"], json!([id(2)]));
    assert_eq!((quote["steps"].clone(), quote["totals"].clone(), quote["allowed"].clone()), (json!([]), json!({}), json!(true)), "nothing to pay for");
    assert_eq!(s.revision(), revision, "a quote changes nothing");
    // Starting it only applies the cached result: no run goes to the host.
    let out = s.dispatch("run.start", &with_accept(&[2], json!({"token": quote["token"]}))).unwrap();
    assert_eq!(out, json!({"reused": [id(2)]}));
    assert!(started.lock().unwrap().is_empty());
    assert_eq!(node_of(&s, 2).active, Some(GenerationId::from_bytes([20; 16])), "the earlier result, selected");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(node_of(&s, 2).active, Some(GenerationId::from_bytes([21; 16])), "one undo step");
    // With caching off (the default for a model), the same request is quoted and paid for.
    s.dispatch("node.cache_set", &json!({"canvas": C, "id": id(2), "enabled": null})).unwrap();
    let quote = s.dispatch("run.quote", &request(&[2])).unwrap();
    assert_eq!((quote["reused"].clone(), quote["steps_total"].clone()), (json!([]), json!(1)));
}

#[test]
fn a_run_applies_cached_results_first_and_runs_only_the_rest() {
    let (mut s, started) = chain_with_a_cached_write();
    let upstream = json!({"canvas": C, "mode": "with_upstream", "nodes": [id(2)]});
    let quote = s.dispatch("run.quote", &upstream).unwrap();
    assert_eq!(quote["reused"], json!([id(4)]));
    let steps: Vec<Value> = quote["steps"].as_array().unwrap().iter().map(|step| step["node"].clone()).collect();
    assert_eq!(steps, vec![json!(id(2))], "only the generator runs");
    let revision = s.revision();
    let mut start = upstream.clone();
    start["accept"] = json!({"token": quote["token"], "amounts": quote["totals"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v["micros"].clone())).collect::<Map<_, _>>()});
    let out = s.dispatch("run.start", &start).unwrap();
    assert_eq!(out, json!({"run": "accepted", "reused": [id(4)]}));
    assert_eq!(s.revision(), revision + 1, "the reuse is one edit");
    assert_eq!(node_of(&s, 4).active, Some(GenerationId::from_bytes([40; 16])));
    // The run was bound to the document with the reused result in place: it authorizes, and
    // the generator reads the reused text.
    let prepared = started.lock().unwrap().pop().unwrap();
    let (run, progress) = s.authorize_prepared_run(prepared, run_options()).unwrap();
    let (_, job) = crate::generate::provider_job(run.work(progress.jobs[0].id).unwrap(), "fixture").unwrap();
    assert!(format!("{:?}", job.inputs).contains("result 40"), "{:?}", job.inputs);
}

#[test]
fn a_run_bound_after_its_reuse_is_still_refused_after_another_edit() {
    let (mut s, started) = chain_with_a_cached_write();
    let upstream = json!({"canvas": C, "mode": "with_upstream", "nodes": [id(2)]});
    let quote = s.dispatch("run.quote", &upstream).unwrap();
    let mut start = upstream.clone();
    start["accept"] = json!({"token": quote["token"], "amounts": {"USD": 1_000_000}});
    s.dispatch("run.start", &start).unwrap();
    images(&mut s, 4);
    let prepared = started.lock().unwrap().pop().unwrap();
    let error = s.authorize_prepared_run(prepared, run_options()).err().unwrap();
    assert!(error.message.contains("stale"), "{}", error.message);
}

#[test]
fn a_node_fed_by_a_step_that_runs_never_reuses() {
    let (mut s, _) = chain_with_a_cached_write();
    // The generator is cached too, but for what the write had before: once the write's text
    // changes and it runs again, the generator's cached result would be stale.
    s.dispatch("node.cache_set", &json!({"canvas": C, "id": id(2), "enabled": true})).unwrap();
    s.dispatch("node.cache_set", &json!({"canvas": C, "id": id(4), "enabled": false})).unwrap();
    record_current(&mut s, 2, 22);
    images(&mut s, 4);
    record_current(&mut s, 2, 23);
    images(&mut s, 3);
    // 4 is stale (text changed back to one its active result wasn't made from) and runs.
    let quote = s.dispatch("run.quote", &json!({"canvas": C, "mode": "with_upstream", "nodes": [id(2)]})).unwrap();
    assert_eq!(quote["reused"], json!([]));
    assert_eq!(quote["steps_total"], json!(2));
}

#[test]
fn a_cached_node_already_holding_its_result_is_not_run_again() {
    let (mut s, started) = session(Budgets::default(), true);
    s.dispatch("node.cache_set", &json!({"canvas": C, "id": id(2), "enabled": true})).unwrap();
    record_current(&mut s, 2, 25);
    let quote = s.dispatch("run.quote", &request(&[2])).unwrap();
    assert_eq!((quote["reused"].clone(), quote["steps"].clone()), (json!([id(2)]), json!([])));
    let revision = s.revision();
    let out = s.dispatch("run.start", &with_accept(&[2], json!({"token": quote["token"]}))).unwrap();
    assert_eq!(out, json!({"reused": [id(2)]}));
    assert_eq!(s.revision(), revision, "nothing to apply");
    assert!(started.lock().unwrap().is_empty());
    // A malformed acceptance is still refused when nothing runs.
    let bad = s.dispatch("run.start", &with_accept(&[2], json!({"token": quote["token"], "amounts": {"usd": 1}})));
    assert!(bad.is_err());
}

#[test]
fn a_requested_node_made_current_by_a_reused_upstream_result_is_not_run() {
    let (mut s, started) = chain_with_a_cached_write();
    let select = |s: &mut Session, g: u8| {
        s.dispatch("history.select", &json!({"canvas": C, "id": id(4), "generation": GenerationId::from_bytes([g; 16])})).unwrap();
    };
    // The generator has a result made from 40, the write's cached result for the current text.
    select(&mut s, 40);
    s.dispatch("node.cache_set", &json!({"canvas": C, "id": id(2), "enabled": true})).unwrap();
    record_current(&mut s, 2, 24);
    select(&mut s, 41);
    let upstream = json!({"canvas": C, "mode": "with_upstream", "nodes": [id(2)]});
    let quote = s.dispatch("run.quote", &upstream).unwrap();
    assert_eq!((quote["reused"].clone(), quote["steps"].clone()), (json!([id(2), id(4)]), json!([])));
    let mut start = upstream.clone();
    start["accept"] = json!({"token": quote["token"]});
    assert_eq!(s.dispatch("run.start", &start).unwrap(), json!({"reused": [id(2), id(4)]}));
    assert_eq!((node_of(&s, 4).active, node_of(&s, 2).active), (Some(GenerationId::from_bytes([40; 16])), Some(GenerationId::from_bytes([24; 16]))));
    assert!(started.lock().unwrap().is_empty());
}

#[test]
fn a_run_that_reuses_waits_while_another_accepted_run_has_not_started() {
    let (mut s, started) = chain_with_a_cached_write();
    // A run with nothing to reuse is accepted and waits in the host.
    let plain = s.dispatch("run.quote", &request(&[2])).unwrap();
    assert_eq!(plain["reused"], json!([]));
    s.dispatch("run.start", &with_accept(&[2], json!({"token": plain["token"], "amounts": {"USD": 1_000_000}}))).unwrap();
    // One that applies a reused result first would leave the waiting run bound to a document
    // that is gone: it is refused, and changes nothing.
    let upstream = json!({"canvas": C, "mode": "with_upstream", "nodes": [id(2)]});
    let quote = s.dispatch("run.quote", &upstream).unwrap();
    assert_eq!(quote["reused"], json!([id(4)]));
    let mut start = upstream.clone();
    start["accept"] = json!({"token": quote["token"], "amounts": {"USD": 1_000_000}});
    let revision = s.revision();
    let error = s.dispatch("run.start", &start).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Busy);
    assert_eq!(s.revision(), revision);
    let waiting = started.lock().unwrap().pop().unwrap();
    s.authorize_prepared_run(waiting, run_options()).unwrap();
}

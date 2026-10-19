//! Real typed Session admission and asynchronous Driver composition; no serial runner fallback.
use nodecraft_engine::{Session, commands::Registry};
use nodecraft_exec::scheduler::JobId;
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Project};
use nodecraft_providers::mock::{MockModel, MockOutput};
use nodecraft_testkit::{driver_golden::*, kinds};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Value};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
fn session() -> Session {
    session_many(1, false, "mock-text")
}
fn session_many(count: u8, action: bool, model: &str) -> Session {
    let mut kinds = kinds::registry().unwrap();
    if action {
        kinds.register(Box::new(ActionSource(source_descriptor()))).unwrap();
    }
    let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Driver fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    let mut changes = Vec::new();
    for n in 1..=count + 1 {
        let kind = if n == 1 { if action { "driver.source" } else { "source.text" } } else { "mock.text" };
        let params = if n == 1 {
            [("text".into(), Value::Text("public fox".into()))].into_iter().collect()
        } else {
            [("model".into(), Value::Text(model.into()))].into_iter().collect()
        };
        let mut node = kinds.get(kind).unwrap().instantiate_instance(id(n), Default::default(), &params).unwrap();
        node.title = if n == 1 { "Text".into() } else { "Mock text".into() };
        changes.push(Change::AddNode { canvas: C, node });
    }
    for n in 2..=count + 1 {
        changes.push(Change::Connect {
            canvas: C,
            edge: Edge {
                id: EdgeId::from_bytes([n; 16]),
                from: Endpoint { node: id(1), port: "out".into() },
                to: Endpoint { node: id(n), port: "in".into() },
                order: 0,
            },
        });
    }
    Changeset::new(changes).apply(&mut p).unwrap();
    Session::with_registries(p, Registry::builtins().unwrap(), kinds).unwrap()
}
fn fixture(polls: u32) -> DriverFixture {
    let mut model = MockModel::new(MockOutput::Text);
    model.polls = polls;
    DriverFixture {
        selections: vec![(id(2), 0)],
        adapters: vec![AdapterFixture {
            id: "mock".into(),
            instance: None,
            models: vec![("mock-text".into(), model)],
            operations: vec![],
            submit_open: true,
            poll_open: true,
            synthetic: None,
        }],
        ..Default::default()
    }
}
fn request() -> DriverRequest {
    DriverRequest::selected(C, vec![id(2)])
}
#[test]
fn typed_source_and_eager_or_deferred_generation_records_one_undo() {
    for polls in [0, 2] {
        let s = session();
        let before = s.document().clone();
        let mut out = run_driver(s, request(), fixture(polls)).unwrap();
        assert!(out.cleanup_complete);
        assert!(out.recorded);
        assert_eq!(out.record_dispatches, 1);
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.deliveries.len(), 1);
        assert!(out.jobs.iter().all(|j| j.notified && j.terminal == Some("done")));
        assert_eq!(out.adapters[0].counters().submits, 1);
        assert_eq!(out.adapters[0].counters().polls, polls as usize);
        let requests = out.adapters[0].requests().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].model, "mock-text");
        assert!(matches!(&requests[0].inputs[0],nodecraft_providers::JobInput::Text{text,..}if text=="public fox"));
        let recorded = out.session.document().clone();
        out.session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
        assert_eq!(out.session.document(), &before);
        out.session.dispatch("edit.redo", &serde_json::Value::Null).unwrap();
        assert_eq!(out.session.document(), &recorded);
    }
}
#[test]
fn closed_gate_is_explicit_fault_and_cleanup_owns_outputs() {
    let mut f = fixture(0);
    f.adapters[0].submit_open = false;
    let err = run_driver(session(), request(), f).err().unwrap();
    assert_eq!(err.code, "stalled_gate");
    assert!(err.outcome.cleanup_complete);
    assert_eq!(err.outcome.adapters[0].counters().submits, 0);
}
#[test]
fn timed_gate_and_partial_are_actual_async_updates() {
    let mut f = fixture(1);
    f.adapters[0].poll_open = false;
    f.adapters[0].synthetic = Some(SyntheticFixture { outputs: None, partial_keys: vec![BASE.into()] });
    f.actions.push(Action { trigger: Trigger::At(10), kind: ActionKind::PollGate(0) });
    let out = run_driver(session(), request(), f).unwrap();
    assert!(out.trace.iter().any(|t| t.kind == "partial"));
    assert!(out.recorded);
}
#[test]
fn stale_authority_refuses_before_provider() {
    for change in [AuthorityChange::Source, AuthorityChange::Catalog, AuthorityChange::Routing] {
        let mut f = fixture(0);
        f.before_authorize = change;
        let e = run_driver(session(), request(), f).err().unwrap();
        assert_eq!(e.code, "authorize");
        assert_eq!(e.outcome.adapters[0].counters().submits, 0);
    }
}
#[test]
fn captured_binding_survives_registry_replacement() {
    let mut f = fixture(0);
    f.replace_routing_after_authorize = true;
    assert!(run_driver(session(), request(), f).unwrap().recorded);
}
#[test]
fn conversion_and_record_refusals_keep_paid_original() {
    for cap in [true, false] {
        let mut f = fixture(0);
        if cap {
            f.conversion_cap = 0;
        } else {
            f.record_cap = 0;
        }
        let e = run_driver(session(), request(), f).err().unwrap();
        assert_eq!(e.outcome.deliveries.len(), 1);
        assert!(!e.outcome.deliveries[0].delivery.completed.outputs.is_empty());
        assert!(!e.outcome.recorded);
        assert_eq!(e.outcome.record_dispatches, 0);
    }
}
#[test]
fn actual_record_rejection_keeps_document_and_delivery() {
    let s = session();
    let before = s.document().clone();
    let mut f = fixture(0);
    f.invalid_record = true;
    let e = run_driver(s, request(), f).err().unwrap();
    assert_eq!(e.code, "record_dispatch");
    assert_eq!(e.outcome.record_dispatches, 1);
    assert_eq!(e.outcome.session.document(), &before);
    assert_eq!(e.outcome.deliveries.len(), 1);
}
#[test]
fn queued_cancel_runs_through_handle_and_notifies_run() {
    let mut f = fixture(1);
    f.scheduler.global = 1;
    f.actions.push(Action { trigger: Trigger::At(0), kind: ActionKind::Pause(true) });
    f.actions.push(Action { trigger: Trigger::At(0), kind: ActionKind::Cancel(vec![JobId(1)]) });
    let out = run_driver(session(), request(), f).unwrap();
    assert!(out.jobs.iter().all(|j| j.notified));
    assert_eq!(out.adapters[0].counters().submits, 0);
}

const BASE: &str = "nodecraft:70707070707070707070707070707070:1";
fn scripted(script: nodecraft_providers::mock::MockScript, polls: u32, generations: u32) -> DriverFixture {
    let mut f = fixture(polls);
    f.adapters[0].operations.push(nodecraft_providers::mock::MockOperation {
        id: "operation".into(),
        model: "mock-text".into(),
        sent_keys: (0..=generations).map(|g| nodecraft_engine::jobs::sent_key(BASE, g)).collect(),
        script,
    });
    f
}
#[test]
fn finite_submit_failure_recovers_or_exhausts_exact_attempts() {
    use nodecraft_providers::{
        ErrorKind,
        mock::{MockFailures, MockScript},
    };
    for count in [2, 4] {
        let f = scripted(MockScript { submit_failures: Some(MockFailures { count, kind: ErrorKind::Network }), ..Default::default() }, 0, 0);
        let out = run_driver(session(), request(), f).unwrap();
        assert_eq!(out.adapters[0].counters().submits, (count + u32::from(count < 4)) as usize);
        assert_eq!(out.jobs[0].starts.len(), (count + u32::from(count < 4)) as usize);
        assert_eq!(out.recorded, count < 4);
        let requests = out.adapters[0].requests().unwrap();
        assert!(requests.iter().all(|r| r.idempotency_key == BASE));
        assert_eq!(out.jobs[0].terminal, Some(if count < 4 { "done" } else { "failed" }));
    }
}
#[test]
fn lost_reply_and_poll_failure_preserve_identity_and_single_acceptance() {
    use nodecraft_providers::{
        ErrorKind,
        mock::{MockFailures, MockScript},
    };
    for script in [
        MockScript { lost_replies: Some(MockFailures { count: 1, kind: ErrorKind::Network }), ..Default::default() },
        MockScript { poll_failures: Some(MockFailures { count: 1, kind: ErrorKind::Network }), ..Default::default() },
    ] {
        let out = run_driver(session(), request(), scripted(script, 1, 0)).unwrap();
        assert!(out.recorded);
        assert_eq!(out.deliveries.len(), 1);
        assert!(out.adapters[0].requests().unwrap().iter().all(|r| r.idempotency_key == BASE));
        assert!(out.trace.iter().filter(|t| t.key.is_some()).all(|t| t.key.as_deref() == Some(BASE)));
    }
}
#[test]
fn actual_terminal_generation_failure_uses_jobs_sent_key() {
    use nodecraft_providers::{
        ErrorKind,
        mock::{MockFailures, MockScript},
    };
    let out = run_driver(
        session(),
        request(),
        scripted(MockScript { failed_generations: Some(MockFailures { count: 1, kind: ErrorKind::Network }), ..Default::default() }, 1, 1),
    )
    .unwrap();
    assert!(out.recorded);
    let requests = out.adapters[0].requests().unwrap();
    assert_eq!(requests.iter().map(|r| r.idempotency_key.as_str()).collect::<Vec<_>>(), vec![BASE, nodecraft_engine::jobs::sent_key(BASE, 1).as_str()]);
    assert_eq!(out.jobs[0].starts, vec![1, 2]);
    assert_eq!(out.deliveries.len(), 1);
}

struct ActionSource(nodecraft_nodes::NodeDescriptor);
fn source_descriptor() -> nodecraft_nodes::NodeDescriptor {
    let mut d = nodecraft_nodes::Registry::builtins().unwrap().get("source.text").unwrap().descriptor().clone();
    d.id = "driver.source".into();
    d.category = nodecraft_nodes::Category::Action;
    d.quick_add = None;
    d
}
impl nodecraft_nodes::NodeKind for ActionSource {
    fn descriptor(&self) -> &nodecraft_nodes::NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[nodecraft_types::PortType]) -> Option<nodecraft_nodes::ModeId> {
        nodecraft_nodes::ModeId::new("generate").ok()
    }
    fn plan(&self, c: &nodecraft_nodes::PlanCtx<'_>) -> Result<Vec<nodecraft_nodes::JobSpec>, nodecraft_types::Error> {
        Ok(vec![nodecraft_nodes::JobSpec::Source { node: c.node.id, output: "out".into(), value: Value::Text("public fox".into()) }])
    }
}
#[test]
fn source_job_uses_driver_source_runner_and_taken_outputs() {
    let out = run_driver(session_many(1, true, "mock-text"), request(), fixture(0)).unwrap();
    assert_eq!(out.jobs.len(), 2);
    assert!(!out.jobs[0].provider);
    assert!(out.jobs[1].provider);
    assert_eq!(out.results.len(), 2);
    assert!(out.jobs.iter().all(|j| j.notified));
    assert_eq!(out.adapters[0].counters().submits, 1);
}
#[test]
fn real_global_and_target_lane_limits_constrain_live_overlap() {
    for (global, lane, expected) in [(1, 4, 1), (4, 1, 1), (4, 2, 2)] {
        let mut f = fixture(2);
        f.adapters[0].poll_open = false;
        f.scheduler.global = global;
        f.scheduler.default_per_lane = lane;
        f.selections = vec![(id(2), 0), (id(3), 0), (id(4), 0)];
        f.actions.push(Action { trigger: Trigger::At(10), kind: ActionKind::PollGate(0) });
        let out = run_driver(session_many(3, false, "mock-text"), DriverRequest::selected(C, vec![id(2), id(3), id(4)]), f).unwrap();
        assert_eq!(out.peak_global, expected);
        assert!(out.peak_lanes.values().all(|n| *n <= lane));
        assert_eq!(out.deliveries.len(), 3);
    }
}
#[test]
fn accepted_handle_cancel_and_completion_races_keep_real_terminal() {
    for cancel_first in [true, false] {
        let mut f = fixture(1);
        f.adapters[0].poll_open = false;
        let trigger = Trigger::Update(JobId(1), "submitted");
        let cancel = Action { trigger: trigger.clone(), kind: ActionKind::Cancel(vec![JobId(1)]) };
        let open = Action { trigger, kind: ActionKind::PollGate(0) };
        if cancel_first {
            f.actions.extend([cancel, open]);
        } else {
            f.actions.extend([open, cancel]);
        }
        let out = run_driver(session(), request(), f).unwrap();
        assert!(matches!(out.jobs[0].terminal, Some("done" | "cancelled")));
        assert!(out.jobs[0].notified);
        assert!(out.cleanup_complete);
        assert!(out.deliveries.len() <= 1);
        assert_eq!(out.adapters[0].counters().submits, 1);
        if out.jobs[0].terminal == Some("cancelled") {
            assert!(!out.recorded);
        }
    }
}
#[test]
fn accounting_failure_keeps_capability_and_repairs_without_run_revival() {
    let mut f = fixture(1);
    f.adapters[0].models[0].0 = "mock-usage".into();
    f.adapters[0].models[0].1.cost = Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 7));
    f.adapters[0].poll_open = false;
    f.actions = vec![
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::ReplaceLedger },
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::PollGate(0) },
        Action { trigger: Trigger::Event(JobId(1), "failed"), kind: ActionKind::Repair { delivery: 0, wrong_ledger: true } },
        Action { trigger: Trigger::Event(JobId(1), "failed"), kind: ActionKind::RestoreLedger },
        Action { trigger: Trigger::Event(JobId(1), "failed"), kind: ActionKind::Repair { delivery: 0, wrong_ledger: false } },
    ];
    let out = run_driver(session_many(1, false, "mock-usage"), request(), f).unwrap();
    assert_eq!(out.jobs[0].terminal, Some("failed"));
    assert!(!out.recorded);
    assert_eq!(out.deliveries.len(), 1);
    assert_eq!(out.deliveries[0].disposition, "accounting_repaired_retained");
    assert_eq!(out.adapters[0].counters().submits, 1);
    assert!(out.deliveries[0].delivery.completed.usage.cost.is_some());
}

#[test]
fn done_requeue_is_rejected_and_preserves_original_delivery() {
    let mut f = fixture(0);
    f.record = false;
    f.actions.push(Action {
        trigger: Trigger::Event(JobId(1), "done"),
        kind: ActionKind::Requeue { job: JobId(1), key: "lifecycle-second".into(), replacement: None },
    });
    let out = run_driver(session(), request(), f).unwrap();
    assert_eq!(out.jobs.len(), 2);
    assert_eq!(out.jobs[0].starts, vec![1]);
    assert!(out.jobs[1].starts.is_empty());
    assert!(out.jobs[0].notified);
    assert!(!out.jobs[1].notified);
    assert_eq!(out.jobs[1].terminal, Some("rejected"));
    assert_eq!(out.results.len(), 1);
    assert_eq!(out.deliveries.len(), 1);
    assert_eq!(out.ownership.epochs, 2);
    assert_eq!(out.adapters[0].counters().submits, 1);
}
#[test]
fn cancelled_requeue_is_new_scheduler_epoch_without_run_revival() {
    let mut f = fixture(0);
    f.record = false;
    f.actions = vec![
        Action { trigger: Trigger::At(0), kind: ActionKind::Cancel(vec![JobId(1)]) },
        Action {
            trigger: Trigger::Event(JobId(1), "cancelled"),
            kind: ActionKind::Requeue { job: JobId(1), key: "lifecycle-second".into(), replacement: None },
        },
    ];
    let out = run_driver(session(), request(), f).unwrap();
    assert_eq!(out.jobs.len(), 2);
    assert_eq!(out.jobs[0].terminal, Some("cancelled"));
    assert_eq!(out.jobs[1].terminal, Some("done"));
    assert!(!out.jobs[1].notified);
    assert!(out.results.is_empty());
    assert_eq!(out.deliveries.len(), 1);
    assert_eq!(out.deliveries[0].disposition, "late_retained");
    assert_eq!(out.adapters[0].counters().submits, 1);
}
#[test]
fn named_target_restore_uses_captured_identity_even_when_remote_ids_alias() {
    let mut f = fixture(1);
    f.adapters[0].poll_open = false;
    f.adapters[0].instance = Some(nodecraft_types::ProviderInstanceId::from_bytes([4; 16]));
    let mut other = fixture(1).adapters.remove(0);
    other.instance = Some(nodecraft_types::ProviderInstanceId::from_bytes([5; 16]));
    f.adapters.push(other);
    f.actions.extend([
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::Restore { job: JobId(1), adapter: 0, base: BASE.into() } },
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::PollGate(0) },
    ]);
    let out = run_driver(session(), request(), f).unwrap();
    assert!(out.recorded);
    assert_eq!(out.adapters[0].counters().submits, 1);
    assert_eq!(out.adapters[1].counters().submits, 0);
    assert_eq!(out.adapters[1].counters().polls, 0);
    assert!(out.trace.iter().filter_map(|t| t.target.as_ref()).all(|t| t.instance() == Some(nodecraft_types::ProviderInstanceId::from_bytes([4; 16]))));
}
#[test]
fn synthetic_invalid_media_preserves_inner_usage_and_refuses_conversion() {
    let mut f = fixture(0);
    f.adapters[0].models[0].1.cost = Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 7));
    f.adapters[0].models[0].0 = "mock-usage".into();
    f.adapters[0].synthetic = Some(SyntheticFixture {
        outputs: Some(vec![nodecraft_providers::Output::Bytes { mime: "image/png".into(), bytes: std::sync::Arc::from(b"invalid public image".as_slice()) }]),
        partial_keys: vec![],
    });
    let out = run_driver(session_many(1, false, "mock-usage"), request(), f).unwrap();
    assert!(!out.recorded);
    assert_eq!(out.deliveries.len(), 1);
    assert_eq!(out.deliveries[0].disposition, "conversion_refused");
    assert_eq!(out.deliveries[0].delivery.completed.usage.cost, Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 7)));
    assert_eq!(out.adapters[0].counters().submits, 1);
}

#[test]
fn old_accounting_repair_cannot_consume_replacement_hold() {
    let mut f = fixture(1);
    f.record = false;
    f.adapters[0].models[0].0 = "mock-usage".into();
    f.adapters[0].models[0].1.cost = Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 7));
    f.adapters[0].poll_open = false;
    let mut next = fixture(1).adapters.remove(0);
    next.models[0].0 = "mock-usage".into();
    next.models[0].1.cost = Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 11));
    next.instance = Some(nodecraft_types::ProviderInstanceId::from_bytes([8; 16]));
    next.poll_open = false;
    f.adapters.push(next);
    f.actions = vec![
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::ReplaceLedger },
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::PollGate(0) },
        Action { trigger: Trigger::Event(JobId(1), "failed"), kind: ActionKind::RestoreLedger },
        Action { trigger: Trigger::Event(JobId(1), "failed"), kind: ActionKind::Requeue { job: JobId(1), key: "replacement".into(), replacement: Some(1) } },
        Action { trigger: Trigger::Epoch(JobId(1), 1, "submitted"), kind: ActionKind::Repair { delivery: 0, wrong_ledger: false } },
        Action { trigger: Trigger::Epoch(JobId(1), 1, "submitted"), kind: ActionKind::PollGate(1) },
        Action { trigger: Trigger::Epoch(JobId(1), 1, "done"), kind: ActionKind::Forget },
    ];
    let out = run_driver(session_many(1, false, "mock-usage"), request(), f).unwrap();
    assert_eq!(out.jobs[0].terminal, Some("failed"));
    assert_eq!(out.jobs[1].terminal, Some("done"));
    assert!(!out.jobs[1].notified);
    assert!(out.results.is_empty());
    assert_eq!(out.deliveries.len(), 2);
    assert_eq!(out.deliveries[0].disposition, "accounting_repaired_retained");
    let log = out.budget.log.lock().unwrap();
    assert_eq!(log.entries().count(), 2);
    assert_eq!(log.entries().map(|e| e.cost.micros).sum::<u64>(), 18);
    assert_eq!(log.metadata_statistics().held, 0);
    assert_eq!(out.adapters.iter().map(|a| a.counters().submits).sum::<usize>(), 2);
}
#[test]
fn known_unknown_and_changed_budget_use_real_admission() {
    let usd = |n| nodecraft_types::Money::new(nodecraft_types::Currency::USD, n);
    for unknown in [false, true] {
        let model = if unknown { "mock-usage" } else { "mock-counted" };
        let mut f = fixture(0);
        f.adapters[0].models[0].0 = "mock-text".into();
        if unknown {
            f.adapters[0].models[0].0 = "mock-usage".into();
        }
        f.budgets.per_call = Some(usd(1));
        let mut r = request();
        r.amounts.insert("USD".into(), 20);
        let e = run_driver(session_many(1, false, model), r, f).err().unwrap();
        assert_eq!(e.outcome.adapters[0].counters().submits, 0);
    }
    let mut f = fixture(0);
    f.before_authorize = AuthorityChange::Budget;
    let mut r = request();
    r.amounts.insert("USD".into(), 20);
    let e = run_driver(session_many(1, false, "mock-counted"), r, f).err().unwrap();
    assert_eq!(e.code, "authorize");
    assert_eq!(e.outcome.adapters[0].counters().submits, 0);
    let mut f = fixture(0);
    f.adapters[0].models[0].0 = "mock-usage".into();
    let mut r = request();
    r.partial = false;
    assert!(run_driver(session_many(1, false, "mock-usage"), r, f).is_err());
}
#[test]
fn synthetic_configuration_bounds_refuse_before_work() {
    use nodecraft_providers::Output;
    for outputs in [
        vec![Output::Text("x".repeat(8193))],
        vec![Output::Url { url: "https://invalid.example/public".into(), mime: None }],
        vec![Output::Bytes { mime: "x".repeat(65), bytes: std::sync::Arc::from(b"x".as_slice()) }],
    ] {
        let mut f = fixture(0);
        f.adapters[0].synthetic = Some(SyntheticFixture { outputs: Some(outputs), partial_keys: vec![] });
        let e = run_driver(session(), request(), f).err().unwrap();
        assert!(e.outcome.deliveries.is_empty());
        assert_eq!(e.outcome.record_dispatches, 0);
    }
}

struct Multi(nodecraft_nodes::NodeDescriptor);
impl nodecraft_nodes::NodeKind for Multi {
    fn descriptor(&self) -> &nodecraft_nodes::NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[nodecraft_types::PortType]) -> Option<nodecraft_nodes::ModeId> {
        nodecraft_nodes::ModeId::new("generate").ok()
    }
    fn plan(&self, c: &nodecraft_nodes::PlanCtx<'_>) -> Result<Vec<nodecraft_nodes::JobSpec>, nodecraft_types::Error> {
        let catalog = nodecraft_testkit::catalog::mock_catalog()?;
        let mut jobs = Vec::new();
        for name in ["model", "second"] {
            let Some(Value::Text(model)) = c.node.params.get(name) else {
                return Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::InvalidInput, "fixture model"));
            };
            let def = catalog.get(model).ok_or_else(|| nodecraft_types::Error::new(nodecraft_types::ErrorKind::InvalidInput, "fixture catalog"))?;
            jobs.push(nodecraft_nodes::JobSpec::Generate {
                node: c.node.id,
                provider: "mock".into(),
                instance: c.targets.get(c.node.id, "mock").and_then(|t| t.instance()),
                model: def.provider_model.clone(),
                params: def.params.resolve(&nodecraft_exec::cost::model_params(c.node, def), true)?.into_visible_values(),
                inputs: c.inputs.iter().map(|i| nodecraft_nodes::GenerateInput { role: "prompt".into(), value: i.value.clone() }).collect(),
                outputs: vec![nodecraft_nodes::GenerateOutput { port: "out".into(), modality: nodecraft_types::Modality::Text }],
            });
        }
        Ok(jobs)
    }
}
fn multi_session(action: bool) -> Session {
    let base = session_many(1, action, "mock-text");
    let mut p = base.document().clone();
    let mut kinds = kinds::registry().unwrap();
    if action {
        kinds.register(Box::new(ActionSource(source_descriptor()))).unwrap();
    }
    let mut d = kinds.get("mock.text").unwrap().descriptor().clone();
    d.id = "driver.multi".into();
    d.outputs[0].port_type.many = true;
    let mut second = d.params.params[0].clone();
    second.name = "second".into();
    second.default = Some(Value::Text("mock-usage".into()));
    d.params.params.push(second);
    kinds.register(Box::new(Multi(d))).unwrap();
    let params = [("model".into(), Value::Text("mock-counted".into()))].into_iter().collect();
    let node = kinds.get("driver.multi").unwrap().instantiate_instance(id(2), Default::default(), &params).unwrap();
    Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
    Session::with_registries(p, Registry::builtins().unwrap(), kinds).unwrap()
}
#[test]
fn multi_model_quote_partial_consent_and_two_actual_driver_jobs() {
    let s = multi_session(false);
    let mut f = fixture(0);
    f.adapters[0].models.push(("mock-usage".into(), MockModel::new(MockOutput::Text)));
    let mut r = request();
    r.amounts.insert("USD".into(), 20);
    let out = run_driver(s, r, f).unwrap();
    assert!(out.recorded);
    assert_eq!(out.jobs.len(), 2);
    assert_eq!(out.deliveries.len(), 2);
    assert_eq!(out.results.len(), 1);
    assert_eq!(out.adapters[0].counters().submits, 2);
}

#[test]
fn committed_small_input_and_reviewed_causal_baseline() {
    let project: Project = serde_json::from_str(include_str!("golden_driver/baseline.project.json")).unwrap();
    assert_eq!(&project, session().document());
    let kinds = kinds::registry().unwrap();
    let s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap();
    let mut out = run_driver(s, request(), fixture(0)).unwrap();
    let expected: serde_json::Value = serde_json::from_str(include_str!("golden_driver/baseline.report.json")).unwrap();
    assert_eq!(out.semantic_report().unwrap(), expected);
}
#[test]
fn joint_config_emission_and_inner_owner_boundary_is_before_provider_work() {
    let output = || vec![nodecraft_providers::Output::Text("x".into())];
    let config = 256 + std::mem::size_of::<nodecraft_providers::Output>() + 1;
    let boundary = 4 * 2 * 8192 + config + 8192;
    for delta in [0, 1] {
        let mut f = fixture(0);
        f.adapters[0].synthetic = Some(SyntheticFixture { outputs: Some(output()), partial_keys: vec![] });
        f.original_cap = boundary - delta;
        f.epoch_ceiling = 1;
        let result = run_driver(session(), request(), f);
        if delta == 0 {
            let out = result.unwrap();
            assert_eq!(out.ownership.original, boundary);
            assert_eq!(out.deliveries.len(), 1);
        } else {
            let e = result.err().unwrap();
            assert_eq!(e.code, "original_ownership");
            assert_eq!(e.outcome.adapters[0].counters().submits, 0);
            assert!(e.outcome.deliveries.is_empty());
        }
    }
}

#[test]
fn synthetic_spare_capacity_and_output_count_are_not_hidden_owners() {
    use nodecraft_providers::Output;
    let mut spare = String::with_capacity(8193);
    spare.push('x');
    let mut outputs = Vec::with_capacity(17);
    outputs.push(Output::Text("x".into()));
    for outputs in [vec![Output::Text(spare)], outputs, vec![Output::Text("x".into()); 17]] {
        let mut f = fixture(0);
        f.adapters[0].synthetic = Some(SyntheticFixture { outputs: Some(outputs), partial_keys: vec![] });
        let e = run_driver(session(), request(), f).err().unwrap();
        assert!(e.outcome.adapters.is_empty());
        assert_eq!(e.outcome.record_dispatches, 0);
    }
}
#[test]
fn synthetic_partial_is_once_per_explicit_operation_through_poll_retries() {
    use nodecraft_providers::{
        ErrorKind,
        mock::{MockFailures, MockScript},
    };
    let mut f = scripted(MockScript { poll_failures: Some(MockFailures { count: 2, kind: ErrorKind::Network }), ..Default::default() }, 2, 0);
    f.adapters[0].synthetic = Some(SyntheticFixture { outputs: None, partial_keys: vec![BASE.into()] });
    let out = run_driver(session(), request(), f).unwrap();
    assert_eq!(out.trace.iter().filter(|t| t.kind == "partial").count(), 1);
    assert!(out.recorded);
    assert_eq!(out.deliveries.len(), 1);
    assert_eq!(out.adapters[0].counters().submits, 1);
}
#[test]
fn synthetic_inner_failure_never_manufactures_completion() {
    use nodecraft_providers::{
        ErrorKind, Output,
        mock::{MockFailures, MockScript},
    };
    let mut f = scripted(MockScript { submit_failures: Some(MockFailures { count: 4, kind: ErrorKind::Network }), ..Default::default() }, 0, 0);
    f.adapters[0].synthetic = Some(SyntheticFixture { outputs: Some(vec![Output::Text("synthetic should not appear".into())]), partial_keys: vec![] });
    let out = run_driver(session(), request(), f).unwrap();
    assert!(!out.recorded);
    assert!(out.deliveries.is_empty());
    assert_eq!(out.adapters[0].counters().submits, 4);
}

#[test]
fn accepted_paid_cancel_attempts_remote_cancel_and_keeps_unresolved_charge() {
    let mut f = fixture(1);
    f.adapters[0].poll_open = false;
    f.adapters[0].models[0].1.cost = Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 20));
    f.actions.push(Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::Cancel(vec![JobId(1)]) });
    let mut r = request();
    r.amounts.insert("USD".into(), 20);
    let out = run_driver(session_many(1, false, "mock-counted"), r, f).unwrap();
    assert_eq!(out.jobs[0].terminal, Some("cancelled"));
    assert_eq!(out.adapters[0].counters().submits, 1);
    assert_eq!(out.adapters[0].counters().cancels, 1);
    let log = out.budget.log.lock().unwrap();
    assert_eq!(log.entries().map(|e| (e.cost.micros, e.status)).collect::<Vec<_>>(), vec![(20, nodecraft_exec::cost::Charge::Unresolved)]);
    assert!(out.deliveries.is_empty());
    assert!(!out.recorded);
}
#[test]
fn pause_resume_and_last_handle_allow_real_driver_to_drain() {
    let mut f = fixture(0);
    f.actions.extend([Action { trigger: Trigger::At(0), kind: ActionKind::Pause(true) }, Action { trigger: Trigger::At(10), kind: ActionKind::Pause(false) }]);
    let out = run_driver(session(), request(), f).unwrap();
    assert!(out.cleanup_complete);
    assert!(out.recorded);
    assert!(out.scheduler.is_some());
    assert_eq!(out.adapters[0].counters().submits, 1);
}

#[test]
fn scenario_causal_reports() {
    for case in 1..=9 {
        let mut f = fixture(0);
        let mut r = request();
        let mut s = session();
        match case {
            1 => {}
            2 => {
                f = scripted(
                    nodecraft_providers::mock::MockScript {
                        submit_failures: Some(nodecraft_providers::mock::MockFailures { count: 2, kind: nodecraft_providers::ErrorKind::Network }),
                        ..Default::default()
                    },
                    0,
                    0,
                );
            }
            3 => {
                f = scripted(
                    nodecraft_providers::mock::MockScript {
                        lost_replies: Some(nodecraft_providers::mock::MockFailures { count: 1, kind: nodecraft_providers::ErrorKind::Network }),
                        poll_failures: Some(nodecraft_providers::mock::MockFailures { count: 1, kind: nodecraft_providers::ErrorKind::Network }),
                        ..Default::default()
                    },
                    1,
                    0,
                );
            }
            4 => {
                f = scripted(
                    nodecraft_providers::mock::MockScript {
                        failed_generations: Some(nodecraft_providers::mock::MockFailures { count: 1, kind: nodecraft_providers::ErrorKind::Network }),
                        ..Default::default()
                    },
                    1,
                    1,
                );
            }
            5 => {
                f.adapters[0].models[0].0 = "mock-usage".into();
                s = session_many(1, false, "mock-usage");
            }
            6 => {
                f.adapters[0].models[0].1.polls = 1;
                f.adapters[0].poll_open = false;
                f.actions.push(Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::Cancel(vec![JobId(1)]) });
                s = session_many(1, false, "mock-counted");
                r.amounts.insert("USD".into(), 20);
            }
            7 => {
                f.adapters[0].models[0].1.polls = 1;
                f.adapters[0].models[0].1.cost = Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 7));
                f.adapters[0].models[0].0 = "mock-usage".into();
                f.adapters[0].poll_open = false;
                s = session_many(1, false, "mock-usage");
                f.actions = vec![
                    Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::ReplaceLedger },
                    Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::PollGate(0) },
                    Action { trigger: Trigger::Event(JobId(1), "failed"), kind: ActionKind::RestoreLedger },
                    Action { trigger: Trigger::Event(JobId(1), "failed"), kind: ActionKind::Repair { delivery: 0, wrong_ledger: false } },
                ];
            }
            8 => {
                f.adapters[0].synthetic = Some(SyntheticFixture {
                    outputs: Some(vec![nodecraft_providers::Output::Bytes {
                        mime: "image/png".into(),
                        bytes: std::sync::Arc::from(b"invalid image".as_slice()),
                    }]),
                    partial_keys: vec![],
                });
            }
            9 => {
                f.adapters[0].models[0].1.polls = 1;
                f.adapters[0].instance = Some(nodecraft_types::ProviderInstanceId::from_bytes([4; 16]));
                f.adapters[0].poll_open = false;
                f.replace_routing_after_authorize = true;
                f.actions = vec![
                    Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::Restore { job: JobId(1), adapter: 0, base: BASE.into() } },
                    Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::PollGate(0) },
                ];
            }
            _ => unreachable!(),
        }
        let mut out = run_driver(s, r, f).unwrap();
        let report = out.semantic_report().unwrap();
        assert!(out.cleanup_complete);
        assert!(out.jobs.iter().all(|j| j.notified));
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden_driver").join(format!("scenario{case}.json"));
        let expected: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(report, expected, "scenario{case}: causal/ledger projection changed");
    }
}

#[test]
fn provisional_one_expands_to_two_real_jobs_under_precharged_ceiling() {
    for ceiling in [2, 3] {
        let mut f = fixture(0);
        f.epoch_ceiling = ceiling;
        f.adapters[0].models.push(("mock-usage".into(), MockModel::new(MockOutput::Text)));
        let mut r = request();
        r.amounts.insert("USD".into(), 20);
        let result = run_driver(multi_session(true), r, f);
        if ceiling == 3 {
            let out = result.unwrap();
            assert_eq!(out.quote_jobs.iter().find(|(n, _)| *n == id(2)).unwrap().1, 1);
            assert_eq!(out.jobs.len(), 3);
            assert_eq!(out.ownership.epochs, 3);
            assert_eq!(out.ownership.original, 196608);
            assert_eq!(out.ownership.delivery_metadata, 49152);
            assert_eq!(out.deliveries.len(), 2);
            assert_eq!(out.adapters[0].counters().submits, 2);
            assert!(out.jobs.iter().all(|j| j.notified));
            assert_eq!(out.record_dispatches, 1);
        } else {
            let e = result.err().unwrap();
            assert_eq!(e.code, "epoch_limit");
            assert_eq!(e.outcome.adapters[0].counters().submits, 0);
            assert!(e.outcome.deliveries.is_empty());
        }
    }
    let mut f = fixture(0);
    f.epoch_ceiling = 3;
    f.original_cap = 196607;
    f.adapters[0].models.push(("mock-usage".into(), MockModel::new(MockOutput::Text)));
    let mut r = request();
    r.amounts.insert("USD".into(), 20);
    let e = run_driver(multi_session(true), r, f).err().unwrap();
    assert_eq!(e.code, "original_ownership");
    assert_eq!(e.outcome.adapters[0].counters().submits, 0);
    assert!(e.outcome.jobs.is_empty());
}
#[test]
fn logical_1024_attempts_are_not_a_byte_allocation_entitlement() {
    let mut f = fixture(0);
    f.epoch_ceiling = 128;
    let mut r = request();
    r.options.retry.max_attempts = 8;
    let e = run_driver(session(), r, f).err().unwrap();
    assert_eq!(e.code, "original_ownership");
    assert_eq!(e.outcome.adapters[0].counters().submits, 0);
    assert!(e.outcome.jobs.is_empty());
}

#[test]
fn real_media_import_and_record_refusal_have_honest_store_consequences() {
    for invalid in [false, true] {
        let base = session();
        let mut p = base.document().clone();
        let kinds = kinds::registry().unwrap();
        let node = kinds.get("mock.image").unwrap().instantiate_instance(id(2), Default::default(), &Default::default()).unwrap();
        Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
        let s = Session::with_registries(p, Registry::builtins().unwrap(), kinds).unwrap();
        let before = s.document().clone();
        let mut f = fixture(0);
        f.adapters[0].models[0] = ("mock-image".into(), MockModel::new(MockOutput::Image));
        f.invalid_record = invalid;
        let mut r = request();
        r.amounts.insert("USD".into(), 40_000);
        let result = run_driver(s, r, f);
        let out = match result {
            Ok(out) => {
                assert!(!invalid);
                out
            }
            Err(e) => {
                assert!(invalid);
                assert_eq!(e.code, "record_dispatch");
                assert_eq!(e.outcome.session.document(), &before);
                *e.outcome
            }
        };
        assert_eq!(out.deliveries.len(), 1);
        let media = out.results[0].generation.outputs.iter().find_map(|v| if let Value::Media(a) = v { Some(a) } else { None }).unwrap();
        assert!(out.session.assets().get(&media.hash).is_some(), "successful import legitimately remains on record refusal");
        assert_eq!(out.recorded, !invalid);
    }
}

#[test]
fn failed_branch_skips_its_downstream_but_records_independent_success_once() {
    let base = session_many(3, false, "mock-text");
    let mut p = base.document().clone();
    let kinds = kinds::registry().unwrap();
    let params = [("model".into(), Value::Text("mock-usage".into()))].into_iter().collect();
    let node = kinds.get("mock.text").unwrap().instantiate_instance(id(2), Default::default(), &params).unwrap();
    Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
    Change::Disconnect { canvas: C, id: EdgeId::from_bytes([4; 16]) }.apply(&mut p).unwrap();
    Change::Connect {
        canvas: C,
        edge: Edge {
            id: EdgeId::from_bytes([4; 16]),
            from: Endpoint { node: id(2), port: "out".into() },
            to: Endpoint { node: id(4), port: "in".into() },
            order: 0,
        },
    }
    .apply(&mut p)
    .unwrap();
    let s = Session::with_registries(p, Registry::builtins().unwrap(), kinds).unwrap();
    let before = s.document().clone();
    let mut f = fixture(0);
    let mut failed = MockModel::new(MockOutput::Text);
    failed.fail = Some(nodecraft_providers::mock::MockFailure::Submit(nodecraft_providers::ErrorKind::UnsupportedInput));
    f.adapters[0].models.push(("mock-usage".into(), failed));
    f.selections = vec![(id(2), 0), (id(3), 0), (id(4), 0)];
    let mut out = run_driver(s, DriverRequest::selected(C, vec![id(3), id(4)]), f).unwrap();
    assert!(out.recorded);
    assert_eq!(out.record_dispatches, 1);
    assert_eq!(out.results.len(), 1);
    assert_eq!(out.results[0].node, id(3));
    assert_eq!(out.jobs.len(), 2);
    assert!(out.jobs.iter().all(|j| j.notified));
    assert!(out.failures.iter().any(|(n, _)| *n == id(2)));
    assert!(out.failures.iter().any(|(n, _)| *n == id(4)));
    assert_eq!(out.adapters[0].counters().submits, 2);
    assert_eq!(out.deliveries.len(), 1);
    assert!(!out.trace.iter().any(|t| t.kind == "skipped"), "Run owns frontier/downstream skip propagation");
    out.session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    assert_eq!(out.session.document(), &before);
}

#[test]
fn both_named_instances_return_real_aliased_handles_and_restore_only_their_target() {
    let mut f = fixture(1);
    f.adapters[0].instance = Some(nodecraft_types::ProviderInstanceId::from_bytes([4; 16]));
    f.adapters[0].poll_open = false;
    let mut second = fixture(1).adapters.remove(0);
    second.instance = Some(nodecraft_types::ProviderInstanceId::from_bytes([5; 16]));
    second.poll_open = false;
    f.adapters.push(second);
    f.selections = vec![(id(2), 0), (id(3), 1)];
    for (job, adapter, base) in [(JobId(1), 0, BASE), (JobId(2), 1, "nodecraft:70707070707070707070707070707070:2")] {
        f.actions.push(Action { trigger: Trigger::Update(job, "submitted"), kind: ActionKind::Restore { job, adapter, base: base.into() } });
        f.actions.push(Action { trigger: Trigger::Update(job, "submitted"), kind: ActionKind::PollGate(adapter) });
    }
    let out = run_driver(session_many(2, false, "mock-text"), DriverRequest::selected(C, vec![id(2), id(3)]), f).unwrap();
    let submitted: Vec<_> = out.trace.iter().filter(|t| t.kind == "submitted").collect();
    assert_eq!(submitted.len(), 2);
    assert_eq!(submitted[0].handle.as_ref().unwrap().remote_id, submitted[1].handle.as_ref().unwrap().remote_id);
    assert_ne!(submitted[0].target, submitted[1].target);
    assert!(out.adapters.iter().all(|a| a.counters().submits == 1 && a.counters().polls == 1));
    assert_eq!(out.deliveries.len(), 2);
    assert!(out.recorded);
}
#[test]
fn isolated_same_key_default_named_lifecycle_keeps_two_charge_identities() {
    let mut f = fixture(1);
    f.record = false;
    f.adapters[0].poll_open = false;
    let mut second = fixture(1).adapters.remove(0);
    second.instance = Some(nodecraft_types::ProviderInstanceId::from_bytes([9; 16]));
    second.models[0].1.cost = Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 11));
    f.adapters.push(second);
    f.actions = vec![
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::Cancel(vec![JobId(1)]) },
        Action { trigger: Trigger::Event(JobId(1), "cancelled"), kind: ActionKind::Requeue { job: JobId(1), key: BASE.into(), replacement: Some(1) } },
    ];
    // The possibly billed first attempt keeps its debit, so both attempts must be accepted.
    let mut r = request();
    r.amounts.insert("USD".into(), 40);
    let out = run_driver(session_many(1, false, "mock-counted"), r, f).unwrap();
    assert!(!out.recorded);
    assert!(out.results.is_empty());
    assert_eq!(out.jobs.len(), 2);
    assert_eq!(out.jobs[0].terminal, Some("cancelled"));
    assert_eq!(out.jobs[1].terminal, Some("done"));
    assert!(!out.jobs[1].notified);
    let log = out.budget.log.lock().unwrap();
    let entries: Vec<_> = log.entries().collect();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.idempotency_key == BASE));
    assert!(entries.iter().any(|e| e.instance.is_none() && e.cost.micros == 20 && e.status == nodecraft_exec::cost::Charge::Unresolved));
    assert!(entries.iter().any(|e| e.instance == Some(nodecraft_types::ProviderInstanceId::from_bytes([9; 16])) && e.cost.micros == 11));
}

#[test]
fn rejected_nonterminal_requeue_never_relabels_live_attempt_or_loses_its_output() {
    let mut f = fixture(1);
    f.adapters[0].poll_open = false;
    f.actions = vec![
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::Requeue { job: JobId(1), key: "rejected-live".into(), replacement: None } },
        Action { trigger: Trigger::At(10), kind: ActionKind::PollGate(0) },
    ];
    let out = run_driver(session(), request(), f).unwrap();
    assert_eq!(out.jobs.len(), 2);
    assert_eq!(out.jobs[0].terminal, Some("done"));
    assert!(out.jobs[0].notified);
    assert_eq!(out.jobs[0].starts, vec![1]);
    assert_eq!(out.jobs[1].terminal, Some("rejected"));
    assert!(out.jobs[1].starts.is_empty());
    assert!(!out.jobs[1].notified);
    assert_eq!(out.deliveries.len(), 1);
    assert_eq!(out.deliveries[0].epoch, 0);
    assert!(out.recorded);
    assert_eq!(out.adapters[0].counters().submits, 1);
}

#[test]
fn definite_add_capacity_refusals_never_start_or_charge() {
    for payload in [false, true] {
        let mut f = fixture(0);
        if payload {
            f.admission.data_payload_bytes = 0;
            f.admission.data_bytes = 0;
        } else {
            f.admission.data_commands = 0;
        }
        let out = run_driver(session(), request(), f).unwrap();
        assert_eq!(out.jobs[0].terminal, Some("rejected"));
        assert!(out.jobs[0].notified);
        assert!(out.jobs[0].starts.is_empty());
        assert_eq!(out.adapters[0].counters().submits, 0);
        assert!(out.deliveries.is_empty());
        assert_eq!(out.budget.log.lock().unwrap().entries().count(), 0);
        assert!(!out.recorded);
    }
}
#[test]
fn registration_replacement_does_not_change_already_captured_provider_future() {
    let mut f = fixture(1);
    f.record = false;
    f.adapters[0].poll_open = false;
    let mut second = fixture(1).adapters.remove(0);
    second.instance = Some(nodecraft_types::ProviderInstanceId::from_bytes([7; 16]));
    f.adapters.push(second);
    f.actions = vec![
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::Replace { job: JobId(1), adapter: 1 } },
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::PollGate(0) },
    ];
    let out = run_driver(session(), request(), f).unwrap();
    assert_eq!(out.adapters[0].counters().submits, 1);
    assert_eq!(out.adapters[1].counters().submits, 0);
    assert_eq!(out.adapters[1].counters().polls, 0);
    assert_eq!(out.deliveries.len(), 1);
    assert_eq!(out.deliveries[0].epoch, 0);
    assert_eq!(out.jobs[1].terminal, Some("registered_only"));
    assert!(!out.jobs[1].notified);
    assert!(out.cleanup_complete);
}
#[test]
fn synthetic_partial_does_not_reset_on_cancel_and_requeue() {
    let mut f = fixture(1);
    f.record = false;
    f.adapters[0].synthetic = Some(SyntheticFixture { outputs: None, partial_keys: vec![BASE.into()] });
    f.actions = vec![
        Action { trigger: Trigger::Update(JobId(1), "partial"), kind: ActionKind::Cancel(vec![JobId(1)]) },
        Action { trigger: Trigger::Event(JobId(1), "cancelled"), kind: ActionKind::Requeue { job: JobId(1), key: "next-operation".into(), replacement: None } },
    ];
    let out = run_driver(session(), request(), f).unwrap();
    assert_eq!(out.trace.iter().filter(|t| t.kind == "partial").count(), 1);
    assert_eq!(out.adapters[0].counters().submits, 2);
    assert!(out.results.is_empty());
    assert_eq!(out.deliveries.len(), 1);
    assert_eq!(out.deliveries[0].disposition, "late_retained");
}
#[test]
fn each_declared_synthetic_operation_has_its_own_single_partial() {
    let mut f = fixture(1);
    f.adapters[0].synthetic = Some(SyntheticFixture { outputs: None, partial_keys: vec![BASE.into(), "nodecraft:70707070707070707070707070707070:2".into()] });
    f.selections.push((id(3), 0));
    let out = run_driver(session_many(2, false, "mock-text"), DriverRequest::selected(C, vec![id(2), id(3)]), f).unwrap();
    assert_eq!(out.trace.iter().filter(|t| t.kind == "partial").count(), 2);
    for job in [JobId(1), JobId(2)] {
        assert_eq!(out.trace.iter().filter(|t| t.kind == "partial" && t.job == job).count(), 1);
    }
    assert_eq!(out.deliveries.len(), 2);
    assert!(out.recorded);
}

#[test]
fn byte_text_conversion_cap_is_reserved_before_copy_and_record() {
    use nodecraft_providers::Output;
    use std::sync::Arc;
    for mime in ["text/plain", " Text/Plain", "text/custom"] {
        for bom in [false, true] {
            let text = if bom { format!("\u{feff}{}", "x".repeat(8189)) } else { "x".repeat(8192) };
            let mut f = fixture(0);
            f.conversion_cap = 4096;
            f.adapters[0].synthetic =
                Some(SyntheticFixture { outputs: Some(vec![Output::Bytes { mime: mime.into(), bytes: Arc::from(text.as_bytes()) }]), partial_keys: vec![] });
            let e = run_driver(session(), request(), f).err().unwrap();
            assert_eq!(e.code, "ownership_limit");
            assert_eq!(e.outcome.record_dispatches, 0);
            assert_eq!(e.outcome.adapters[0].counters().submits, 1);
            assert_eq!(e.outcome.deliveries.len(), 1);
            assert!(matches!(&e.outcome.deliveries[0].delivery.completed.outputs[0],Output::Bytes{bytes,..}if bytes.as_ref()==text.as_bytes()));
        }
    }
    let mut f = fixture(0);
    f.adapters[0].synthetic = Some(SyntheticFixture {
        outputs: Some(vec![Output::Bytes { mime: "text/plain".into(), bytes: Arc::from(&b"positive byte text"[..]) }]),
        partial_keys: vec![],
    });
    let out = run_driver(session(), request(), f).unwrap();
    assert_eq!(out.record_dispatches, 1);
    assert!(out.recorded);
    assert!(matches!(&out.results[0].generation.outputs[0], Value::Text(s) if s=="positive byte text"));
    let charge = out.ownership.conversion;
    for cap in [charge - 1, charge, charge + 1] {
        let mut f = fixture(0);
        f.conversion_cap = cap;
        f.adapters[0].synthetic = Some(SyntheticFixture {
            outputs: Some(vec![Output::Bytes { mime: "text/plain".into(), bytes: Arc::from(&b"positive byte text"[..]) }]),
            partial_keys: vec![],
        });
        let result = run_driver(session(), request(), f);
        if cap < charge {
            let e = result.err().unwrap();
            assert_eq!(e.outcome.record_dispatches, 0);
            assert_eq!(e.outcome.deliveries.len(), 1);
        } else {
            assert!(result.unwrap().recorded);
        }
    }
}

#[test]
fn requeue_then_forget_cannot_erase_pending_or_replacement_registration() {
    for replacement in [false, true] {
        let mut f = fixture(0);
        f.record = false;
        let mut second = fixture(0).adapters.remove(0);
        second.instance = Some(nodecraft_types::ProviderInstanceId::from_bytes([8; 16]));
        f.adapters.push(second);
        f.actions = vec![
            Action { trigger: Trigger::At(0), kind: ActionKind::Cancel(vec![JobId(1)]) },
            Action {
                trigger: Trigger::Event(JobId(1), "cancelled"),
                kind: ActionKind::Requeue { job: JobId(1), key: "replacement-after-cancel".into(), replacement: replacement.then_some(1) },
            },
            Action { trigger: Trigger::Event(JobId(1), "cancelled"), kind: ActionKind::Forget },
        ];
        let out = run_driver(session(), request(), f).unwrap();
        assert_eq!(out.adapters[usize::from(replacement)].counters().submits, 1);
        assert_eq!(out.jobs[0].terminal, Some("cancelled"));
        assert_eq!(out.jobs[1].terminal, Some("done"));
        assert!(!out.jobs[1].notified);
        assert_eq!(out.deliveries.len(), 1);
        assert!(out.results.is_empty());
        assert_eq!(out.record_dispatches, 0);
        assert!(out.cleanup_complete);
    }
    let mut f = fixture(1);
    f.adapters[0].poll_open = false;
    f.actions = vec![
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::Forget },
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::PollGate(0) },
        Action { trigger: Trigger::Event(JobId(1), "done"), kind: ActionKind::Forget },
    ];
    let out = run_driver(session(), request(), f).unwrap();
    assert!(out.recorded);
    assert!(out.scheduler.as_ref().unwrap().state(JobId(1)).is_none());
    assert_eq!(out.deliveries.len(), 1);
}

#[test]
fn generated_output_drives_successful_downstream_frontier_and_one_undo() {
    let base = session_many(2, false, "mock-text");
    let mut p = base.document().clone();
    Change::Disconnect { canvas: C, id: EdgeId::from_bytes([3; 16]) }.apply(&mut p).unwrap();
    Change::Connect {
        canvas: C,
        edge: Edge {
            id: EdgeId::from_bytes([3; 16]),
            from: Endpoint { node: id(2), port: "out".into() },
            to: Endpoint { node: id(3), port: "in".into() },
            order: 0,
        },
    }
    .apply(&mut p)
    .unwrap();
    let s = Session::with_registries(p, Registry::builtins().unwrap(), kinds::registry().unwrap()).unwrap();
    let before = s.document().clone();
    let mut f = fixture(1);
    f.selections.push((id(3), 0));
    let mut out = run_driver(s, DriverRequest::selected(C, vec![id(3)]), f).unwrap();
    assert_eq!(out.jobs.len(), 2);
    assert_eq!(out.deliveries.len(), 2);
    assert_eq!(out.record_dispatches, 1);
    let requests = out.adapters[0].requests().unwrap();
    assert_eq!(requests.len(), 2);
    let first = out.results.iter().find(|r| r.node == id(2)).unwrap();
    let Value::Text(upstream) = &first.generation.outputs[0] else { panic!("text frontier") };
    assert!(matches!(&requests[1].inputs[0],nodecraft_providers::JobInput::Text{text,..}if text==upstream));
    let done = out.trace.iter().position(|t| t.job == JobId(1) && t.kind == "done").unwrap();
    let downstream = out.trace.iter().position(|t| t.job == JobId(2) && t.kind == "started").unwrap();
    assert!(done < downstream);
    let recorded = out.session.document().clone();
    out.session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    assert_eq!(out.session.document(), &before);
    out.session.dispatch("edit.redo", &serde_json::Value::Null).unwrap();
    assert_eq!(out.session.document(), &recorded);
}

#[test]
fn named_instance_lanes_have_independent_caps_and_global_overlap() {
    let mut f = fixture(1);
    f.scheduler.global = 2;
    f.scheduler.default_per_lane = 1;
    f.adapters[0].poll_open = false;
    f.adapters[0].instance = Some(nodecraft_types::ProviderInstanceId::from_bytes([4; 16]));
    let mut second = fixture(1).adapters.remove(0);
    second.poll_open = false;
    second.instance = Some(nodecraft_types::ProviderInstanceId::from_bytes([5; 16]));
    f.adapters.push(second);
    f.selections = vec![(id(2), 0), (id(3), 1)];
    for adapter in [0, 1] {
        f.actions.push(Action { trigger: Trigger::At(10), kind: ActionKind::PollGate(adapter) });
    }
    let out = run_driver(session_many(2, false, "mock-text"), DriverRequest::selected(C, vec![id(2), id(3)]), f).unwrap();
    assert_eq!(out.peak_global, 2);
    assert_eq!(out.peak_lanes.len(), 2);
    assert!(out.peak_lanes.values().all(|n| *n == 1));
    assert_eq!(out.deliveries.len(), 2);
}

#[test]
fn actual_completion_independent_retry_timer_and_cancel_are_ready_together() {
    use nodecraft_providers::mock::{MockOperation, MockScript};
    let mut f = fixture(1);
    f.scheduler.global = 2;
    f.scheduler.default_per_lane = 2;
    f.adapters[0].poll_open = false;
    f.adapters[0].models[0].1.cost = Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 20));
    f.adapters[0].operations =
        vec![MockOperation { id: "race-completion".into(), model: "mock-text".into(), sent_keys: vec![BASE.into()], script: MockScript::default() }];
    let mut second = fixture(0).adapters.remove(0);
    second.instance = Some(nodecraft_types::ProviderInstanceId::from_bytes([6; 16]));
    second.models[0].1.cost = Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 20));
    second.operations = vec![MockOperation {
        id: "race-retry".into(),
        model: "mock-text".into(),
        sent_keys: vec!["nodecraft:70707070707070707070707070707070:2".into()],
        script: MockScript {
            submit_failures: Some(nodecraft_providers::mock::MockFailures { count: 1, kind: nodecraft_providers::ErrorKind::Network }),
            ..Default::default()
        },
    }];
    f.adapters.push(second);
    f.selections = vec![(id(2), 0), (id(3), 1)];
    f.actions
        .push(Action { trigger: Trigger::Event(JobId(2), "retrying"), kind: ActionKind::ReadyRace { adapter: 0, completion: JobId(1), retrying: JobId(2) } });
    let mut r = DriverRequest::selected(C, vec![id(2), id(3)]);
    r.amounts.insert("USD".into(), 40);
    let out = run_driver(session_many(2, false, "mock-counted"), r, f).unwrap();
    assert!(out.trace.iter().any(|t| t.kind == "completion_timer_cancel_ready"));
    assert!(out.cleanup_complete);
    assert!(matches!(out.jobs[0].terminal, Some("done" | "cancelled")));
    assert_eq!(out.jobs[1].terminal, Some("done"));
    assert!(out.jobs.iter().all(|j| j.notified));
    assert_eq!(out.adapters[0].counters().submits, 1);
    assert_eq!(out.adapters[1].counters().submits, 2);
    for job in [JobId(1), JobId(2)] {
        assert_eq!(out.trace.iter().filter(|t| t.job == job && matches!(t.kind, "done" | "failed" | "cancelled")).count(), 1);
        assert_eq!(out.deliveries.iter().filter(|d| d.job == job).count(), 1);
    }
    assert!(out.peak_global <= 2);
    assert!(out.peak_lanes.values().all(|n| *n <= 2));
    assert_eq!(out.jobs[1].starts, vec![1, 2]);
    let retries = out.adapters[1].requests().unwrap();
    assert_eq!(retries.len(), 2);
    assert_eq!(retries[0].idempotency_key, retries[1].idempotency_key);
    let log = out.budget.log.lock().unwrap();
    let entries: Vec<_> = log.entries().collect();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.cost.micros == 20));
    assert_ne!(entries[0].idempotency_key, entries[1].idempotency_key);
}

#[test]
fn concurrent_deferred_paid_jobs_share_session_project_caps_and_currency() {
    use nodecraft_exec::cost::Budgets;
    use nodecraft_types::{Currency, Money};
    for cap in [40, 80] {
        for project in [false, true] {
            let base = multi_session(true);
            let mut p = base.document().clone();
            let mut node = p.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap().clone();
            node.params.insert("second".into(), Value::Text("mock-counted".into()));
            Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
            let mut kinds = kinds::registry().unwrap();
            kinds.register(Box::new(ActionSource(source_descriptor()))).unwrap();
            let mut d = kinds.get("mock.text").unwrap().descriptor().clone();
            d.id = "driver.multi".into();
            d.outputs[0].port_type.many = true;
            let mut second = d.params.params[0].clone();
            second.name = "second".into();
            second.default = Some(Value::Text("mock-counted".into()));
            d.params.params.push(second);
            kinds.register(Box::new(Multi(d))).unwrap();
            let s = Session::with_registries(p, Registry::builtins().unwrap(), kinds).unwrap();
            let mut f = fixture(1);
            f.adapters[0].poll_open = false;
            f.adapters[0].models[0].1.cost = Some(Money::new(Currency::USD, 20));
            f.scheduler.global = 2;
            f.scheduler.default_per_lane = 2;
            f.budgets = if project {
                Budgets { project: Some(Money::new(Currency::USD, cap)), ..Default::default() }
            } else {
                Budgets { session: Some(Money::new(Currency::USD, cap)), ..Default::default() }
            };
            f.actions.push(Action { trigger: Trigger::At(10), kind: ActionKind::PollGate(0) });
            // Both fanned-out jobs are accepted (#366), so only the budget cap decides.
            let mut r = request();
            r.amounts.insert("USD".into(), 80);
            let out = run_driver(s, r, f).unwrap();
            if cap == 80 {
                assert_eq!(out.adapters[0].counters().submits, 2);
                assert_eq!(out.deliveries.len(), 2);
                assert_eq!(out.peak_global, 2);
                assert!(out.recorded);
                assert_eq!(out.record_dispatches, 1);
                let log = out.budget.log.lock().unwrap();
                assert_eq!(log.entries().count(), 2);
                assert_eq!(log.entries().map(|e| e.cost.micros).sum::<u64>(), 40);
                continue;
            }
            assert_eq!(out.adapters[0].counters().submits, 1, "second job must be denied while first conservative40 hold survives");
            assert_eq!(out.jobs.iter().filter(|j| j.provider && j.terminal == Some("failed")).count(), 1);
            assert_eq!(out.jobs.iter().filter(|j| j.provider && j.terminal == Some("cancelled")).count(), 1);
            // Run cancels the accepted sibling after the other job's budget failure.
            // Its genuine accepted hold remains unresolved, never treated as free.
            assert!(out.deliveries.is_empty());
            assert!(out.cleanup_complete);
            let log = out.budget.log.lock().unwrap();
            assert_eq!(log.entries().count(), 1);
            let held = log.entries().next().unwrap();
            assert_eq!(held.cost.micros, 40);
            assert_eq!(held.status, nodecraft_exec::cost::Charge::Unresolved);
        }
    }
    for project in [false, true] {
        let mut f = fixture(0);
        let limit = Money::new(Currency::EUR, 1000);
        f.budgets = if project { Budgets { project: Some(limit), ..Default::default() } } else { Budgets { session: Some(limit), ..Default::default() } };
        let mut r = request();
        r.amounts.insert("USD".into(), 20);
        let e = run_driver(session_many(1, false, "mock-counted"), r, f).err().unwrap();
        assert_eq!(e.outcome.adapters[0].counters().submits, 0);
        assert_eq!(e.outcome.record_dispatches, 0);
    }
}

struct LocalGenerator(nodecraft_nodes::NodeDescriptor);
impl nodecraft_nodes::NodeKind for LocalGenerator {
    fn descriptor(&self) -> &nodecraft_nodes::NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[nodecraft_types::PortType]) -> Option<nodecraft_nodes::ModeId> {
        nodecraft_nodes::ModeId::new("generate").ok()
    }
    fn plan(&self, c: &nodecraft_nodes::PlanCtx<'_>) -> Result<Vec<nodecraft_nodes::JobSpec>, nodecraft_types::Error> {
        Ok(vec![nodecraft_nodes::JobSpec::Generate {
            node: c.node.id,
            provider: "local".into(),
            instance: c.targets.get(c.node.id, "local").and_then(|t| t.instance()),
            model: "mock-text".into(),
            params: Default::default(),
            inputs: c.inputs.iter().map(|i| nodecraft_nodes::GenerateInput { role: "prompt".into(), value: i.value.clone() }).collect(),
            outputs: vec![nodecraft_nodes::GenerateOutput { port: "out".into(), modality: nodecraft_types::Modality::Text }],
        }])
    }
}
#[test]
fn literal_local_adapter_lane_is_distinct_from_actual_source_runner_lane() {
    let mut kinds = kinds::registry().unwrap();
    kinds.register(Box::new(ActionSource(source_descriptor()))).unwrap();
    let mut d = kinds.get("mock.text").unwrap().descriptor().clone();
    d.id = "driver.local".into();
    kinds.register(Box::new(LocalGenerator(d))).unwrap();
    let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Local lanes").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    for (n, kind) in [(1, "driver.source"), (2, "source.text"), (3, "driver.local")] {
        let params = if n == 3 {
            [("model".into(), Value::Text("mock-text".into()))].into_iter().collect()
        } else {
            [("text".into(), Value::Text("local lane".into()))].into_iter().collect()
        };
        let node = kinds.get(kind).unwrap().instantiate_instance(id(n), Default::default(), &params).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut p).unwrap();
    }
    Change::Connect {
        canvas: C,
        edge: Edge {
            id: EdgeId::from_bytes([3; 16]),
            from: Endpoint { node: id(2), port: "out".into() },
            to: Endpoint { node: id(3), port: "in".into() },
            order: 0,
        },
    }
    .apply(&mut p)
    .unwrap();
    let s = Session::with_registries(p, Registry::builtins().unwrap(), kinds).unwrap();
    let mut f = fixture(1);
    f.adapters[0].id = "local".into();
    f.adapters[0].poll_open = false;
    f.selections = vec![(id(3), 0)];
    f.scheduler.global = 2;
    f.scheduler.default_per_lane = 1;
    let mut model = nodecraft_testkit::catalog::mock_catalog().unwrap().get("mock-text").unwrap().clone();
    model.provider = "local".into();
    f.catalog = Some(nodecraft_catalog::Catalog::from_models([model]).unwrap());
    f.actions.push(Action { trigger: Trigger::At(10), kind: ActionKind::PollGate(0) });
    let out = run_driver(s, DriverRequest::selected(C, vec![id(1), id(3)]), f).unwrap();
    assert_eq!(out.peak_global, 2);
    assert_eq!(out.peak_lanes.len(), 2);
    assert!(out.peak_lanes.values().all(|n| *n == 1));
    assert_eq!(out.jobs.iter().filter(|j| !j.provider).count(), 1);
    assert_eq!(out.adapters[0].counters().submits, 1);
    assert_eq!(out.deliveries.len(), 1);
    assert!(out.recorded);
}

/// The deferred-fan-out session from `concurrent_deferred_paid_jobs_share_session_project_caps_and_currency`:
/// one deferred step quoted as one USD 40 job that becomes two at run time.
fn fan_out_session() -> Session {
    let base = multi_session(true);
    let mut p = base.document().clone();
    let mut node = p.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap().clone();
    node.params.insert("second".into(), Value::Text("mock-counted".into()));
    Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
    let mut kinds = kinds::registry().unwrap();
    kinds.register(Box::new(ActionSource(source_descriptor()))).unwrap();
    let mut d = kinds.get("mock.text").unwrap().descriptor().clone();
    d.id = "driver.multi".into();
    d.outputs[0].port_type.many = true;
    let mut second = d.params.params[0].clone();
    second.name = "second".into();
    second.default = Some(Value::Text("mock-counted".into()));
    d.params.params.push(second);
    kinds.register(Box::new(Multi(d))).unwrap();
    Session::with_registries(p, Registry::builtins().unwrap(), kinds).unwrap()
}

#[test]
fn deferred_fan_out_past_the_accepted_amount_is_refused_before_the_provider() {
    use nodecraft_types::{Currency, Money};
    for (accepted, submits) in [(79, 1), (80, 2)] {
        let mut f = fixture(1);
        f.adapters[0].poll_open = false;
        f.adapters[0].models[0].1.cost = Some(Money::new(Currency::USD, 20));
        f.scheduler.global = 2;
        f.scheduler.default_per_lane = 2;
        f.actions.push(Action { trigger: Trigger::At(10), kind: ActionKind::PollGate(0) });
        let mut r = request();
        r.amounts.insert("USD".into(), accepted);
        let out = run_driver(fan_out_session(), r, f).unwrap();
        assert_eq!(out.adapters[0].counters().submits, submits, "accepted {accepted}: no budgets, only the run ceiling");
        let log = out.budget.log.lock().unwrap();
        assert_eq!(log.entries().count(), submits, "a refused job reserves nothing");
        if submits == 1 {
            assert_eq!(out.jobs.iter().filter(|j| j.provider && j.terminal == Some("failed")).count(), 1);
            assert!(out.deliveries.is_empty(), "the run fails; the accepted sibling's charge is kept, never freed");
        }
    }
}

#[test]
fn a_requeue_after_a_possibly_billed_attempt_must_fit_what_is_left() {
    let mut f = fixture(1);
    f.record = false;
    f.adapters[0].poll_open = false;
    f.actions = vec![
        Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::Cancel(vec![JobId(1)]) },
        Action { trigger: Trigger::Event(JobId(1), "cancelled"), kind: ActionKind::Requeue { job: JobId(1), key: "ceiling-second".into(), replacement: None } },
    ];
    let mut r = request();
    r.amounts.insert("USD".into(), 20);
    let out = run_driver(session_many(1, false, "mock-counted"), r, f).unwrap();
    assert_eq!(out.jobs.len(), 2);
    assert_eq!(out.jobs[1].terminal, Some("failed"));
    assert_eq!(out.adapters[0].counters().submits, 1, "the requeue never reaches the provider");
    let log = out.budget.log.lock().unwrap();
    let entries: Vec<_> = log.entries().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!((entries[0].cost.micros, entries[0].status), (20, nodecraft_exec::cost::Charge::Unresolved));
}

/// A cancel before the provider never strands the accepted amount; the credit itself is proven by
/// `the_run_ceiling_is_debited_once_and_credited_only_by_its_owner` in the engine.
#[test]
fn a_cancel_before_the_provider_leaves_room_for_the_requeue() {
    let mut f = fixture(0);
    f.record = false;
    f.actions = vec![
        Action { trigger: Trigger::At(0), kind: ActionKind::Cancel(vec![JobId(1)]) },
        Action { trigger: Trigger::Event(JobId(1), "cancelled"), kind: ActionKind::Requeue { job: JobId(1), key: "ceiling-second".into(), replacement: None } },
    ];
    let mut r = request();
    r.amounts.insert("USD".into(), 20);
    let out = run_driver(session_many(1, false, "mock-counted"), r, f).unwrap();
    assert_eq!(out.jobs[0].terminal, Some("cancelled"));
    assert_eq!(out.jobs[1].terminal, Some("done"));
    assert_eq!(out.adapters[0].counters().submits, 1);
}

#[test]
fn unknown_price_fan_out_is_bounded_by_the_accepted_job_count() {
    // One deferred unknown-price step (mock-counted + mock-usage) quoted as one job that becomes
    // two at run time: the accepted count, not a price, bounds it.
    for (accepted, submits) in [(1, 1), (2, 2)] {
        let mut f = fixture(0);
        f.epoch_ceiling = 3;
        f.adapters[0].models.push(("mock-usage".into(), MockModel::new(MockOutput::Text)));
        let mut r = request();
        r.amounts.insert("USD".into(), 20);
        r.unpriced_jobs = Some(accepted);
        let out = run_driver(multi_session(true), r, f).unwrap();
        assert_eq!(out.adapters[0].counters().submits, submits, "accepted {accepted}");
        if submits == 1 {
            let refused = out.jobs.iter().filter(|j| j.provider && j.terminal == Some("failed")).count();
            assert_eq!(refused, 1, "the second job is refused before the provider");
        }
    }
    // Partial consent without a count admits no unknown-price job: the run never starts.
    let mut f = fixture(0);
    f.epoch_ceiling = 3;
    f.adapters[0].models.push(("mock-usage".into(), MockModel::new(MockOutput::Text)));
    let mut r = request();
    r.amounts.insert("USD".into(), 20);
    r.unpriced_jobs = None;
    let e = run_driver(multi_session(true), r, f).err().unwrap();
    assert_eq!(e.outcome.adapters[0].counters().submits, 0);
}

#[test]
fn an_accepted_but_lost_reply_keeps_its_debit_and_a_requeue_must_fit() {
    use nodecraft_providers::{
        ErrorKind,
        mock::{MockFailures, MockOperation, MockScript},
    };
    // Every reply to the first attempt is lost, so it may have been billed. With USD 20 accepted
    // its debit fills the ceiling and the requeue under a new key never reaches the provider;
    // with USD 40 (the control) the requeue is sent.
    for (accepted, requeue_sent) in [(20, false), (40, true)] {
        let mut f = fixture(0);
        f.record = false;
        f.adapters[0].models.push(("mock-counted".into(), MockModel::new(MockOutput::Text)));
        f.adapters[0].operations.push(MockOperation {
            id: "operation".into(),
            model: "mock-counted".into(),
            sent_keys: vec![nodecraft_engine::jobs::sent_key(BASE, 0)],
            script: MockScript { lost_replies: Some(MockFailures { count: 4, kind: ErrorKind::Network }), ..Default::default() },
        });
        f.actions = vec![Action {
            trigger: Trigger::Event(JobId(1), "failed"),
            kind: ActionKind::Requeue { job: JobId(1), key: "ceiling-second".into(), replacement: None },
        }];
        let mut r = request();
        r.amounts.insert("USD".into(), accepted);
        let out = run_driver(session_many(1, false, "mock-counted"), r, f).unwrap();
        assert_eq!(out.jobs.len(), 2);
        assert_eq!(out.jobs[0].terminal, Some("failed"));
        let sent: Vec<String> = out.adapters[0].requests().unwrap().into_iter().map(|r| r.idempotency_key).collect();
        assert!(sent.iter().any(|k| k == BASE), "the first attempt reached the provider");
        assert_eq!(sent.iter().any(|k| k != BASE), requeue_sent, "USD {accepted}: {sent:?}");
        let log = out.budget.log.lock().unwrap();
        let first = log.entries().find(|e| e.idempotency_key == BASE).unwrap();
        assert_eq!((first.cost.micros, first.status), (20, nodecraft_exec::cost::Charge::Unresolved), "the possibly billed attempt keeps its charge");
        assert_eq!(log.entries().count(), if requeue_sent { 2 } else { 1 }, "a refused requeue reserves nothing");
        if !requeue_sent {
            assert_eq!(out.jobs[1].terminal, Some("failed"));
        }
    }
}

#[test]
fn each_currency_has_its_own_ceiling_through_the_driver() {
    use nodecraft_types::{Currency, Money};
    // A job priced EUR 20 with plenty of USD accepted: after a possibly billed attempt, the
    // requeue must fit the EUR amount, and the USD amount doesn't help.
    // The mock catalog with mock-counted priced in EUR.
    let euro_catalog = || {
        let base = nodecraft_testkit::catalog::mock_catalog().unwrap();
        let models = base.models().cloned().map(|mut m| {
            if m.id == "mock-counted" {
                m.pricing = nodecraft_catalog::Pricing::PerUnit {
                    unit: nodecraft_catalog::Unit::Image,
                    price: nodecraft_catalog::Price(Money::new(Currency::EUR, 10)),
                    count: Some("count".into()),
                    default_count: None,
                };
            }
            m
        });
        nodecraft_catalog::Catalog::from_models(models.collect::<Vec<_>>()).unwrap()
    };
    for (accepted, second) in [(20, "failed"), (40, "done")] {
        let mut f = fixture(1);
        f.catalog = Some(euro_catalog());
        f.record = false;
        f.adapters[0].poll_open = false;
        f.actions = vec![
            Action { trigger: Trigger::Update(JobId(1), "submitted"), kind: ActionKind::Cancel(vec![JobId(1)]) },
            Action {
                trigger: Trigger::Event(JobId(1), "cancelled"),
                kind: ActionKind::Requeue { job: JobId(1), key: "ceiling-second".into(), replacement: None },
            },
        ];
        if second == "done" {
            f.actions.push(Action { trigger: Trigger::Epoch(JobId(1), 1, "submitted"), kind: ActionKind::PollGate(0) });
        }
        let mut r = request();
        r.amounts.insert("EUR".into(), accepted);
        r.amounts.insert("USD".into(), 1_000_000);
        let out = run_driver(session_many(1, false, "mock-counted"), r, f).unwrap();
        assert_eq!(out.jobs.len(), 2);
        assert_eq!(out.jobs[1].terminal, Some(second), "EUR {accepted}");
        let log = out.budget.log.lock().unwrap();
        assert!(log.entries().all(|e| e.cost.currency == Currency::EUR));
        assert_eq!(log.entries().count(), if second == "done" { 2 } else { 1 });
        let first = log.entries().find(|e| e.idempotency_key == BASE).unwrap();
        assert_eq!(first.status, nodecraft_exec::cost::Charge::Unresolved, "the cancelled-after-submit attempt keeps its charge");
        assert_eq!(out.adapters[0].counters().submits, if second == "done" { 2 } else { 1 });
    }
}

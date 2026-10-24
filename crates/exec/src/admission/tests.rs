use super::*;
use crate::{
    planner::Step,
    run::{Run, RunOptions},
    scheduler::RetryPolicy,
};
use nodecraft_catalog::{Price, Unit};
use nodecraft_graph::{Canvas, Change, Port, Project};
use nodecraft_nodes::{GenerateOutput, ModeId, NodeDescriptor, NodeKind, PlanCtx};
use nodecraft_types::{Currency, Modality, ParamSpec, PortType, ProjectId, Unsigned, VisibleIf};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
fn spec(name: &str, control: ParamControl, default: Value) -> ParamSpec {
    ParamSpec { name: name.into(), label: name.into(), description: None, control, required: false, default: Some(default), visible_if: None }
}
fn schema(count: u64) -> ParamSchema {
    let mut hidden = spec("hidden", ParamControl::Unsigned { min: None, max: Some(Unsigned(100)), step: Unsigned(1) }, Value::Unsigned(Unsigned(5)));
    hidden.visible_if = Some(VisibleIf { parameter: "toggle".into(), equals: Value::Bool(true) });
    ParamSchema {
        params: vec![
            spec("toggle", ParamControl::Toggle, Value::Bool(false)),
            spec("count", ParamControl::Unsigned { min: Some(Unsigned(1)), max: Some(Unsigned(100)), step: Unsigned(1) }, Value::Unsigned(Unsigned(count))),
            hidden,
        ],
    }
}
fn catalog(count: u64) -> Catalog {
    let model = ModelDescriptor {
        parameter_contract: None,
        id: "selected".into(),
        provider: "mock".into(),
        provider_model: "remote".into(),
        label: "Fixture".into(),
        description: "Fixture".into(),
        modes: vec![nodecraft_catalog::ModeSpec {
            id: "generate".into(),
            output: Modality::Text,
            inputs: vec![],
            params: None,
            audio: None,
            edit: None,
            document: None,
        }],
        params: schema(count),
        pricing: Pricing::PerUnit { unit: Unit::Image, price: Price(Money::new(Currency::USD, 10)), count: Some("count".into()), default_count: None },
        usage: None,
        latency_s: 0,
        quality: 1,
        style_family: None,
        cards: true,
    };
    Catalog::from_models(vec![model]).unwrap()
}
struct Kind(NodeDescriptor);
impl NodeKind for Kind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("generate").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let mut job = job(ctx.node.id, ParamValues::new());
        if let JobSpec::Generate { instance, .. } = &mut job {
            *instance = ctx.targets.get(ctx.node.id, "mock").and_then(|t| t.instance());
        }
        Ok(vec![job])
    }
}
fn fixture() -> (Canvas, Registry) {
    let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    let mut params = schema(2);
    params.params.insert(0, spec("model", ParamControl::Model, Value::Text("selected".into())));
    let descriptor = NodeDescriptor {
        id: "fixture.generate".into(),
        label: "Fixture".into(),
        category: Category::Generation,
        inputs: vec![],
        outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } }],
        params,
        quick_add: None,
    };
    let mut node = Node::new(id(1), &descriptor.id);
    node.outputs = descriptor.outputs.clone();
    Change::AddNode { canvas: C, node }.apply(&mut p).unwrap();
    let mut kinds = Registry::builtins().unwrap();
    kinds.register(Box::new(Kind(descriptor))).unwrap();
    (p.canvases().get(&C).unwrap().clone(), kinds)
}
fn job(node: NodeId, params: ParamValues) -> JobSpec {
    JobSpec::Generate {
        instance: None,
        node,
        outputs: vec![GenerateOutput { port: "out".into(), modality: Modality::Text, style_family: None }],
        provider: "mock".into(),
        model: "remote".into(),
        params,
        inputs: vec![],
    }
}
fn plan(jobs: Option<Vec<JobSpec>>) -> RunPlan {
    RunPlan {
        steps: vec![Step {
            node: id(1),
            kind: "fixture.generate".into(),
            mode: ModeId::new("generate").unwrap(),
            after: vec![],
            inputs: vec![],
            jobs,
            fanout: None,
            variations: 1,
        }],
        edges: vec![],
        inner: Default::default(),
    }
}
fn options() -> RunOptions {
    RunOptions { id: [3; 16], first_job: 1, limits: RunLimits::default(), retry: RetryPolicy::NEVER }
}
#[test]
fn canonical_actual_work_fills_defaults_strips_hidden_and_uses_remote_alias() {
    let (canvas, kinds) = fixture();
    let catalog = catalog(2);
    let (admitted, quote) =
        admit_run(plan(Some(vec![job(id(1), [("hidden".into(), Value::Unsigned(Unsigned(99)))].into())])), C, canvas, &kinds, &catalog, Default::default())
            .unwrap();
    assert_eq!(quote.totals.get(&Currency::USD).unwrap().micros, 20);
    let (run, progress) = Run::start_admitted(admitted, &kinds, options()).unwrap();
    let JobSpec::Generate { model, params, .. } = run.work(progress.jobs[0].id).unwrap() else { panic!() };
    assert_eq!(model, "remote");
    assert_eq!(params.get("count"), Some(&Value::Unsigned(Unsigned(2))));
    assert!(!params.contains_key("hidden"));
}
#[test]
fn a_result_records_the_settings_its_job_was_submitted_with() {
    let (canvas, kinds) = fixture();
    let catalog = catalog(2);
    let (admitted, _) =
        admit_run(plan(Some(vec![job(id(1), [("hidden".into(), Value::Unsigned(Unsigned(99)))].into())])), C, canvas, &kinds, &catalog, Default::default())
            .unwrap();
    let (mut run, progress) = Run::start_admitted(admitted, &kinds, options()).unwrap();
    let JobSpec::Generate { params, inputs, .. } = run.work(progress.jobs[0].id).unwrap().clone() else { panic!() };
    let done = run.job_done(&kinds, progress.jobs[0].id, vec![("out".into(), Value::Text("made".into()))]);
    let settings = &done.finished[0].generation.settings;
    // Exactly what was sent: the remote model id and the canonical parameters, the hidden
    // value stripped and the defaults filled in, and the inputs it was given.
    let inputs = inputs.into_iter().map(|input| nodecraft_graph::JobInput { role: input.role, value: input.value }).collect();
    assert_eq!(
        settings,
        &vec![nodecraft_graph::JobSettings {
            provider: "mock".into(),
            instance: None,
            model: "remote".into(),
            params: params.clone(),
            inputs,
            item: None,
            billable_units: None
        }]
    );
    assert_eq!(params.get("count"), Some(&Value::Unsigned(Unsigned(2))));
    assert!(!params.contains_key("hidden"));
}
#[test]
fn full_vector_rejects_late_mismatch_before_first_canonical_copy() {
    let (canvas, kinds) = fixture();
    let catalog = catalog(2);
    for wrong in ["count", "unknown", "hidden"] {
        CANONICAL_COPIES.with(|n| n.set(0));
        let jobs = vec![job(id(1), Default::default()), job(id(1), [(wrong.into(), Value::Unsigned(Unsigned(101)))].into())];
        assert!(admit_run(plan(Some(jobs)), C, canvas.clone(), &kinds, &catalog, Default::default()).is_err());
        assert_eq!(CANONICAL_COPIES.with(|n| n.get()), 0);
    }
    for field in [0, 1] {
        let mut j = job(id(1), Default::default());
        if let JobSpec::Generate { provider, model, .. } = &mut j {
            if field == 0 { *provider = "other".into() } else { *model = "selected".into() }
        }
        assert!(admit_run(plan(Some(vec![j])), C, canvas.clone(), &kinds, &catalog, Default::default()).is_err());
    }
}
#[test]
fn finite_work_witness_snapshot_and_transfer_limits_are_exact() {
    let (canvas, kinds) = fixture();
    let catalog = catalog(2);
    let make = || plan(Some(vec![job(id(1), Default::default())]));
    let (a, _) = admit_run(make(), C, canvas.clone(), &kinds, &catalog, Default::default()).unwrap();
    let work = a.usage.bytes;
    let step_work = a.usage.steps.get(&id(1)).copied().unwrap();
    assert_eq!(work, step_work + size_of::<RunPlan>() + size_of::<Usage>(), "fixed roots are global, charged exactly once");
    let witness = a.witness.bytes;
    let snap = measure::canvas(&canvas).unwrap();
    let mut limits = AdmissionLimits::default();
    limits.work.step_bytes = step_work;
    limits.work.run_bytes = work;
    limits.witness_bytes = witness;
    limits.snapshot_bytes = snap;
    let (a, _) = admit_run(make(), C, canvas.clone(), &kinds, &catalog, limits).unwrap();
    let mut tighter = options();
    tighter.limits.run_bytes = work - 1;
    assert!(Run::start_admitted(a, &kinds, tighter).is_err());
    for field in 0..4 {
        let mut l = limits;
        match field {
            0 => l.work.step_bytes -= 1,
            1 => l.work.run_bytes -= 1,
            2 => l.witness_bytes -= 1,
            _ => l.snapshot_bytes -= 1,
        };
        assert!(admit_run(make(), C, canvas.clone(), &kinds, &catalog, l).is_err());
    }
    let mut l = limits;
    l.scratch_bytes = 1;
    assert!(admit_run(make(), C, canvas, &kinds, &catalog, l).is_err());
}
#[test]
fn free_and_usage_validate_required_visible_values() {
    let (canvas, kinds) = fixture();
    for pricing in [Pricing::Free, Pricing::ByUsage { currency: Currency::USD }] {
        let mut catalog = catalog(2);
        let mut model = catalog.get("selected").unwrap().clone();
        model.pricing = pricing;
        model.params.params[1].required = true;
        model.params.params[1].default = None;
        catalog = Catalog::from_models(vec![model]).unwrap();
        assert!(admit_run(plan(Some(vec![job(id(1), Default::default())])), C, canvas.clone(), &kinds, &catalog, Default::default()).is_err());
    }
}
#[test]
fn deferred_actual_jobs_share_admission_and_source_category_cannot_bypass() {
    let (canvas, kinds) = fixture();
    let catalog = catalog(2);
    let (a, _) = admit_run(plan(None), C, canvas.clone(), &kinds, &catalog, Default::default()).unwrap();
    let (_, p) = Run::start_admitted(a, &kinds, options()).unwrap();
    assert_eq!(p.jobs.len(), 1);
    let (_, p) = Run::start(&plan(None), C, &canvas, &kinds, options()).unwrap();
    assert!(p.jobs.is_empty());
    assert_eq!(p.failed.len(), 1);
}
#[test]
fn value_and_map_logical_formula_counts_payload_once() {
    let v = Value::List(vec![Value::Text("abc".into())]);
    assert_eq!(measure::values(&v).unwrap(), size_of::<Value>() + 3);
    let map = ParamValues::from([("x".into(), v)]);
    assert_eq!(measure::params(&map).unwrap(), size_of::<(String, Value)>() + measure::LINK + 1 + size_of::<Value>() + 3);
}

pub(crate) fn free_catalog(id: &str, provider: &str, remote: &str) -> Catalog {
    Catalog::from_models([ModelDescriptor {
        parameter_contract: None,
        id: id.into(),
        provider: provider.into(),
        provider_model: remote.into(),
        label: "Fixture".into(),
        description: "Fixture".into(),
        modes: vec![nodecraft_catalog::ModeSpec {
            id: "generate".into(),
            output: Modality::Text,
            inputs: vec![],
            params: None,
            audio: None,
            edit: None,
            document: None,
        }],
        params: ParamSchema::default(),
        pricing: Pricing::Free,
        usage: None,
        latency_s: 0,
        quality: 1,
        style_family: None,
        cards: true,
    }])
    .unwrap()
}

#[test]
fn aliases_are_unambiguous_and_hidden_model_controls_still_price() {
    let (canvas, kinds) = fixture();
    for conflict in [false, true] {
        let mut desc = kinds.get("fixture.generate").unwrap().descriptor().clone();
        let mut model2 = spec("model2", ParamControl::Model, Value::Text("alias".into()));
        model2.visible_if = Some(VisibleIf { parameter: "toggle".into(), equals: Value::Bool(true) });
        desc.params.params.push(model2);
        let mut kinds = Registry::builtins().unwrap();
        kinds.register(Box::new(Kind(desc))).unwrap();
        let first = catalog(2).get("selected").unwrap().clone();
        let mut alias = first.clone();
        alias.id = "alias".into();
        if conflict {
            alias.params = schema(3);
        }
        let catalog = Catalog::from_models([first, alias]).unwrap();
        let result = admit_run(plan(Some(vec![job(id(1), Default::default())])), C, canvas.clone(), &kinds, &catalog, Default::default());
        if conflict {
            assert!(result.is_err());
        } else {
            let (a, q) = result.unwrap();
            assert_eq!(q.totals.get(&Currency::USD).unwrap().micros, 40);
            assert_eq!(a.witness.models.len(), 2);
        }
    }
}
#[test]
fn usage_priced_valid_jobs_are_admitted_as_partial_and_free_jobs_still_canonical() {
    let (canvas, kinds) = fixture();
    for price in [Pricing::Free, Pricing::ByUsage { currency: Currency::USD }] {
        let mut model = catalog(2).get("selected").unwrap().clone();
        model.pricing = price.clone();
        let catalog = Catalog::from_models([model]).unwrap();
        let (a, q) = admit_run(plan(Some(vec![job(id(1), Default::default())])), C, canvas.clone(), &kinds, &catalog, Default::default()).unwrap();
        assert_eq!(q.is_complete(), price == Pricing::Free);
        assert!(crate::cost::check(&q, &Default::default(), &Default::default()).unwrap().allowed());
        let (run, p) = Run::start_admitted(a, &kinds, options()).unwrap();
        assert!(matches!(run.work(p.jobs[0].id),Some(JobSpec::Generate{params,..})if params.contains_key("count")));
    }
}
#[test]
fn source_category_future_generation_never_gains_a_legacy_witness() {
    let (canvas, kinds) = fixture();
    let mut d = kinds.get("fixture.generate").unwrap().descriptor().clone();
    d.category = Category::Source;
    d.params.params.clear();
    let mut kinds = Registry::builtins().unwrap();
    kinds.register(Box::new(Kind(d))).unwrap();
    let (a, _) = admit_run(plan(None), C, canvas, &kinds, &catalog(2), Default::default()).unwrap();
    let (run, p) = Run::start_admitted(a, &kinds, options()).unwrap();
    assert!(p.jobs.is_empty());
    assert_eq!(p.failed.len(), 1);
    assert_eq!(run.work(crate::scheduler::JobId(1)), None);
}

#[test]
fn deferred_input_copies_and_jobs_keep_the_same_transferred_ledger() {
    use crate::planner::{InputSource, PlannedInput};
    let (canvas, kinds) = fixture();
    let catalog = catalog(2);
    let mut p = Project::new(ProjectId::from_bytes([9; 16]), "Deferred").unwrap();
    Change::AddCanvas { canvas }.apply(&mut p).unwrap();
    let source = kinds.get("source.text").unwrap().descriptor().instantiate(id(2), &[("text".into(), Value::Text("upstream".into()))].into()).unwrap();
    Change::AddNode { canvas: C, node: source }.apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let mut plan = plan(None);
    plan.steps[0].after = vec![id(2)];
    plan.steps[0].inputs = vec![PlannedInput {
        port: "in".into(),
        port_type: PortType { modality: Modality::Text, many: false },
        from: vec![InputSource::Step { node: id(2), port: "out".into(), list: false }],
        origins: vec![id(2)],
    }];
    plan.steps.insert(
        0,
        Step {
            node: id(2),
            kind: "source.text".into(),
            mode: ModeId::new("source").unwrap(),
            after: vec![],
            inputs: vec![],
            jobs: Some(vec![JobSpec::Source { node: id(2), output: "out".into(), value: Value::Text("upstream".into()) }]),
            fanout: None,
            variations: 1,
        },
    );
    plan.edges = vec![(id(2), id(1))];
    let (a, _) = admit_run(plan.clone(), C, canvas.clone(), &kinds, &catalog, Default::default()).unwrap();
    let initial = a.usage.bytes;
    let roots = size_of::<RunPlan>() + size_of::<Usage>();
    assert_eq!(initial, a.usage.steps.values().sum::<usize>() + roots);
    let (mut run, first) = Run::start_admitted(a, &kinds, options()).unwrap();
    let next = run.job_done(&kinds, first.jobs[0].id, vec![("out".into(), Value::Text("upstream".into()))]);
    assert_eq!(next.jobs.len(), 1);
    let final_bytes = run.admission.as_ref().unwrap().1.bytes;
    assert!(final_bytes > initial);
    assert_eq!(
        final_bytes,
        run.admission.as_ref().unwrap().1.steps.values().sum::<usize>() + roots,
        "transfer/deferred work must not add a second root charge"
    );
    let mut limits = AdmissionLimits::default();
    limits.work.run_bytes = final_bytes - 1;
    let (a, _) = admit_run(plan, C, canvas.clone(), &kinds, &catalog, limits).unwrap();
    let (mut run, first) = Run::start_admitted(a, &kinds, options()).unwrap();
    CANONICAL_COPIES.with(|n| n.set(0));
    let next = run.job_done(&kinds, first.jobs[0].id, vec![("out".into(), Value::Text("upstream".into()))]);
    assert!(next.jobs.is_empty());
    assert_eq!(next.failed.len(), 1);
    assert_eq!(CANONICAL_COPIES.with(|n| n.get()), 0);
    assert!(run.admission.as_ref().unwrap().1.bytes > initial, "materialized inputs remain charged after job refusal");
}

#[test]
fn report_and_typed_metadata_are_reserved_before_ownership() {
    let (canvas, kinds) = fixture();
    let catalog = catalog(2);
    let (a, _) = admit_run(plan(Some(vec![job(id(1), Default::default())])), C, canvas.clone(), &kinds, &catalog, Default::default()).unwrap();
    let limits = AdmissionLimits { witness_bytes: a.witness.bytes + 64, ..Default::default() };
    let (mut a, _) = admit_run(plan(Some(vec![job(id(1), Default::default())])), C, canvas.clone(), &kinds, &catalog, limits).unwrap();
    a.reserve_metadata(64).unwrap();
    assert!(a.reserve_metadata(1).is_err());
    let mut large = plan(None);
    large.steps = vec![large.steps[0].clone(); 10_000];
    QUOTE_COPIES.with(|n| n.set(0));
    assert!(quote(&large, &canvas, &kinds, &catalog).is_err());
    assert_eq!(QUOTE_COPIES.with(|n| n.get()), 0, "no quoted-model copy before full report admission");
}

#[test]
fn independent_review275_work_roots_must_fit_even_empty_plan() {
    let (canvas, kinds) = fixture();
    let mut limits = AdmissionLimits::default();
    limits.work.run_bytes = 0;
    limits.work.step_bytes = 0;
    let result = admit_run(RunPlan::default(), C, canvas, &kinds, &Catalog::default(), limits);
    println!("empty admitted with zero work budget: {}", result.is_ok());
    assert!(result.is_err(), "RunPlan/Usage retained roots require nonzero logical work ownership");
}

#[test]
fn empty_root_work_boundary_is_global_exact_and_transferred() {
    let (canvas, kinds) = fixture();
    let roots = measure::add(size_of::<RunPlan>(), size_of::<Usage>()).unwrap();
    for limit in [0, roots - 1, roots] {
        let mut limits = AdmissionLimits::default();
        limits.work.run_bytes = limit;
        limits.work.step_bytes = 0;
        QUOTE_COPIES.with(|n| n.set(0));
        CANONICAL_COPIES.with(|n| n.set(0));
        let result = admit_run(RunPlan::default(), C, canvas.clone(), &kinds, &Catalog::default(), limits);
        if limit < roots {
            assert!(result.is_err());
            assert_eq!(QUOTE_COPIES.with(|n| n.get()), 0);
            assert_eq!(CANONICAL_COPIES.with(|n| n.get()), 0);
        } else {
            let (a, _) = result.unwrap();
            assert_eq!(a.usage.bytes, roots);
            assert!(a.usage.steps.is_empty());
            let (too_tight, _) = admit_run(RunPlan::default(), C, canvas.clone(), &kinds, &Catalog::default(), limits).unwrap();
            let mut opt = options();
            opt.limits.run_bytes = roots - 1;
            assert!(Run::start_admitted(too_tight, &kinds, opt).is_err());
            let (run, p) = Run::start_admitted(a, &kinds, options()).unwrap();
            assert!(p.jobs.is_empty());
            assert_eq!(run.admission.as_ref().unwrap().1.bytes, roots);
        }
    }
}

// Existing catalog controls now explicitly select their default endpoint.
fn admit_run(
    plan: RunPlan,
    canvas_id: CanvasId,
    canvas: Canvas,
    kinds: &Registry,
    catalog: &Catalog,
    limits: AdmissionLimits,
) -> Result<(AdmittedRunPlan, Quote), Error> {
    let mut targets = nodecraft_types::TargetSelection::empty();
    for node in canvas.nodes().keys() {
        targets.insert(*node, &nodecraft_types::ProviderTarget::default_instance("mock")?)?;
    }
    super::admit_run_with_targets(plan, canvas_id, canvas, kinds, catalog, limits, &targets)
}

#[test]
fn endpoint_eager_manual_deferred_and_late_mismatch_share_the_same_selection() {
    let (canvas, kinds) = fixture();
    let catalog = catalog(2);
    let mut targets = nodecraft_types::TargetSelection::empty();
    let named = nodecraft_types::ProviderTarget::named("mock", nodecraft_types::ProviderInstanceId::from_bytes([0; 16])).unwrap();
    targets.insert(id(1), &named).unwrap();
    let planned =
        crate::planner::plan_with_targets(&canvas, &kinds, &crate::planner::RunMode::Selected, &[id(1)].into(), &Default::default(), &targets).unwrap();
    let crate::planner::Planned::Ready(eager) = planned else { panic!("named eager plan blocked") };
    assert!(matches!(&eager.steps[0].jobs.as_ref().unwrap()[0], JobSpec::Generate { instance: Some(_), .. }));
    let (admitted, _) = super::admit_run_with_targets(eager, C, canvas.clone(), &kinds, &catalog, Default::default(), &targets).unwrap();
    let (run, progress) = Run::start_admitted(admitted, &kinds, options()).unwrap();
    assert_eq!(progress.jobs.len(), 1);
    assert!(matches!(run.work(progress.jobs[0].id), Some(JobSpec::Generate { instance: Some(_), .. })));
    let mut good = job(id(1), Default::default());
    if let JobSpec::Generate { instance, .. } = &mut good {
        *instance = named.instance();
    }
    for jobs in [vec![job(id(1), Default::default())], vec![good.clone(), job(id(1), Default::default())]] {
        CANONICAL_COPIES.with(|n| n.set(0));
        assert!(super::admit_run_with_targets(plan(Some(jobs)), C, canvas.clone(), &kinds, &catalog, Default::default(), &targets).is_err());
        assert_eq!(CANONICAL_COPIES.with(|n| n.get()), 0);
    }
    let (admitted, _) = super::admit_run_with_targets(plan(None), C, canvas.clone(), &kinds, &catalog, Default::default(), &targets).unwrap();
    drop(targets);
    let (run, progress) = Run::start_admitted(admitted, &kinds, options()).unwrap();
    assert_eq!(progress.jobs.len(), 1);
    assert!(matches!(run.work(progress.jobs[0].id), Some(JobSpec::Generate { instance: Some(_), .. })));
    assert!(super::admit_run(plan(Some(vec![good])), C, canvas, &kinds, &catalog, Default::default()).is_err(), "legacy empty view must not admit generation");
}

#[test]
fn selected_target_witness_refuses_before_target_copies_at_the_actual_limit() {
    let (canvas, kinds) = fixture();
    let catalog = catalog(2);
    let mut targets = nodecraft_types::TargetSelection::empty();
    targets.insert(id(1), &nodecraft_types::ProviderTarget::default_instance("mock").unwrap()).unwrap();
    let limits = AdmissionLimits { witness_bytes: 1, ..Default::default() };
    TARGET_COPIES.with(|n| n.set(0));
    assert!(super::admit_run_with_targets(plan(Some(vec![job(id(1), Default::default())])), C, canvas, &kinds, &catalog, limits, &targets).is_err());
    assert_eq!(TARGET_COPIES.with(|n| n.get()), 0);
}

struct WrongTargetVector(NodeDescriptor);
impl NodeKind for WrongTargetVector {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("generate").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let mut first = job(ctx.node.id, Default::default());
        if let JobSpec::Generate { instance, .. } = &mut first {
            *instance = ctx.targets.get(ctx.node.id, "mock").and_then(|t| t.instance());
        }
        Ok(vec![first, job(ctx.node.id, Default::default())])
    }
}
#[test]
fn eager_and_deferred_late_wrong_target_emit_no_accepted_prefix() {
    let (canvas, old_kinds) = fixture();
    let descriptor = old_kinds.get("fixture.generate").unwrap().descriptor().clone();
    let mut kinds = Registry::builtins().unwrap();
    kinds.register(Box::new(WrongTargetVector(descriptor))).unwrap();
    let mut targets = nodecraft_types::TargetSelection::empty();
    targets.insert(id(1), &nodecraft_types::ProviderTarget::named("mock", nodecraft_types::ProviderInstanceId::from_bytes([0; 16])).unwrap()).unwrap();
    assert!(matches!(
        crate::planner::plan_with_targets(&canvas, &kinds, &crate::planner::RunMode::Selected, &[id(1)].into(), &Default::default(), &targets).unwrap(),
        crate::planner::Planned::Blocked(_)
    ));
    let (admitted, _) = super::admit_run_with_targets(plan(None), C, canvas, &kinds, &catalog(2), Default::default(), &targets).unwrap();
    CANONICAL_COPIES.with(|n| n.set(0));
    let (run, progress) = Run::start_admitted(admitted, &kinds, options()).unwrap();
    assert!(progress.jobs.is_empty());
    assert_eq!(progress.failed.len(), 1);
    assert!(run.is_finished());
    assert_eq!(CANONICAL_COPIES.with(|n| n.get()), 0);
}

/// A semantic model whose one setting is a seed drawn per run (#162).
fn seeded() -> Catalog {
    let schema = ParamSchema {
        params: vec![ParamSpec {
            name: "seed".into(),
            label: "Seed".into(),
            description: None,
            control: ParamControl::Seed { max: Some(Unsigned(999)) },
            required: false,
            default: None,
            visible_if: None,
        }],
    };
    let mut contract = nodecraft_types::Contract::from_schema("selected", nodecraft_types::ParamOwner::Model, schema.clone());
    contract
        .rules
        .push(nodecraft_types::ParamRule { default: Some(nodecraft_types::DefaultPolicy::RandomSeed), ..nodecraft_types::ParamRule::implied("seed") });
    let mut catalog = catalog(2).models().next().unwrap().clone();
    catalog.params = schema;
    catalog.parameter_contract = Some(contract);
    catalog.pricing = Pricing::PerUnit { unit: Unit::Image, price: Price(Money::new(Currency::USD, 10)), count: None, default_count: Some(1) };
    Catalog::from_models(vec![catalog]).unwrap()
}
#[test]
fn a_random_seed_is_drawn_per_run_within_its_bound_and_recorded() {
    let drawn = |jobs: Option<Vec<JobSpec>>, run: u8| {
        let (canvas, kinds) = fixture();
        let (admitted, _) = admit_run(plan(jobs), C, canvas, &kinds, &seeded(), Default::default()).unwrap();
        let (mut run, progress) = Run::start_admitted(admitted, &kinds, RunOptions { id: [run; 16], ..options() }).unwrap();
        let JobSpec::Generate { params, .. } = run.work(progress.jobs[0].id).unwrap().clone() else { panic!() };
        let done = run.job_done(&kinds, progress.jobs[0].id, vec![("out".into(), Value::Text("made".into()))]);
        assert_eq!(done.finished[0].generation.settings[0].params, params, "the result records the seed it was sent");
        let Some(Value::Unsigned(seed)) = params.get("seed") else { panic!("no seed drawn: {params:?}") };
        assert!(seed.0 <= 999);
        *seed
    };
    let eager = |run| drawn(Some(vec![job(id(1), ParamValues::new())]), run);
    let lazy = |run| drawn(None, run);
    assert_eq!(eager(3), eager(3), "a run's draw is fixed, so a retry sends the same seed");
    assert_eq!(eager(3), lazy(3), "planned now or at admission, the same job draws the same seed");
    assert_ne!(eager(3), eager(4), "another run draws afresh");
}
#[test]
fn each_job_of_a_step_draws_its_own_seed() {
    let (canvas, kinds) = fixture();
    let jobs = vec![job(id(1), ParamValues::new()), job(id(1), ParamValues::new())];
    let (admitted, _) = admit_run(plan(Some(jobs)), C, canvas, &kinds, &seeded(), Default::default()).unwrap();
    let (run, progress) = Run::start_admitted(admitted, &kinds, options()).unwrap();
    let sent: Vec<_> = progress.jobs.iter().map(|j| run.work(j.id).unwrap().clone()).collect();
    let seeds: Vec<_> =
        sent.iter().map(|work| if let JobSpec::Generate { params, .. } = work { params.get("seed").cloned().unwrap() } else { panic!() }).collect();
    assert_ne!(seeds[0], seeds[1]);
}

/// A job takes a mode's narrower settings only when its inputs fit that mode (#559): a
/// text-to-video job that leaves out `duration` doesn't pass as the speech mode's submission.
#[test]
fn a_narrower_submission_admits_only_jobs_planned_for_its_mode() {
    use nodecraft_nodes::GenerateInput;
    let selected = Selected {
        provider: "mock".into(),
        remote: "remote".into(),
        schema: ParamSchema::default(),
        semantic: false,
        // text-to-video: an inline prompt; speech-to-video: prompt, portrait and speech.
        modes: vec![vec![("prompt".into(), 0, 1)], vec![("prompt".into(), 0, 1), ("portrait".into(), 1, 1), ("speech".into(), 1, 1)]],
    };
    let full: ParamValues = [("duration".into(), Value::Unsigned(Unsigned(5))), ("seed".into(), Value::Unsigned(Unsigned(7)))].into();
    let narrow: ParamValues = [("seed".into(), Value::Unsigned(Unsigned(7)))].into();
    let allowed = Allowed { id: "m".into(), expected: full.clone(), seeds: vec![], narrowed: vec![(1, narrow.clone(), vec![])] };
    let input = |role: &str| GenerateInput { role: role.into(), value: Value::Text(String::new()) };

    let (expected, _, narrowed) = allowed.submission(&selected, &[input("prompt")]);
    assert_eq!((expected, narrowed), (&full, false), "text to video is admitted with every setting");
    let (expected, _, narrowed) = allowed.submission(&selected, &[input("prompt"), input("portrait"), input("speech")]);
    assert_eq!((expected, narrowed), (&narrow, true), "the speech mode's job sends only its settings");
    // Inputs that fit no mode, or a role a mode lacks, fall back to the full submission.
    assert_eq!(selected.mode_of(&[input("speech")]), None);
    assert_eq!(selected.mode_of(&[input("prompt"), input("reference")]), None);
}

use crate::planner::{InputSource, PlannedInput};
fn clip(ms: Option<u64>) -> Value {
    let meta = nodecraft_types::MediaMeta { duration_ms: ms, ..Default::default() };
    Value::Media(nodecraft_types::AssetRef { hash: nodecraft_types::Blake3::from_bytes([7; 32]), mime: "video/mp4".parse().unwrap(), meta })
}
fn source_input(from: InputSource) -> Vec<PlannedInput> {
    vec![PlannedInput { port: "source".into(), port_type: PortType { modality: Modality::Video, many: false }, from: vec![from], origins: vec![id(2)] }]
}

#[test]
fn a_source_edit_is_quoted_at_its_longest_known_source() {
    let mut step = plan(None).steps.remove(0);
    step.inputs = source_input(InputSource::Value(clip(Some(5_000))));
    assert_eq!(source_ms(&step, "source"), Some(5_000));
    assert_eq!(source_ms(&step, "other"), None);
    // A one-item list is that item.
    step.inputs = source_input(InputSource::Value(Value::List(vec![clip(Some(4_200))])));
    assert_eq!(source_ms(&step, "source"), Some(4_200));
    // A source made earlier in the run isn't known yet.
    step.inputs = source_input(InputSource::Step { node: id(2), port: "out".into(), list: false });
    assert_eq!(source_ms(&step, "source"), None);
    // A fanned step is quoted at its longest item, and one unknown item leaves it unknown.
    let cells = |lengths: &[Option<u64>]| lengths.iter().map(|ms| source_input(InputSource::Value(clip(*ms)))).collect();
    step.fanout = Some(crate::planner::Fanout { items: vec![], cells: cells(&[Some(4_000), Some(31_000), Some(9_000)]), jobs: vec![], resume: None });
    assert_eq!(source_ms(&step, "source"), Some(31_000));
    step.fanout = Some(crate::planner::Fanout { items: vec![], cells: cells(&[Some(4_000), None]), jobs: vec![], resume: None });
    assert_eq!(source_ms(&step, "source"), None);
}

fn pdf(pages: Option<u32>) -> Value {
    let meta = nodecraft_types::MediaMeta { page_count: pages, ..Default::default() };
    Value::Media(nodecraft_types::AssetRef { hash: nodecraft_types::Blake3::from_bytes([9; 32]), mime: "application/pdf".parse().unwrap(), meta })
}
fn document_input(from: Vec<InputSource>) -> Vec<PlannedInput> {
    vec![PlannedInput { port: "document".into(), port_type: PortType { modality: Modality::Document, many: true }, from, origins: vec![id(2)] }]
}

#[test]
fn wired_pdfs_are_quoted_at_their_counted_pages() {
    let reads = |role: &str| nodecraft_types::DocumentUse { role: role.into(), max_pages: 100, tokens_per_page: 3000 };
    let mut step = plan(None).steps.remove(0);
    step.inputs = document_input(vec![InputSource::Value(pdf(Some(12))), InputSource::Value(Value::List(vec![pdf(Some(3)), pdf(Some(5))]))]);
    assert_eq!(document_pages(&step, &reads("document")), 20, "every wired PDF's pages count");
    assert_eq!(document_pages(&step, &reads("other")), 0);
    // An uncounted PDF, or one made earlier in the run, is quoted at the most the mode reads.
    step.inputs = document_input(vec![InputSource::Value(pdf(Some(12))), InputSource::Value(pdf(None))]);
    assert_eq!(document_pages(&step, &reads("document")), 100);
    step.inputs = document_input(vec![InputSource::Step { node: id(2), port: "out".into(), list: false }]);
    assert_eq!(document_pages(&step, &reads("document")), 100);
    step.inputs = document_input(vec![InputSource::Value(pdf(Some(u32::MAX))), InputSource::Value(pdf(Some(u32::MAX)))]);
    assert_eq!(document_pages(&step, &reads("document")), 100, "hostile counts are capped");
    // A fanned step is quoted at its longest item.
    let cells = |pages: &[Option<u32>]| pages.iter().map(|p| document_input(vec![InputSource::Value(pdf(*p))])).collect();
    step.fanout = Some(crate::planner::Fanout { items: vec![], cells: cells(&[Some(4), Some(40), Some(9)]), jobs: vec![], resume: None });
    assert_eq!(document_pages(&step, &reads("document")), 40);
    step.fanout = Some(crate::planner::Fanout { items: vec![], cells: cells(&[Some(4), None]), jobs: vec![], resume: None });
    assert_eq!(document_pages(&step, &reads("document")), 100);
}

#[test]
fn known_character_inputs_are_priced_at_admission_and_allow_an_exact_budget() {
    let (canvas, kinds) = fixture();
    let mut model = catalog(2).get("selected").unwrap().clone();
    model.provider = "elevenlabs".into();
    model.pricing = Pricing::PerUnit { unit: Unit::ThousandCharacters, price: Price(Money::new(Currency::USD, 80_000)), count: None, default_count: None };
    let catalog = Catalog::from_models(vec![model]).unwrap();
    let mut work = job(id(1), Default::default());
    if let JobSpec::Generate { provider, inputs, .. } = &mut work {
        *provider = "elevenlabs".into();
        inputs.push(nodecraft_nodes::GenerateInput { role: "prompt".into(), value: Value::Text("雪é🙂".into()) });
    }
    let quote = quote(&plan(Some(vec![work])), &canvas, &kinds, &catalog).unwrap();
    assert!(quote.is_complete());
    assert_eq!(quote.totals[&Currency::USD].micros, 240);
    let caps = crate::cost::Budgets { per_call: Some(Money::new(Currency::USD, 240)), ..Default::default() };
    assert!(crate::cost::check(&quote, &caps, &Default::default()).unwrap().allowed());
    let low = crate::cost::Budgets { per_call: Some(Money::new(Currency::USD, 239)), ..Default::default() };
    assert!(!crate::cost::check(&quote, &low, &Default::default()).unwrap().allowed());
    let deferred = super::quote(&plan(None), &canvas, &kinds, &catalog).unwrap();
    assert!(!deferred.is_complete());
    assert!(!crate::cost::check(&deferred, &caps, &Default::default()).unwrap().allowed());
}

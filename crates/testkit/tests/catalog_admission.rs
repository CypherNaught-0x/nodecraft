//! Actual Session -> typed queue -> live authorization -> Run -> converter -> mock.
use nodecraft_engine::{
    Session,
    commands::{Registry as Commands, Resources},
    generate,
    run_cmds::{BudgetState, PreparedRun, RunCatalog},
};
use nodecraft_exec::{
    cost::{Budgets, Spent},
    run::{RunLimits, RunOptions},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Node, Port, Project};
use nodecraft_nodes::{Category, GenerateOutput, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, Registry};
use nodecraft_providers::{BoxFuture, JobHandle, JobProgress, Provider, ProviderError, ProviderJob, Submission};
use nodecraft_testkit::{catalog::mock_catalog, mock::MockProvider};
use nodecraft_types::{CanvasId, Modality, NodeId, ParamControl, ParamSpec, PortType, ProjectId, Unsigned, Value};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);
// Deliberate provider-side fallback differs from the catalog default. The wrapper
// records the actual resulting request through the unchanged deterministic mock.
struct FallbackProvider(MockProvider);
impl Provider for FallbackProvider {
    fn id(&self) -> &str {
        self.0.id()
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move {
            let mut actual = job.clone();
            actual.params.entry("count").or_insert(json!(99));
            self.0.submit(&actual).await
        })
    }
    fn poll<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        self.0.poll(h)
    }
    fn cancel<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        self.0.cancel(h)
    }
}
struct Loose {
    descriptor: NodeDescriptor,
    calls: Arc<AtomicUsize>,
    wrong: bool,
}
impl NodeKind for Loose {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("generate").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, nodecraft_types::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut params = std::collections::BTreeMap::from([("hidden".into(), Value::Unsigned(Unsigned(99)))]);
        if self.wrong {
            params.insert("count".into(), Value::Unsigned(Unsigned(7)));
        }
        Ok(vec![JobSpec::Generate {
            instance: None,
            node: ctx.node.id,
            provider: "mock".into(),
            model: "mock-text".into(),
            params,
            inputs: vec![],
            outputs: vec![GenerateOutput { port: "out".into(), modality: Modality::Text }],
        }])
    }
}
fn fixture(wrong: bool) -> (Session, Arc<Mutex<Vec<PreparedRun>>>, Arc<AtomicUsize>) {
    let catalog = mock_catalog().unwrap();
    let mut schema = catalog.get("mock-counted").unwrap().params.clone();
    schema.params.insert(
        0,
        ParamSpec {
            name: "model".into(),
            label: "Model".into(),
            description: None,
            control: ParamControl::Model,
            required: true,
            default: Some(Value::Text("mock-counted".into())),
            visible_if: None,
        },
    );
    let descriptor = NodeDescriptor {
        id: "fixture.loose".into(),
        label: "Loose".into(),
        category: Category::Generation,
        params: schema,
        inputs: vec![],
        outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } }],
        quick_add: None,
    };
    let mut node = Node::new(N, &descriptor.id);
    node.outputs = descriptor.outputs.clone(); // Deliberately omit defaults from stored values.
    let calls = Arc::new(AtomicUsize::new(0));
    let mut kinds = Registry::builtins().unwrap();
    kinds.register(Box::new(Loose { descriptor, calls: calls.clone(), wrong })).unwrap();
    let mut p = Project::new(ProjectId::from_bytes([3; 16]), "Public fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut p).unwrap();
    let mut resources = Resources::default();
    resources.insert(RunCatalog::new(catalog).unwrap());
    resources.insert(BudgetState { budgets: Budgets::default(), spent: Spent::default() });
    let mut s = Session::with_registries(p, Commands::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    let mut routing = nodecraft_engine::provider_routing::ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(MockProvider::standard())).unwrap();
    routing.select(N, "mock", binding.target()).unwrap();
    s.set_provider_routing(routing).unwrap();
    let queued = Arc::new(Mutex::new(Vec::new()));
    let q = queued.clone();
    s.register_run_host(move |p| {
        q.lock().unwrap().push(p);
        Ok(json!({"queued":true}))
    })
    .unwrap();
    (s, queued, calls)
}
fn options() -> RunOptions {
    RunOptions { id: [4; 16], first_job: 1, limits: RunLimits::default(), retry: RetryPolicy::NEVER }
}
#[test]
fn actual_wire_request_contains_catalog_default_and_no_hidden_override() {
    let (mut s, queued, calls) = fixture(false);
    let request = json!({"canvas":C,"mode":"selected","nodes":[N]});
    let quote = s.dispatch("run.quote", &request).unwrap();
    assert_eq!(quote["totals"]["USD"]["micros"], 20);
    let mut start = request;
    start["accept"] = json!({"token":quote["token"],"amounts":{"USD":20}});
    calls.store(0, Ordering::SeqCst);
    s.enabled("run.start", &start).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    s.dispatch("run.start", &start).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let prepared = queued.lock().unwrap().pop().unwrap();
    let (run, progress) = s.authorize_prepared_run(prepared, options()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let (_, job) = generate::provider_job(run.work(progress.jobs[0].id).unwrap(), "public-catalog-job").unwrap();
    // Fixture fallback99 would expose omission; canonical catalog default2 must reach wire.
    assert_eq!(job.params.get("count").and_then(|v| v.as_u64()).unwrap_or(99), 2);
    assert!(!job.params.contains_key("hidden"));
    assert_eq!(job.model, "mock-text");
    let provider = FallbackProvider(MockProvider::standard());
    assert!(matches!(futures::executor::block_on(provider.submit(&job)).unwrap(), Submission::Finished(_)));
    assert_eq!(provider.0.requests()[0].params["count"], 2);
    assert_eq!(provider.0.billed(), 1);
    let control = FallbackProvider(MockProvider::standard());
    let mut omitted = job.clone();
    omitted.params.remove("count");
    futures::executor::block_on(control.submit(&omitted)).unwrap();
    assert_eq!(control.0.requests()[0].params["count"], 99);
}
#[test]
fn visible_mismatch_blocks_even_with_partial_consent_and_zero_provider_calls() {
    let (mut s, queued, calls) = fixture(true);
    let request = json!({"canvas":C,"mode":"selected","nodes":[N]});
    let quote = s.dispatch("run.quote", &request).unwrap();
    let mut start = request;
    start["accept"] = json!({"token":quote["token"],"amounts":{"USD":1_000_000},"partial":true});
    calls.store(0, Ordering::SeqCst);
    assert!(s.dispatch("run.start", &start).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(queued.lock().unwrap().is_empty());
}

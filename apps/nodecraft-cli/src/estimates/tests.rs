//! Real CLI runner + MCP protocol estimates against the deterministic mock provider.
use super::*;
use nodecraft_automation::mcp::McpServer;
use nodecraft_engine::commands::{Registry, Resources, Services};
use nodecraft_engine::generation_catalog::GenerationCatalog;
use nodecraft_engine::provider_setup::Readiness;
use nodecraft_engine::run_cmds::RunCatalog;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_provider_testkit::mock::{MockModel, MockOutput, MockProvider};
use nodecraft_providers::Provider;
use nodecraft_types::{Currency, Money, ProjectId, Value as Output};
use std::sync::Arc;

const C: CanvasId = CanvasId::from_bytes([83; 16]);
const N: NodeId = NodeId::from_bytes([84; 16]);
const PRICE: Money = Money { currency: Currency::USD, micros: 250_000 };
const MODEL: &str = r#"
[[model]]
id = "mock.usage"
provider = "mock"
provider_model = "usage"
label = "Public mock usage fixture"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "by_usage", currency = "USD" }
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

fn fixture(price: Result<Money, nodecraft_providers::ErrorKind>) -> (Host, MockProvider) {
    fixture_with_runner(price, false)
}

fn fixture_with_runner(price: Result<Money, nodecraft_providers::ErrorKind>, executable: bool) -> (Host, MockProvider) {
    let catalog = nodecraft_catalog::Catalog::from_toml(MODEL).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([85; 16]), "Public estimates").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Output::Text("mock.usage".into())), ("prompt".into(), Output::Text("Describe the sea".into()))].into();
    let node = kinds.get(nodecraft_nodes::generation::TEXT_KIND).unwrap().instantiate_instance(N, Default::default(), &values).unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mut services = Services::default();
    let runner =
        if executable { Runner::new(&mut services, "estimate-test".into()) } else { Runner::estimates_only(&mut services, "estimate-test".into()) }.unwrap();
    let session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    let mock = MockProvider::new("mock", [("usage".into(), MockModel::new(MockOutput::Text))]).with_quote("usage", price);
    let provider = mock.clone();
    let maker = Box::new(move |adapter: &str| -> Result<Arc<dyn Provider>, Readiness> {
        assert_eq!(adapter, "mock");
        Ok(Arc::new(provider.clone()))
    });
    (Host::new(session, runner, maker), mock)
}

fn params() -> Value {
    json!({"canvas": C, "mode": "selected", "nodes": [N]})
}

#[test]
fn blocking_cli_estimate_matches_its_request_and_prices_the_following_quote() {
    let (mut host, mock) = fixture(Ok(PRICE));
    prepare(&mut host.session, &params(), &mut *host.maker, &mut host.routing).unwrap();
    host.runner.install(&mut host.session);
    let before = host.session.dispatch("run.quote", &params()).unwrap();
    assert_eq!(before["complete"], false);
    // An unrelated completion must never satisfy our own wait.
    let earlier = host.session.dispatch("run.estimate", &json!({"canvas": C, "mode": "selected"})).unwrap();
    let result = host.runner.estimate(&mut host.session, &params()).unwrap();
    assert_ne!(result["request"], earlier["request"]);
    assert_eq!(result["status"], "estimated");
    assert_eq!(result["quoted"], 1);
    assert_eq!(result["unquoted"], 0);
    let after = host.session.dispatch("run.quote", &params()).unwrap();
    assert_eq!(after["complete"], true);
    assert_eq!(after["quoted"], json!([N]));
    assert_eq!(after["totals"]["USD"]["micros"], PRICE.micros);
    assert_ne!(before["token"], after["token"]);
    assert_eq!(mock.quoted().len(), 1);
    assert!(mock.quoted().iter().all(|job| job.idempotency_key.is_empty()));
    assert!(mock.requests().is_empty());
    assert_eq!(mock.billed(), 0);
}

fn call(server: &mut McpServer, n: u64, command: &str, params: Value) -> Value {
    serde_json::from_str(
        &server
            .handle(
                &json!({"jsonrpc": "2.0", "id": n, "method": "tools/call", "params": {
                    "name": "command_run", "arguments": {"id": command, "params": params}
                }})
                .to_string(),
            )
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn mcp_returns_the_terminal_notice_and_keeps_execution_unavailable() {
    let (host, mock) = fixture(Ok(PRICE));
    let mut server = McpServer::with_host(host, Box::new(|| [86; 16]));
    let estimated = call(&mut server, 1, "run.estimate", params());
    assert_eq!(estimated["result"]["isError"], false, "{estimated}");
    let result = &estimated["result"]["structuredContent"]["result"];
    assert_eq!(result["request"], 0);
    assert_eq!(result["status"], "estimated");
    assert_eq!(result["quoted"], 1);
    let quoted = call(&mut server, 2, "run.quote", params());
    let quote = &quoted["result"]["structuredContent"]["result"];
    assert_eq!(quote["totals"]["USD"]["micros"], PRICE.micros, "{quoted}");
    let mut start = params();
    start["accept"] = json!({"token": quote["token"], "amounts": {"USD": PRICE.micros}});
    let refused = call(&mut server, 3, "run.start", start);
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(mock.requests().is_empty());
    assert_eq!(mock.billed(), 0);
    assert_eq!(server.session().unwrap().revision(), 0);
}

#[test]
fn failed_provider_quotes_remain_unknown_and_submit_nothing() {
    let (mut host, mock) = fixture(Err(nodecraft_providers::ErrorKind::Busy));
    let result = host.dispatch("run.estimate", &params()).unwrap();
    assert_eq!(result["status"], "estimated");
    assert_eq!(result["quoted"], 0);
    assert_eq!(result["unquoted"], 1);
    let quote = host.session.dispatch("run.quote", &params()).unwrap();
    assert_eq!(quote["complete"], false);
    assert_eq!(quote["totals"], json!({}));
    assert!(mock.requests().is_empty());
    assert_eq!(mock.billed(), 0);
}

#[test]
fn not_estimated_is_returned_with_its_accepted_request_id() {
    let (mut host, mock) = fixture(Ok(PRICE));
    prepare(&mut host.session, &params(), &mut *host.maker, &mut host.routing).unwrap();
    host.runner.install(&mut host.session);
    // A syntactically valid request whose empty selection cannot make a plan is accepted,
    // then explained by the host's NotEstimated notice.
    let result = host.runner.estimate(&mut host.session, &json!({"canvas": C, "mode": "selected"})).unwrap();
    assert_eq!(result["accepted"], true);
    assert_eq!(result["request"], 0);
    assert_eq!(result["status"], "not_estimated");
    assert!(result["reason"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(mock.quoted().is_empty());
    assert!(mock.requests().is_empty());
}

#[test]
fn invalid_and_empty_selections_never_acquire_an_adapter() {
    let (mut host, mock) = fixture(Ok(PRICE));
    host.maker = Box::new(|_| panic!("unused provider must not be built"));
    assert!(host.dispatch("run.estimate", &json!({"canvas": C, "mode": "selected", "nodes": ["not-an-id"]})).is_err());
    let empty = host.dispatch("run.estimate", &json!({"canvas": C, "mode": "selected"})).unwrap();
    assert_eq!(empty["status"], "not_estimated");
    assert!(empty["reason"].is_string());
    assert!(mock.quoted().is_empty());
}

#[test]
fn cli_run_uses_the_estimated_price_but_still_requires_spending_consent() {
    let (mut host, mock) = fixture_with_runner(Ok(PRICE), true);
    crate::quote::prepare(&mut host.session, C, &[N], &mut *host.maker).unwrap();
    host.runner.install(&mut host.session);
    let estimated = for_targets(&mut host.runner, &mut host.session, C, &[N]).unwrap();
    assert_eq!(estimated["quoted"], 1);
    let quoted = crate::quote::quote(&mut host.session, C, &[N]).unwrap();
    assert_eq!(quoted["totals"]["USD"]["micros"], PRICE.micros);
    let refused = host.runner.run(&mut host.session, C, &[N], &quoted, crate::run::Consent::default()).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Budget);
    assert!(mock.requests().is_empty());
    let steps = host.runner.run(&mut host.session, C, &[N], &quoted, crate::run::Consent { spend: true }).unwrap();
    assert_eq!(steps, vec![json!({"node": N, "status": "recorded"})]);
    assert_eq!(mock.quoted().len(), 1);
    assert_eq!(mock.requests().len(), 1);
    assert!(!mock.requests()[0].idempotency_key.is_empty());
    assert_eq!(mock.billed(), 1);
}

#[test]
fn estimates_for_two_cards_and_an_empty_request_keep_prior_prices_and_adapter() {
    let (mut host, mock) = fixture(Ok(PRICE));
    let second = NodeId::from_bytes([87; 16]);
    let mut other = host.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap().clone();
    other.id = second;
    other.params.insert("prompt".into(), Output::Text("A different job for the second card".into()));
    host.session.dispatch("node.add", &json!({"canvas": C, "node": other})).unwrap();
    let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&builds);
    let provider = mock.clone();
    host.maker = Box::new(move |_| {
        count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Arc::new(provider.clone()))
    });
    assert_eq!(host.dispatch("run.estimate", &params()).unwrap()["quoted"], 1);
    let second_params = json!({"canvas": C, "mode": "selected", "nodes": [second]});
    assert_eq!(host.dispatch("run.estimate", &second_params).unwrap()["quoted"], 1);
    assert_eq!(host.dispatch("run.quote", &params()).unwrap()["complete"], true);
    assert_eq!(host.dispatch("run.quote", &second_params).unwrap()["complete"], true);
    assert_eq!(host.dispatch("run.estimate", &json!({"canvas": C, "mode": "selected"})).unwrap()["status"], "not_estimated");
    assert_eq!(host.dispatch("run.quote", &params()).unwrap()["complete"], true);
    assert_eq!(builds.load(std::sync::atomic::Ordering::Relaxed), 1, "the same captured adapter is reused without another keychain read");
    assert_eq!(mock.quoted().len(), 2);
    assert!(mock.requests().is_empty());
    assert_eq!(mock.billed(), 0);
}

#[test]
fn optional_estimate_setup_failure_keeps_ordinary_mcp_commands_available() {
    let mut services = Services::default();
    assert!(optional_setup(&mut services, || Err(Error::new(ErrorKind::Unsupported, "offline TLS fixture"))).is_none());
    let session = nodecraft_automation::blank_session(ProjectId::from_bytes([88; 16]), C).unwrap().with_services(services);
    let mut server = McpServer::new(session, Box::new(|| [89; 16]));
    let ordinary = call(&mut server, 1, "canvas.stale", json!({"canvas": C}));
    assert_eq!(ordinary["result"]["isError"], false, "{ordinary}");
    assert_eq!(ordinary["result"]["structuredContent"]["result"]["nodes"], json!([]));
    let unsupported = call(&mut server, 2, "run.estimate", json!({"canvas": C, "mode": "all_stale"}));
    assert_eq!(unsupported["result"]["isError"], true, "{unsupported}");
}

#[test]
fn a_panicking_adapter_factory_returns_an_error_and_mcp_keeps_working() {
    let (mut host, mock) = fixture(Ok(PRICE));
    host.maker = Box::new(|_| panic!("public factory panic fixture"));
    assert_eq!(host.dispatch("run.estimate", &params()).unwrap_err().kind, ErrorKind::Internal);
    assert!(host.dispatch("canvas.stale", &json!({"canvas": C})).unwrap()["nodes"].is_array());
    assert!(mock.requests().is_empty());
    assert_eq!(mock.billed(), 0);
}

#[test]
fn an_idle_mcp_command_expires_a_quote_after_ten_minutes_without_estimating_again() {
    use nodecraft_exec::driver::{Runtime, TaskFuture};
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Clock(Arc<AtomicU64>);
    impl Runtime for Clock {
        fn spawn(&self, mut task: TaskFuture<()>) {
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(task.as_mut().poll(&mut context).is_ready(), "the fixture's quote completes synchronously");
        }
        fn sleep(&self, _: u64) -> TaskFuture<()> {
            Box::pin(async {})
        }
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
    }
    let (mut host, mock) = fixture(Ok(PRICE));
    let clock = Arc::new(AtomicU64::new(0));
    let mut services = Services::default();
    host.runner = Runner::test_estimates(&mut services, Arc::new(Clock(Arc::clone(&clock)))).unwrap();
    host.session = host.session.with_services(services);
    assert_eq!(host.dispatch("run.estimate", &params()).unwrap()["quoted"], 1);
    assert_eq!(host.dispatch("run.quote", &params()).unwrap()["complete"], true);
    clock.store(nodecraft_engine::provider_quotes::QUOTE_TTL_MS + 1, Ordering::Relaxed);
    assert_eq!(host.dispatch("run.quote", &params()).unwrap()["complete"], false);
    assert_eq!(mock.quoted().len(), 1, "idle expiry never asks for another price automatically");
    assert!(mock.requests().is_empty());
    assert_eq!(mock.billed(), 0);
}

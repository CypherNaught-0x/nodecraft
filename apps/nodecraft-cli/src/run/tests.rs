//! The CLI's runs against the mock provider: nothing here reads the keychain or calls a real one.
use super::*;
use nodecraft_engine::commands::{Registry, Resources};
use nodecraft_engine::generation_catalog::GenerationCatalog;
use nodecraft_engine::provider_setup::Readiness;
use nodecraft_engine::run_cmds::RunCatalog;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_provider_testkit::mock::{MockFailure, MockModel, MockOutput, MockProvider};
use nodecraft_providers::Provider;
use nodecraft_types::ProjectId;

const C: CanvasId = CanvasId::from_bytes([91; 16]);
const N: NodeId = NodeId::from_bytes([92; 16]);

const MOCK_IMAGE: &str = r#"
[[model]]
id = "mock-square"
provider = "mock"
provider_model = "mock-image"
label = "Mock image"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

/// A session with one image card on the mock model, routed and ready to run.
fn scene(model: MockModel) -> (Session, Runner) {
    scene_with(model, None)
}

/// [`scene`], its runner held to `spend` when given.
fn scene_with(model: MockModel, spend: Option<&Spend>) -> (Session, Runner) {
    let catalog = nodecraft_catalog::Catalog::from_toml(MOCK_IMAGE).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([93; 16]), "Runs").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds
        .get(nodecraft_nodes::generation::KIND)
        .unwrap()
        .instantiate_instance(
            N,
            Default::default(),
            &[("model".into(), Output::Text("mock-square".into())), ("prompt".into(), Output::Text("A red square".into()))].into(),
        )
        .unwrap();
    node.title = "Red square!".into();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mut services = Services::default();
    let runner = match spend {
        Some(spend) => Runner::with_spend(&mut services, "cli-test".into(), spend).unwrap(),
        None => Runner::new(&mut services, "cli-test".into()).unwrap(),
    };
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    let mock = MockProvider::new("mock", [("mock-image".to_owned(), model)]);
    let mut maker = move |adapter: &str| -> Result<Arc<dyn Provider>, Readiness> {
        if adapter == "mock" { Ok(Arc::new(mock.clone())) } else { Err(Readiness::NotSetUp("not in this test".into())) }
    };
    crate::quote::prepare(&mut session, C, &[N], &mut maker).unwrap();
    runner.install(&mut session);
    (session, runner)
}

#[test]
fn a_paid_run_needs_consent_then_runs_and_exports_its_result() {
    let (mut session, mut runner) = scene(MockModel::new(MockOutput::Image));
    let quote = crate::quote::quote(&mut session, C, &[N]).unwrap();
    let refused = runner.run(&mut session, C, &[N], &quote, Consent::default()).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Budget);
    assert!(refused.message.contains("--yes"), "{}", refused.message);

    let steps = runner.run(&mut session, C, &[N], &quote, Consent { spend: true }).unwrap();
    assert_eq!(steps, [json!({"node": N, "status": "recorded"})]);
    let dir = tempfile::tempdir().unwrap();
    let out = out_folder(dir.path()).unwrap();
    let written = export(&mut session, C, &[N], &out).unwrap();
    let [result] = written.as_slice() else { panic!("{written:?}") };
    let path = PathBuf::from(result["path"].as_str().unwrap());
    let name = path.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("red-square-") && name.ends_with("-out-1.png"), "{name}");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), result["bytes"].as_u64().unwrap());

    // A second export never replaces the first: it takes the next free name.
    let again = export(&mut session, C, &[N], &out).unwrap();
    let second = again[0]["path"].as_str().unwrap();
    assert!(second.ends_with("-out-1-2.png") && std::fs::metadata(&path).is_ok(), "{again:?}");
    assert_eq!(out_folder(&dir.path().join("nowhere")).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(out_folder(&path).unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn a_failed_step_is_reported_not_raised() {
    let mut refusing = MockModel::new(MockOutput::Image);
    refusing.fail = Some(MockFailure::Submit(nodecraft_providers::ErrorKind::UnsupportedInput));
    let (mut session, mut runner) = scene(refusing);
    let quote = crate::quote::quote(&mut session, C, &[N]).unwrap();
    let steps = runner.run(&mut session, C, &[N], &quote, Consent { spend: true }).unwrap();
    let [step] = steps.as_slice() else { panic!("{steps:?}") };
    assert_eq!(step["status"], "failed", "{step}");
    assert!(report_text(&json!({"steps": steps})).contains("failed"));
}

#[test]
fn a_stale_or_unreadable_quote_is_refused() {
    let (mut session, mut runner) = scene(MockModel::new(MockOutput::Image));
    let mut quote = crate::quote::quote(&mut session, C, &[N]).unwrap();
    quote["token"] = json!("not the quote's token");
    assert!(runner.run(&mut session, C, &[N], &quote, Consent { spend: true }).is_err());
    assert!(accept(&json!({}), Consent { spend: true }).is_err());
    assert!(accept(&json!({"token": "t", "allowed": false, "blocks": ["over budget"]}), Consent { spend: true }).unwrap_err().message.contains("over budget"));
    let free = json!({"token": "t", "allowed": true, "complete": true, "steps": [], "steps_total": 0, "totals": {}});
    assert_eq!(accept(&free, Consent::default()).unwrap(), json!({"token": "t", "amounts": {}}));
    let partial = json!({"token": "t", "allowed": true, "complete": false, "steps": [{"unpriced": "no price", "jobs": 2}], "steps_total": 1, "totals": {}});
    assert!(accept(&partial, Consent::default()).is_err());
    assert_eq!(accept(&partial, Consent { spend: true }).unwrap(), json!({"token": "t", "amounts": {}, "partial": true, "unpriced_jobs": 2}));
}

#[test]
fn file_names_are_safe_and_distinct() {
    let id = NodeId::from_bytes([0xab; 16]);
    assert_eq!(stem("../My  Fox/étude", "image", id), format!("my-fox-tude-{}", &id.to_string()[..8]));
    assert_eq!(stem("  ", "text", id), format!("text-{}", &id.to_string()[..8]));
    assert_eq!(name("fox", "../im age", Some(0)), "fox-im-age-1");
    assert_eq!(name("fox", "étude", None), "fox-tude");
    assert_eq!(name("fox", "日本", None), "fox-out");
    assert_eq!(extension("application/x-unknown"), "bin");
}

#[test]
fn an_unanswered_estimate_wait_has_a_defensive_deadline_and_keeps_its_id() {
    let (mut session, mut runner) = scene(MockModel::new(MockOutput::Image));
    let started = Instant::now();
    let result = runner.wait_estimate(&mut session, 12_345, Duration::ZERO);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(result["request"], 12_345);
    assert_eq!(result["status"], "not_estimated");
    assert!(result["reason"].as_str().is_some_and(|reason| !reason.is_empty()));
}

#[test]
fn runners_sharing_a_spend_are_held_to_its_budget_together() {
    let spend = Spend::default();
    let usd = nodecraft_types::Currency::try_from("USD".to_owned()).unwrap();
    spend.budgets.lock().unwrap().project = Some(nodecraft_types::Money { currency: usd, micros: 50_000 });
    let (mut session, mut runner) = scene_with(MockModel::new(MockOutput::Image), Some(&spend));
    let quote = crate::quote::quote(&mut session, C, &[N]).unwrap();
    let steps = runner.run(&mut session, C, &[N], &quote, Consent { spend: true }).unwrap();
    assert_eq!(steps[0]["status"], "recorded", "{steps:?}");

    // A second runner, as the next served run gets: its 0.04 would bring the total past 0.05.
    let (mut session, mut runner) = scene_with(MockModel::new(MockOutput::Image), Some(&spend));
    let quote = crate::quote::quote(&mut session, C, &[N]).unwrap();
    let outcome = runner.run(&mut session, C, &[N], &quote, Consent { spend: true });
    let refused = match &outcome {
        Err(error) => error.message.to_lowercase(),
        Ok(steps) => steps[0].to_string().to_lowercase(),
    };
    assert!(outcome.as_ref().map_or(true, |steps| steps[0]["status"] != "recorded"), "{outcome:?}");
    assert!(refused.contains("budget"), "{refused}");
}

#[test]
fn a_run_past_its_deadline_is_cancelled_and_fails_as_timed_out() {
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = u32::MAX;
    let (mut session, mut runner) = scene(slow);
    let quote = crate::quote::quote(&mut session, C, &[N]).unwrap();
    let started = Instant::now();
    let error = runner.run_within(&mut session, C, &[N], &quote, Consent { spend: true }, Some(started)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout, "{}", error.message);
    assert!(error.message.contains("may still be charged"), "{}", error.message);
    assert!(started.elapsed() < CANCEL_GRACE, "the cancelled job stopped without waiting out the grace");
}

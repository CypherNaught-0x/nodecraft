use super::*;
use std::sync::Arc;
#[test]
fn cli97_csv_headers_shape_defaults_and_literal_equals_are_checked() {
    assert!(rows(vec![vec!["x".into(), "x".into()], vec!["a".into(), "b".into()]], &[]).is_err());
    assert!(rows(vec![vec!["x".into()], vec!["a".into(), "b".into()]], &[]).is_err());
    assert!(rows(vec![vec!["x".into()]], &[]).is_err());
    assert_eq!(
        rows(vec![vec!["prompt".into()], vec!["a=b".into()]], &["prompt=old".into(), "other=keep".into()]).unwrap(),
        vec![vec!["other=keep", "prompt=a=b"]]
    );
}
#[test]
fn cli97_batch_limits_refuse_unknown_different_currency_and_overflow_before_spend() {
    let quote = json!({"complete":true,"totals":{"USD":{"micros":300000}}});
    let mut totals = BTreeMap::new();
    add_totals(&mut totals, &quote).unwrap();
    add_totals(&mut totals, &quote).unwrap();
    assert!(total_limit(&totals, true, &Some(("USD".into(), 599999))).is_err());
    assert!(total_limit(&totals, true, &Some(("USD".into(), 600000))).is_ok());
    assert!(total_limit(&totals, false, &Some(("USD".into(), 600000))).is_err());
    assert!(total_limit(&totals, true, &Some(("EUR".into(), 600000))).is_err());
    assert!(limits(&json!({"complete":false}), &[("USD".into(), 1000000)].into()).is_err());
    totals.insert("USD".into(), u64::MAX);
    assert!(add_totals(&mut totals, &quote).is_err());
}

#[test]
fn cli97_two_csv_rows_use_fresh_documents_on_the_same_runner_and_save_distinct_results() {
    use nodecraft_format::{AssetStore, Snapshot};
    use nodecraft_types::{CanvasId, NodeId, ProjectId};
    let canvas = CanvasId::from_bytes([1; 16]);
    let node = NodeId::from_bytes([2; 16]);
    let mut source = nodecraft_automation::blank_session(ProjectId::from_bytes([97; 16]), canvas).unwrap();
    source.dispatch("node.create", &json!({"canvas":canvas,"id":node,"kind":"source.text","pos":{"x":0,"y":0}})).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.ncraft");
    let bytes = nodecraft_format::encode(&Snapshot { project: source.document().clone(), assets: AssetStore::default(), epoch: 0 }).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let arguments = vec![path.to_str().unwrap().into(), "--csv".into(), "unused.csv".into(), "--out".into(), dir.path().to_str().unwrap().into()];
    let config = args(&arguments).unwrap();
    let target = Target::open(path.to_str().unwrap(), &config.options).unwrap();
    let mut files = Files::default();
    let bindings =
        ["first", "second"].into_iter().map(|value| target.bindings(&[format!("{node}={value}")], dir.path(), &mut files).unwrap()).collect::<Vec<_>>();
    let report = execute(&config, &target, &bindings, &mut |_| panic!("local rows need no provider")).unwrap();
    assert_eq!(report["rows"][0]["status"], "done", "{report}");
    assert_eq!(report["rows"][1]["status"], "done", "{report}");
    assert_eq!(report["rows"][0]["results"][0]["text"], "first");
    assert_eq!(report["rows"][1]["results"][0]["text"], "second");
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

fn paid_target(dir: &Path, config: &Args) -> (Target, Vec<Vec<Binding>>) {
    use nodecraft_types::{CanvasId, ProjectId, Value as Typed};
    let canvas = CanvasId::from_bytes([1; 16]);
    let node = NodeId::from_bytes([2; 16]);
    let mut source = nodecraft_automation::blank_session(ProjectId::from_bytes([97; 16]), canvas).unwrap();
    source.dispatch("node.create", &json!({"canvas":canvas,"id":node,"kind":"generation.image","pos":{"x":0,"y":0}})).unwrap();
    let request = json!({"canvas":canvas,"nodes":[node],"model":"fal.ideogram-v3"});
    let preview = source.dispatch("node.model_preview", &request).unwrap();
    let mut commit = request;
    commit["expected_revision"] = preview["revision"].clone();
    commit["preview_binding"] = preview["binding"].clone();
    source.dispatch("node.set_model", &commit).unwrap();
    source.dispatch("node.param_set", &json!({"canvas":canvas,"id":node,"name":"prompt","value":Typed::Text("fixture".into())})).unwrap();
    let path = dir.join("paid.ncraft");
    std::fs::write(
        &path,
        nodecraft_format::encode(&nodecraft_format::Snapshot { project: source.document().clone(), assets: Default::default(), epoch: 0 }).unwrap(),
    )
    .unwrap();
    let target = Target::open(path.to_str().unwrap(), &config.options).unwrap();
    let mut files = Files::default();
    let rows = ["first", "second"].into_iter().map(|value| target.bindings(&[format!("{node}.prompt={value}")], dir, &mut files).unwrap()).collect();
    (target, rows)
}
fn paid_config(dir: &Path) -> Args {
    args(&["unused.ncraft".into(), "--csv".into(), "unused.csv".into(), "--out".into(), dir.to_str().unwrap().into()]).unwrap()
}
#[test]
fn cli97_paid_batches_preflight_all_rows_and_aggregate_limits_without_any_generation() {
    use nodecraft_provider_testkit::mock::{MockModel, MockOutput, MockProvider};
    let dir = tempfile::tempdir().unwrap();
    let mut config = paid_config(dir.path());
    let (target, rows) = paid_target(dir.path(), &config);
    let mock = MockProvider::new("fal", [("fal-ai/ideogram/v3".into(), MockModel::new(MockOutput::Image))]);
    let mut maker = |_: &str| Ok(Arc::new(mock.clone()) as Arc<dyn nodecraft_providers::Provider>);
    let denied = execute(&config, &target, &rows, &mut maker).unwrap_err();
    assert!(denied.message.contains("--yes"), "{denied:?}");
    config.options.yes = true;
    config.max_total = Some(("USD".into(), 119999));
    assert!(execute(&config, &target, &rows, &mut maker).unwrap_err().message.contains("--max-total"));
    config.max_total = None;
    config.max_cost.insert("USD".into(), 59999);
    assert!(execute(&config, &target, &rows, &mut maker).unwrap_err().message.contains("--max-cost"));
    config.max_cost.clear();
    config.quote_only = true;
    let quoted = execute(&config, &target, &rows, &mut maker).unwrap();
    assert_eq!(quoted["totals_micros"]["USD"], 120000);
    assert_eq!(quoted["rows"].as_array().unwrap().len(), 2);
    assert!(mock.requests().is_empty());
    assert_eq!(mock.billed(), 0);
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with("batch-")));
}
#[test]
fn cli97_batch_fail_fast_and_keep_going_are_real_runner_results() {
    use nodecraft_provider_testkit::mock::{MockFailure, MockModel, MockOutput, MockProvider};
    for keep_going in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = paid_config(dir.path());
        config.options.yes = true;
        config.keep_going = keep_going;
        let (target, rows) = paid_target(dir.path(), &config);
        let mut model = MockModel::new(MockOutput::Image);
        model.fail = Some(MockFailure::Submit(nodecraft_providers::ErrorKind::UnsupportedInput));
        let mock = MockProvider::new("fal", [("fal-ai/ideogram/v3".into(), model)]);
        let report = execute(&config, &target, &rows, &mut |_| Ok(Arc::new(mock.clone()) as Arc<dyn nodecraft_providers::Provider>)).unwrap();
        assert_eq!(report["rows"][0]["status"], "failed", "{report}");
        assert_eq!(report["rows"][1]["status"], if keep_going { "failed" } else { "skipped" }, "{report}");
        assert_eq!(mock.requests().len(), if keep_going { 2 } else { 1 });
        assert_eq!(mock.billed(), 0);
    }
}

#[test]
fn cli97_batch_returns_reused_results_when_a_row_matches_the_saved_project() {
    use nodecraft_types::{CanvasId, ProjectId, Value as Typed};
    let canvas = CanvasId::from_bytes([1; 16]);
    let node = NodeId::from_bytes([2; 16]);
    let mut source = nodecraft_automation::blank_session(ProjectId::from_bytes([97; 16]), canvas).unwrap();
    source.dispatch("node.create", &json!({"canvas":canvas,"id":node,"kind":"source.text","pos":{"x":0,"y":0}})).unwrap();
    source.dispatch("node.param_set", &json!({"canvas":canvas,"id":node,"name":"text","value":Typed::Text("same".into())})).unwrap();
    let mut services = Services::default();
    let mut runner = Runner::new(&mut services, "cli97-prior".into()).unwrap();
    source = source.with_services(services);
    crate::quote::prepare(&mut source, canvas, &[node], &mut |_| panic!("local source")).unwrap();
    runner.install(&mut source);
    let quote = crate::quote::quote(&mut source, canvas, &[node]).unwrap();
    runner.run(&mut source, canvas, &[node], &quote, Consent::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reused.ncraft");
    std::fs::write(
        &path,
        nodecraft_format::encode(&nodecraft_format::Snapshot { project: source.document().clone(), assets: source.assets().clone(), epoch: 0 }).unwrap(),
    )
    .unwrap();
    let config = paid_config(dir.path());
    let target = Target::open(path.to_str().unwrap(), &config.options).unwrap();
    let row = target.bindings(&[format!("{node}=same")], dir.path(), &mut Files::default()).unwrap();
    let report = execute(&config, &target, &[row], &mut |_| panic!("local reuse")).unwrap();
    assert_eq!(report["rows"][0]["status"], "done", "{report}");
    assert_eq!(report["rows"][0]["results"][0]["text"], "same", "{report}");
}

#[test]
fn cli97_default_expansion_is_admitted_before_duplication_at_the_aggregate_boundary() {
    // A small bound exercises the real admission path without allocating a large fixture.
    let records = || vec![vec!["row".into()], vec!["1".into()], vec!["2".into()]];
    let defaults = ["prompt=long default".into()];
    let exact = 2 * ("prompt=long default".len() + "row=1".len());
    let rows = rows_bounded(records(), &defaults, exact).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows_bounded(records(), &defaults, exact - 1).unwrap_err().message.contains("expanded batch"));
    let many = std::iter::once(vec!["row".into()]).chain((0..1000).map(|_| vec!["x".into()])).collect();
    assert!(rows_bounded(many, &[format!("prompt={}", "x".repeat(1024))], 1024).is_err());
    // CSV overrides do not spend the raw expansion budget on an unused default.
    assert!(rows_bounded(records(), &["row=unused large default".into()], 2 * "row=1".len()).is_ok());
}

//! `sheet.rename` and the rows "retry failed" chooses (#163).
use super::{RetryHolds, retry_failed};
use crate::{Session, commands::Registry, generation_catalog::GenerationCatalog};
use crate::{
    run_host::HeldView,
    run_jobs::{JobKey, JobState, JobView},
};
use nodecraft_graph::{Canvas, Change, Generation, Incomplete, ItemKey, Missing, MissingItem, Project};
use nodecraft_types::{CanvasId, ErrorKind, GenerationId, ItemId, NodeId, ProjectId, Value};
use serde_json::json;
use std::collections::BTreeSet;

const SHEET: CanvasId = CanvasId::from_bytes([1; 16]);
const PLAIN: CanvasId = CanvasId::from_bytes([2; 16]);

fn session(prompts: &[&str]) -> (Session, Vec<NodeId>) {
    let kinds = GenerationCatalog::builtin().unwrap().registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([9; 16]), "Sheets").unwrap();
    Change::AddCanvas { canvas: Canvas::new(PLAIN, "Canvas") }.apply(&mut project).unwrap();
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap();
    s.dispatch("sheet.create", &json!({"canvas": SHEET, "title": "Mugs"})).unwrap();
    let made = s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": NodeId::from_bytes([5; 16]), "prompts": prompts})).unwrap();
    let rows = serde_json::from_value(made["rows"].clone()).unwrap();
    (s, rows)
}

fn title(s: &Session) -> String {
    s.document().canvases().get(&SHEET).unwrap().title().to_owned()
}

#[test]
fn a_sheet_is_renamed_as_one_undo_step_and_listed_by_its_new_title() {
    let (mut s, _) = session(&["a mug"]);
    let renamed = s.dispatch("sheet.rename", &json!({"canvas": SHEET, "title": "  Mugs, autumn  "})).unwrap();
    assert_eq!(renamed, json!({"canvas": SHEET, "title": "Mugs, autumn"}), "surrounding spaces are dropped");
    assert_eq!(title(&s), "Mugs, autumn");
    let listed = s.dispatch("sheet.list", &json!({})).unwrap();
    assert_eq!(listed["sheets"][0]["title"], "Mugs, autumn");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(title(&s), "Mugs");
}

#[test]
fn a_bad_rename_is_refused_and_changes_nothing() {
    let (mut s, _) = session(&["a mug"]);
    let revision = s.revision();
    for (params, kind) in [
        (json!({"canvas": SHEET, "title": "   "}), ErrorKind::InvalidInput),
        (json!({"canvas": SHEET, "title": "x".repeat(257)}), ErrorKind::InvalidInput),
        (json!({"canvas": SHEET, "title": "é".repeat(129)}), ErrorKind::InvalidInput),
        (json!({"canvas": SHEET, "title": "Mugs\nautumn"}), ErrorKind::InvalidInput),
        (json!({"canvas": SHEET, "title": "Mugs\tautumn"}), ErrorKind::InvalidInput),
        (json!({"canvas": SHEET, "title": "Mugs"}), ErrorKind::Unsupported),
        (json!({"canvas": PLAIN, "title": "Not a sheet"}), ErrorKind::InvalidInput),
        (json!({"canvas": CanvasId::from_bytes([7; 16]), "title": "Gone"}), ErrorKind::NotFound),
        (json!({"canvas": SHEET}), ErrorKind::InvalidInput),
        (json!({"canvas": SHEET, "title": "Ok", "extra": 1}), ErrorKind::InvalidInput),
    ] {
        let error = s.dispatch("sheet.rename", &params).unwrap_err();
        assert_eq!(error.kind, kind, "{params}: {}", error.message);
    }
    assert_eq!((s.revision(), title(&s)), (revision, "Mugs".to_owned()));
    // The limit is on the name kept, after its surrounding spaces are dropped.
    let long = "x".repeat(256);
    s.dispatch("sheet.rename", &json!({"canvas": SHEET, "title": format!("  {long}  ")})).unwrap();
    assert_eq!(title(&s), long);
}

fn record(s: &Session, row: NodeId, generation: Generation) -> Session {
    let mut project = s.document().clone();
    let node = project.canvases().get(&SHEET).unwrap().nodes().get(&row).unwrap().clone();
    for change in nodecraft_exec::run::record_changes(SHEET, &node, generation).unwrap() {
        change.apply(&mut project).unwrap();
    }
    Session::with_registries(project, Registry::builtins().unwrap(), GenerationCatalog::builtin().unwrap().registry().unwrap()).unwrap()
}

#[test]
fn retry_failed_takes_failures_and_partial_results_but_never_a_success_or_a_held_row() {
    let (s, rows) = session(&["done", "failed", "partial", "failed but running", "never run", "done then failed", "held", "maybe charged"]);
    let [done, failed, partial, busy, never, relapsed, held, charged] = rows.as_slice() else { panic!("eight rows") };
    let s = record(&s, *done, Generation::new(GenerationId::from_bytes([1; 16]), vec![Value::List(Vec::new())]));
    // A fanned result of two items, the second of which failed (#480).
    let key = |n: u8| ItemKey { batch: NodeId::from_bytes([77; 16]), item: ItemId::from_bytes([n; 16]) };
    let mut half = Generation::new(GenerationId::from_bytes([2; 16]), vec![Value::List(vec![Value::List(Vec::new()), Value::List(Vec::new())])]);
    half.items = vec![vec![key(1)], vec![key(2)]];
    half.incomplete = Some(Incomplete { missing: vec![MissingItem::new(1, Missing::Failed)] });
    let s = record(&s, *partial, half);
    let s = record(&s, *relapsed, Generation::new(GenerationId::from_bytes([3; 16]), vec![Value::List(Vec::new())]));
    let canvas = s.document().canvases().get(&SHEET).unwrap();
    let failures: BTreeSet<NodeId> = [*failed, *busy, *relapsed, *held, *charged].into();
    let holds = RetryHolds { running: [*busy].into(), held: [*held].into(), maybe_charged: [*charged].into() };
    assert_eq!(retry_failed(canvas, &failures, &holds), vec![*failed, *partial, *relapsed], "top to bottom, without the held ones");
    assert!(!retry_failed(canvas, &failures, &holds).contains(never), "a row never run is not a failure");
    assert_eq!(retry_failed(canvas, &BTreeSet::new(), &RetryHolds::default()), vec![*partial], "only the partial result without failures");
}

fn job(canvas: CanvasId, node: NodeId, n: u64, state: JobState, sent: bool) -> JobView {
    JobView {
        key: JobKey { run: 1, job: nodecraft_exec::scheduler::JobId(n) },
        canvas,
        node,
        provider: Some("mock".into()),
        model: Some("mock-image".into()),
        estimate: None,
        state,
        sent,
        cancel_requested: false,
        cancel_refused: false,
        reported_sent: false,
        partial: None,
        partial_attempt: 0,
        unrefused_attempt: false,
        preview: None,
        made: None,
    }
}

#[test]
fn holds_come_from_the_latest_job_of_each_row_and_the_held_results_on_this_sheet() {
    let [a, b, c, d] = [1, 2, 3, 4].map(|n| NodeId::from_bytes([n; 16]));
    let failed = |cause| JobState::Failed { reason: "x".into(), cause: Some(cause) };
    let jobs = vec![
        job(SHEET, a, 1, failed(nodecraft_providers::ErrorKind::Timeout), true),
        // b timed out once and was then refused: its latest job proves nothing was run.
        job(SHEET, b, 2, failed(nodecraft_providers::ErrorKind::Timeout), true),
        job(SHEET, b, 3, failed(nodecraft_providers::ErrorKind::UnsupportedInput), true),
        job(SHEET, c, 4, JobState::Queued, false),
        // Another canvas's jobs hold nothing here.
        job(PLAIN, d, 5, failed(nodecraft_providers::ErrorKind::Timeout), true),
    ];
    let held = vec![HeldView {
        id: 1,
        canvas: SHEET,
        node: d,
        provider: "mock".into(),
        model: "mock-image".into(),
        reason: "kept".into(),
        outputs: 1,
        retry: false,
        pending: None,
        saved: false,
    }];
    let holds = RetryHolds::of(SHEET, &jobs, &held, &BTreeSet::new());
    assert_eq!(holds, RetryHolds { running: [c].into(), held: [d].into(), maybe_charged: [a].into() });
    // A card the host remembers as possibly charged stays held after its view is gone.
    let e = NodeId::from_bytes([5; 16]);
    let remembered: BTreeSet<(CanvasId, NodeId)> = [(SHEET, e), (PLAIN, b)].into();
    assert_eq!(RetryHolds::of(SHEET, &[], &[], &remembered).maybe_charged, [e].into());
}

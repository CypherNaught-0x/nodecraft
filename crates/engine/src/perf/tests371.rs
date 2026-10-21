//! Public-path measurement boundaries for #371.
use super::*;
use crate::{
    Session,
    commands::{CommandSpec, Context, Effect, Registry, Resources},
};
use nodecraft_graph::{Canvas, Change, Changeset, Node, Project};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ProjectId};
use serde_json::{Value, json};
use std::{cell::Cell, rc::Rc};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);
fn project() -> Project {
    let mut p = Project::new(ProjectId::from_bytes([3; 16]), "Public performance fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    p
}
fn session() -> Session {
    Session::new(project()).unwrap()
}
fn metric(r: &Report, m: Metric) -> &MetricReport {
    r.metrics.iter().find(|x| x.metric == m).unwrap()
}
fn clock(s: &mut Session) -> (Rc<Cell<u64>>, Rc<Cell<usize>>) {
    let time = Rc::new(Cell::new(100));
    let calls = Rc::new(Cell::new(0));
    let (t, c) = (time.clone(), calls.clone());
    s.perf_mut().set_clock(Some(Box::new(move || {
        c.set(c.get() + 1);
        let old = t.get();
        t.set(old + 10);
        old
    })));
    s.perf_mut().set_enabled(true);
    (time, calls)
}
fn add(s: &mut Session) {
    s.dispatch("node.add", &json!({"canvas":C,"node":Node::new(N,"source.text")})).unwrap();
}
fn registry() -> Registry {
    let mut r = Registry::builtins().unwrap();
    r.register(CommandSpec {
        id: "test.stage",
        label: "Test",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: || json!({}),
        params_doc: "test only",
        enabled: |_, _| Ok(()),
        run: edit,
    })
    .unwrap();
    r.register(CommandSpec {
        id: "test.read",
        label: "Read",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: || json!({}),
        params_doc: "test only",
        enabled: |_, _| Ok(()),
        run: |_, _| Ok(Effect::None { result: Value::Null }),
    })
    .unwrap();
    r
}
fn edit(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    if let Some(t) = c.resources.get::<Rc<Cell<u64>>>() {
        t.set(t.get() + 1000);
    }
    Ok(Effect::Edit { changes: serde_json::from_value(v.clone()).unwrap(), result: Value::Null, gesture: None })
}
fn custom() -> Session {
    Session::with_registries(project(), registry(), nodecraft_nodes::Registry::builtins().unwrap()).unwrap()
}
fn change(change: Change) -> Value {
    serde_json::to_value(Changeset::new(vec![change])).unwrap()
}
#[test]
fn edit_undo_redo_stage_but_previews_and_read_only_commands_do_not() {
    let mut s = session();
    let original = s.document().clone();
    let (_, calls) = clock(&mut s);
    let params = json!({"canvas":C,"node":Node::new(N,"source.text")});
    for _ in 0..3 {
        s.enabled("node.add", &params).unwrap();
    }
    assert_eq!(calls.get(), 0);
    assert_eq!(s.document(), &original);
    assert!(s.events_since(0).unwrap().is_empty());
    add(&mut s);
    let edited = s.document().clone();
    s.dispatch("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.document(), &original);
    s.dispatch("edit.redo", &Value::Null).unwrap();
    assert_eq!(s.document(), &edited);
    assert_eq!(s.revision(), 3);
    assert_eq!(s.events_since(0).unwrap().len(), 3);
    let r = s.perf().report();
    assert_eq!(metric(&r, Metric::GraphStage).samples, 3);
    assert_eq!(metric(&r, Metric::GraphStage).p50, Some(10));
    assert_eq!(metric(&r, Metric::GraphStageRefused).samples, 0);
    assert_eq!(metric(&r, Metric::Dispatch).samples, 3);
    assert_eq!(metric(&r, Metric::Undo).samples, 1);
    assert_eq!(metric(&r, Metric::Redo).samples, 1);
    let n = calls.get();
    s.enabled("edit.undo", &Value::Null).unwrap();
    assert_eq!(calls.get(), n);
    let mut read = custom();
    clock(&mut read);
    read.dispatch("test.read", &Value::Null).unwrap();
    assert_eq!(metric(&read.perf().report(), Metric::GraphStage).samples, 0);
    assert!(read.dispatch("unknown.command", &Value::Null).is_err());
    assert!(read.dispatch("edit.undo", &Value::Null).is_err());
    assert_eq!(metric(&read.perf().report(), Metric::GraphStageRefused).samples, 0);
}
#[test]
fn stage_clock_excludes_handler_work_and_reports_graph_refusals_separately() {
    let mut s = custom();
    let (t, _) = clock(&mut s);
    let mut resources = Resources::default();
    resources.insert(t);
    s = s.with_resources(resources);
    s.dispatch("test.stage", &change(Change::AddNode { canvas: C, node: Node::new(N, "source.text") })).unwrap();
    let r = s.perf().report();
    assert_eq!(metric(&r, Metric::GraphStage).p50, Some(10));
    assert_eq!(metric(&r, Metric::Dispatch).p50, Some(1030));
    let before = s.document().clone();
    let revision = s.revision();
    let events = s.events_since(0).unwrap();
    let error = s.dispatch("test.stage", &change(Change::AddNode { canvas: C, node: Node::new(N, "source.text") })).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    let r = s.perf().report();
    assert_eq!(metric(&r, Metric::GraphStageRefused).p50, Some(10));
    assert_eq!(metric(&r, Metric::GraphStage).samples, 1);
    assert_eq!(metric(&r, Metric::Dispatch).samples, 1);
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
    assert_eq!(s.events_since(0).unwrap(), events);
    assert!(s.can_undo());
    assert!(!s.can_redo());
}
struct RefuseAppend;
impl nodecraft_format::ProjectStorage for RefuseAppend {
    fn load(&mut self, _: &nodecraft_format::StorageLocation) -> Result<Vec<u8>, Error> {
        Err(Error::new(ErrorKind::Unsupported, "fixture load"))
    }
    fn save(&mut self, _: &nodecraft_format::StorageLocation, _: &[u8]) -> Result<(), Error> {
        Ok(())
    }
    fn checkpoint(&mut self, _: &[u8]) -> Result<(), Error> {
        Ok(())
    }
    fn append(&mut self, _: &[u8]) -> Result<(), Error> {
        Err(Error::new(ErrorKind::Conflict, "fixture append refusal"))
    }
}
#[test]
fn successful_stage_survives_later_preparation_and_durable_append_refusal() {
    let snapshot = || nodecraft_format::Snapshot { project: project(), assets: Default::default(), epoch: 0 };
    let mut s = Session::with_storage(snapshot(), Box::new(RefuseAppend)).unwrap();
    clock(&mut s);
    let before = s.document().clone();
    let err = s.dispatch("node.add", &json!({"canvas":C,"node":Node::new(N,"source.text")})).unwrap_err();
    assert_eq!(err.message, "fixture append refusal");
    assert_eq!(metric(&s.perf().report(), Metric::GraphStage).samples, 1);
    assert_eq!(metric(&s.perf().report(), Metric::Dispatch).samples, 0);
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), 0);
    assert!(!s.can_undo());
    assert!(s.events_since(0).unwrap().is_empty());
    assert!(s.file_status().recovery_required);
    // Missing referenced asset fails AFTER graph staging in persistent replay admission.
    let mut s = Session::with_storage(snapshot(), Box::<nodecraft_format::MemoryStorage>::default()).unwrap();
    clock(&mut s);
    let mut n = Node::new(N, "source.image");
    n.params.insert(
        "image".into(),
        nodecraft_types::Value::Media(nodecraft_types::AssetRef {
            hash: nodecraft_types::Blake3::from_bytes([9; 32]),
            mime: "image/png".parse().unwrap(),
            meta: Default::default(),
        }),
    );
    assert!(s.dispatch("node.add", &json!({"canvas":C,"node":n})).is_err());
    assert_eq!(metric(&s.perf().report(), Metric::GraphStage).samples, 1);
    assert_eq!(metric(&s.perf().report(), Metric::GraphStageRefused).samples, 0);
    assert_eq!(s.document(), &before);
    assert!(!s.file_status().recovery_required);
    assert!(!s.can_undo());
    assert_eq!(s.revision(), 0);
    assert!(s.events_since(0).unwrap().is_empty());
}
#[test]
fn graph_observations_respect_backwards_and_failed_clocks_without_changing_edit() {
    let mut s = session();
    let n = Rc::new(Cell::new(0));
    let calls = n.clone();
    s.perf_mut().set_clock(Some(Box::new(move || {
        let i = calls.get();
        calls.set(i + 1);
        [100, 80, 60, 140].get(i).copied().unwrap_or(140)
    })));
    s.perf_mut().set_enabled(true);
    add(&mut s);
    let r = s.perf().report();
    assert_eq!(metric(&r, Metric::GraphStage).invalid, 1);
    assert_eq!(metric(&r, Metric::GraphStage).samples, 0);
    assert_eq!(metric(&r, Metric::Dispatch).p50, Some(40));
    for fail_at in [2, 3, 4] {
        let mut s = session();
        let calls = Rc::new(Cell::new(0));
        let c = calls.clone();
        s.perf_mut().set_clock(Some(Box::new(move || {
            c.set(c.get() + 1);
            assert_ne!(c.get(), fail_at, "fixture timing failure");
            c.get() as u64 * 10
        })));
        s.perf_mut().set_enabled(true);
        add(&mut s);
        assert_eq!(s.revision(), 1);
        assert_eq!(s.document().canvases()[&C].nodes().len(), 1);
        assert!(!s.perf().has_clock());
        assert_eq!(calls.get(), fail_at);
        let stage = metric(&s.perf().report(), Metric::GraphStage).samples;
        assert_eq!(stage, usize::from(fail_at == 4));
        s.dispatch("edit.undo", &Value::Null).unwrap();
        assert!(s.document().canvases()[&C].nodes().is_empty());
        assert_eq!(calls.get(), fail_at);
    }
}
#[test]
fn a_host_with_a_presentation_source_records_presented_intervals_and_dropped_frames() {
    let mut r = Recorder::default();
    r.set_clock(Some(Box::new(|| 100)));
    r.set_presentation_source(Some(PresentationSource::VsyncFramePump));
    // Not recording yet: nothing is kept.
    r.record(Metric::Presentation, 16_667);
    r.set_enabled(true);
    let empty = r.report();
    let p = metric(&empty, Metric::Presentation);
    assert!(p.supported, "a source with no pumped frames is supported with no samples, not zero");
    assert_eq!((p.unavailable_reason, p.samples, p.p50, p.dropped_frames, p.longest_drop), (None, 0, None, Some(0), Some(0)));
    // 16.7 and 25 ms show every frame; 33 ms misses one, 50 ms two, 100 ms five.
    for us in [16_667, 16_600, 25_000, 33_333, 50_000, 100_000] {
        r.record(Metric::Presentation, us);
    }
    r.record(Metric::Presentation, MAX_SAMPLE_US + 1);
    // Presented intervals aren't idle gaps: a 300 ms stall is kept and counted.
    r.record(Metric::Presentation, 300_000);
    let report = r.report();
    assert_eq!(report.presentation_source, Some(PresentationSource::VsyncFramePump));
    let p = metric(&report, Metric::Presentation);
    assert_eq!((p.samples, p.invalid, p.idle), (7, 1, None));
    assert_eq!(p.over_frame_budget, Some(4));
    assert_eq!((p.dropped_frames, p.longest_drop), (Some(1 + 2 + 5 + 17), Some(17)));
    assert_eq!((p.p50, p.max), (Some(33_333), Some(300_000)));
    // The source is the host's to withdraw: presentation is unsupported again, its samples unread.
    r.set_presentation_source(None);
    r.record(Metric::Presentation, 16_667);
    let withdrawn = r.report();
    let p = metric(&withdrawn, Metric::Presentation);
    assert!(!p.supported);
    assert_eq!(p.unavailable_reason, Some("presentation_timestamps_not_available"));
    assert_eq!((p.dropped_frames, p.longest_drop), (None, None));
}

#[test]
fn missed_frames_round_to_whole_60hz_frames() {
    assert_eq!([0, 16_667, INTERVAL_BUDGET_US].map(missed_frames), [0, 0, 0]);
    assert_eq!([INTERVAL_BUDGET_US + 1, 33_333, 41_000, 50_000, u64::MAX].map(missed_frames), [1, 1, 1, 2, u64::MAX / FRAME_BUDGET_US - 1]);
}

#[test]
fn presentation_stays_unsupported_through_every_public_recording_route() {
    let mut r = Recorder::default();
    let calls = Rc::new(Cell::new(0));
    let c = calls.clone();
    r.set_clock(Some(Box::new(move || {
        c.set(c.get() + 1);
        100
    })));
    r.set_enabled(true);
    r.record(Metric::Presentation, 123);
    r.record_since(Metric::Presentation, 0);
    assert_eq!(calls.get(), 0);
    let report = r.report();
    assert_eq!(report.schema, "nodecraft.perf/3");
    assert_eq!(report.presentation_source, None);
    assert_eq!(report.metrics.len(), Metric::ALL.len());
    let p = metric(&report, Metric::Presentation);
    assert!(!p.supported);
    assert_eq!(p.unavailable_reason, Some("presentation_timestamps_not_available"));
    assert_eq!((p.samples, p.recorded, p.invalid), (0, 0, 0));
    assert_eq!((p.p50, p.p95, p.p99, p.max), (None, None, None, None));
    assert_eq!((p.dropped_frames, p.longest_drop), (None, None));
    r.set_clock(None);
    assert_eq!(metric(&r.report(), Metric::Presentation).unavailable_reason, p.unavailable_reason);
}

#[test]
#[ignore = "whole Session dispatch capture overhead; run once on demand"]
fn perf371_dispatch_collection_overhead() {
    use std::time::Instant;
    for enabled in [false, true, true, false] {
        let mut s = session();
        let calls = Rc::new(Cell::new(0usize));
        let c = calls.clone();
        let epoch = Instant::now();
        s.perf_mut().set_clock(Some(Box::new(move || {
            c.set(c.get() + 1);
            u64::try_from(epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
        })));
        s.perf_mut().set_enabled(enabled);
        let params = json!({"canvas":C,"node":Node::new(N,"source.text")});
        for _ in 0..30 {
            s.dispatch("node.add", &params).unwrap();
            s.dispatch("edit.undo", &Value::Null).unwrap();
        }
        s.perf_mut().clear();
        calls.set(0);
        let start = Instant::now();
        for _ in 0..120 {
            s.dispatch("node.add", &params).unwrap();
            s.dispatch("edit.undo", &Value::Null).unwrap();
        }
        let elapsed = start.elapsed();
        let count = calls.get();
        assert_eq!(count, if enabled { 120 * 9 } else { 0 });
        eprintln!(
            "perf371_dispatch enabled={enabled} operations=240 clock_callbacks={count} elapsed_ns={} mean_ns={} os={} arch={} debug={}",
            elapsed.as_nanos(),
            elapsed.as_nanos() / 240,
            std::env::consts::OS,
            std::env::consts::ARCH,
            cfg!(debug_assertions)
        );
    }
}

#[test]
#[ignore = "read-only Session dispatch collection overhead; run once on demand"]
fn perf371_read_only_dispatch_collection_overhead() {
    use std::time::Instant;
    for enabled in [false, true, true, false] {
        let mut s = custom();
        let calls = Rc::new(Cell::new(0usize));
        let c = calls.clone();
        let epoch = Instant::now();
        s.perf_mut().set_clock(Some(Box::new(move || {
            c.set(c.get() + 1);
            u64::try_from(epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
        })));
        s.perf_mut().set_enabled(enabled);
        for _ in 0..30 {
            s.dispatch("test.read", &Value::Null).unwrap();
        }
        s.perf_mut().clear();
        calls.set(0);
        let start = Instant::now();
        for _ in 0..240 {
            s.dispatch("test.read", &Value::Null).unwrap();
        }
        let elapsed = start.elapsed();
        let count = calls.get();
        assert_eq!(count, if enabled { 480 } else { 0 });
        assert_eq!(s.revision(), 0);
        assert!(!s.can_undo());
        assert!(s.events_since(0).unwrap().is_empty());
        eprintln!(
            "perf371_read_dispatch enabled={enabled} operations=240 clock_callbacks={count} elapsed_ns={} mean_ns={} os={} arch={} debug={}",
            elapsed.as_nanos(),
            elapsed.as_nanos() / 240,
            std::env::consts::OS,
            std::env::consts::ARCH,
            cfg!(debug_assertions)
        );
    }
}

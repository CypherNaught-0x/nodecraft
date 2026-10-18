use super::*;
use std::cell::Cell;
use std::rc::Rc;

/// A clock the test moves by hand.
fn manual() -> (Rc<Cell<u64>>, Clock) {
    let time = Rc::new(Cell::new(1_000));
    let reader = Rc::clone(&time);
    (time, Box::new(move || reader.get()))
}

fn metric(report: &Report, metric: Metric) -> &MetricReport {
    report.metrics.iter().find(|m| m.metric == metric).unwrap()
}

#[test]
fn percentiles_are_nearest_rank_over_the_kept_samples() {
    assert_eq!(percentile(&[], 50), None, "empty windows have no percentile");
    assert_eq!(percentile(&[7], 99), Some(7));
    let hundred: Vec<u64> = (1..=100).collect();
    assert_eq!((percentile(&hundred, 50), percentile(&hundred, 95), percentile(&hundred, 99)), (Some(50), Some(95), Some(99)));
    let mut r = Recorder::default();
    r.set_enabled(true);
    for us in [16_000, 17_000, 33_000, 5_000] {
        r.record(Metric::FramePrepare, us);
    }
    let report = r.report();
    let frames = metric(&report, Metric::FramePrepare);
    assert_eq!((frames.samples, frames.p50, frames.max, frames.over_frame_budget), (4, Some(16_000), Some(33_000), Some(2)));
    assert_eq!(metric(&report, Metric::Dispatch).over_frame_budget, None, "only frames have a budget");
}

#[test]
fn retention_is_bounded_and_invalid_samples_are_counted_not_kept() {
    let mut r = Recorder::default();
    r.set_enabled(true);
    for us in 0..(MAX_SAMPLES as u64 + 10) {
        r.record(Metric::Dispatch, us);
    }
    r.record(Metric::Dispatch, MAX_SAMPLE_US + 1);
    let report = r.report();
    let dispatch = metric(&report, Metric::Dispatch);
    assert_eq!((dispatch.samples, dispatch.recorded, dispatch.invalid), (MAX_SAMPLES, MAX_SAMPLES as u64 + 10, 1));
    assert_eq!(dispatch.max, Some(MAX_SAMPLES as u64 + 9), "the oldest were dropped");
    // A clock that runs backwards gives no sample.
    let (time, clock) = manual();
    r.set_clock(Some(clock));
    let start = r.now().unwrap();
    time.set(start - 1);
    r.record_since(Metric::Undo, start);
    assert_eq!(metric(&r.report(), Metric::Undo).invalid, 1);
    r.clear();
    assert_eq!(metric(&r.report(), Metric::Dispatch).samples, 0);
}

#[test]
fn nothing_is_recorded_while_stopped_and_no_clock_means_unsupported_not_zero() {
    let mut r = Recorder::default();
    r.record(Metric::Save, 10);
    r.set_workload(Workload { total_nodes: 3, ..Workload::default() });
    let report = r.report();
    assert_eq!((report.recording, report.clock), (false, false));
    let save = metric(&report, Metric::Save);
    assert_eq!((save.supported, save.samples, save.p50), (false, 0, None));
    assert!(report.workload.is_none());
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["schema"], "nodecraft.perf/1");
    assert!(json["metrics"][0]["p50"].is_null(), "unmeasured is null, never 0");
}

#[test]
fn commands_start_stop_and_report_and_dispatch_is_timed_without_document_contents() {
    use crate::Session;
    use nodecraft_graph::{Canvas, Change, Project};
    use nodecraft_types::{CanvasId, ProjectId};
    use serde_json::json;
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "A secret project title").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let (_time, clock) = manual();
    s.perf_mut().set_clock(Some(clock));
    assert!(s.enabled("perf.stop", &json!({})).is_err(), "not running yet");
    assert_eq!(s.dispatch("perf.start", &json!({})).unwrap(), json!({"recording": true}));
    assert!(!s.can_undo() && s.revision() == 0, "capture is not an edit");
    let _ = s.dispatch("edit.undo", &json!({}));
    let report = s.dispatch("perf.report", &json!({})).unwrap();
    let dispatch = report["metrics"].as_array().unwrap().iter().find(|m| m["metric"] == "dispatch").unwrap().clone();
    assert_eq!(dispatch["samples"], 0, "a refused undo (nothing to undo) isn't a latency sample, nor are perf commands");
    // A command that runs is timed.
    s.dispatch("node.add", &json!({"canvas": canvas, "node": nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([2; 16]), "source.text")}))
        .unwrap();
    s.dispatch("edit.undo", &json!({})).unwrap();
    let report = s.dispatch("perf.report", &json!({})).unwrap();
    let samples = |name: &str| report["metrics"].as_array().unwrap().iter().find(|m| m["metric"] == name).unwrap()["samples"].clone();
    assert_eq!((samples("dispatch"), samples("undo")), (json!(2), json!(1)));
    let text = report.to_string();
    assert!(!text.contains("secret"), "no document contents in the report");
    assert_eq!(s.dispatch("perf.stop", &json!({})).unwrap(), json!({"recording": false}));
    assert_eq!(s.dispatch("perf.clear", &json!({})).unwrap(), json!({"cleared": true}));
    let cleared = s.dispatch("perf.report", &json!({})).unwrap();
    assert_eq!(cleared["metrics"].as_array().unwrap().iter().find(|m| m["metric"] == "dispatch").unwrap()["samples"], 0);
}

#[test]
#[ignore = "overhead measurement, run on demand"]
fn recording_overhead_on_and_off() {
    let mut r = Recorder::default();
    let started = std::time::Instant::now();
    for us in 0..1_000_000u64 {
        r.record(Metric::Dispatch, us % 20_000);
    }
    let off = started.elapsed();
    r.set_enabled(true);
    let started = std::time::Instant::now();
    for us in 0..1_000_000u64 {
        r.record(Metric::Dispatch, us % 20_000);
    }
    let on = started.elapsed();
    eprintln!("1M samples: off {off:?}, on {on:?}");
}

#[test]
fn idle_gaps_are_not_frames_and_intervals_get_jitter_room() {
    let mut r = Recorder::default();
    r.set_enabled(true);
    for us in [16_700, 17_000, 24_000, 26_000, 2_000_000] {
        r.record(Metric::FrameInterval, us);
    }
    r.record(Metric::FramePrepare, 16_700);
    let report = r.report();
    let interval = metric(&report, Metric::FrameInterval);
    assert_eq!((interval.samples, interval.idle, interval.over_frame_budget), (4, Some(1), Some(1)), "the 2 s pause is idle; only 26 ms is over 1.5 budgets");
    assert_eq!(metric(&report, Metric::FramePrepare).over_frame_budget, Some(1), "preparation keeps the strict budget");
    assert_eq!(metric(&report, Metric::FramePrepare).idle, None);
    r.set_renderer(Some("Test GPU\n(Metal)".into()));
    assert_eq!(r.report().environment.renderer.as_deref(), Some("Test GPU(Metal)"));
}

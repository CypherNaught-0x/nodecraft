//! A Batch Generate sheet's frames at its 1,000-row limit (#163), on #65's frame loop
//! ([`frames`]) and with #151's recorder on, so each line also has the recorder's p50/p95/p99
//! frame times and its count of frames over the 60 Hz budget. The table and the review grid are
//! timed while scrolling through every row, and the table again for the frame after an edit
//! (which reads the rows again). Every row has a result, so the grid draws a thumbnail per tile.
//!
//! Like the canvas benchmark it times the CPU side of a frame as kittest runs it, the egui pass
//! with AccessKit plus tessellation, at 1 and 2 pixels per point: no window, texture upload or
//! GPU, so it is regression evidence and not a frame-rate claim. Release mode, on demand:
//!
//! ```sh
//! cargo test -p nodecraft-ui-egui --release perf::sheet163 -- --ignored --nocapture
//! ```
//!
//! The ordinary (debug) tests check what keeps it bounded: only the rows and tiles in view are
//! laid out (`sheet_tests`).
use super::*;
use crate::app::sheet_view::ViewMode;

fn sheet(ppp: f32, mode: ViewMode) -> Harness<'static, NodeCraftApp> {
    let mut app = crate::app::sheet_tests::batch::full_sheet(1000);
    app.sheet.mode = mode;
    let mut h = Harness::builder()
        .with_size(Vec2::new(1600.0, 1000.0))
        .with_pixels_per_point(ppp)
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(240)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    for _ in 0..10 {
        h.step();
    }
    h.state_mut().session.perf_mut().set_clock(nodecraft_engine::perf::native_clock());
    h.state_mut().session.perf_mut().set_enabled(true);
    for _ in 0..10 {
        h.step();
    }
    h.state_mut().session.perf_mut().clear();
    h
}

/// The recorder's frame summary: p50/p95/p99 and frames over budget, from its report.
fn recorded(stats: &Stats) -> String {
    let Some(report) = &stats.report else { return "no report".into() };
    let frame = report
        .get("metrics")
        .and_then(|metrics| metrics.as_array())
        .and_then(|metrics| metrics.iter().find(|metric| metric.get("metric").and_then(serde_json::Value::as_str) == Some("frame_prepare")));
    frame.map_or_else(|| "no frame metric".into(), |metric| metric.to_string())
}

#[test]
#[ignore = "release-mode frame-time benchmark for #163; run with --release -- --ignored --nocapture"]
fn sheet_frame_times_at_1000_rows() {
    for ppp in [1.0, 2.0] {
        for mode in [ViewMode::Table, ViewMode::Grid] {
            let mut h = sheet(ppp, mode);
            // Scroll down through the rows and back, a few lines a frame.
            let stats = frames(&mut h, 240, |h, frame| {
                let down = if frame < 120 { -1.0 } else { 1.0 };
                h.event(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    phase: egui::TouchPhase::Move,
                    delta: Vec2::new(0.0, down * 3.0),
                    modifiers: egui::Modifiers::NONE,
                });
            });
            eprintln!("ppp {ppp} 1000-row sheet {mode:?} scrolling: {stats}  recorder: {}", recorded(&stats));
            assert!(stats.report.is_some(), "the recorder was on");
        }
        let mut h = sheet(ppp, ViewMode::Table);
        let first = nodecraft_engine::sheet_cmds::rows(h.state().session.document().canvases().get(&h.state().canvas).unwrap())[0].id;
        let canvas = h.state().canvas;
        let stats = frames(&mut h, 60, |h, frame| {
            let value = Value::Text(format!("prompt {frame}, edited"));
            h.state_mut().session.dispatch("node.param_set", &json!({"canvas": canvas, "id": first, "name": "prompt", "value": value})).unwrap();
        });
        eprintln!("ppp {ppp} 1000-row sheet Table frame after an edit: {stats}  recorder: {}", recorded(&stats));
    }
}

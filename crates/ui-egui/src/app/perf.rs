//! The M1 performance criterion (#65): frame times over generated 200- and 1,000-node graphs while
//! panning, zooming and editing, plus "undo everything" for a graph built as a user would.
//!
//! The benchmark is ignored by default; run it in release mode:
//!
//! ```sh
//! cargo test -p nodecraft-ui-egui --release perf -- --ignored --nocapture
//! ```
//!
//! It times the CPU side of a frame as kittest runs it: the egui pass (including AccessKit, which
//! eframe only runs with assistive tech on) plus tessellation, at 1 and 2 pixels per point. It does
//! not include windowing, texture uploads or GPU work. Selection, palette and clipboard
//! interactions are not benchmark workloads; their correctness tests do not measure frame time.
use super::*;
use egui_kittest::Harness;
use std::time::{Duration, Instant};

/// The run's grid of `count` nodes (`perf_run::grid_session`) in a kittest window.
fn graph(count: usize, ppp: f32) -> Harness<'static, NodeCraftApp> {
    let (session, canvas) = super::perf_run::grid_session(count).unwrap();
    Harness::builder()
        .with_size(Vec2::new(1600.0, 1000.0))
        .with_pixels_per_point(ppp)
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(240)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap())
}

struct Stats {
    mean: Duration,
    p95: Duration,
    max: Duration,
    raw_ns: Vec<u64>,
    workloads: Vec<serde_json::Value>,
    report: Option<serde_json::Value>,
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        write!(f, "mean {:>6.2} ms, p95 {:>6.2} ms, max {:>6.2} ms", ms(self.mean), ms(self.p95), ms(self.max))
    }
}

/// Time `count` frames; `before` runs ahead of each (untimed) and may change the view or the
/// document. The pointer moves over the canvas every frame, as it does in use.
fn frames(h: &mut Harness<'static, NodeCraftApp>, count: usize, mut before: impl FnMut(&mut Harness<'static, NodeCraftApp>, usize)) -> Stats {
    let mut times = Vec::with_capacity(count);
    let mut workloads = Vec::new();
    for frame in 0..count {
        before(h, frame);
        let wobble = (frame % 40) as f32;
        h.hover_at(Pos2::new(600.0 + wobble * 4.0, 400.0 + wobble * 2.0));
        // The UI pass plus tessellating its shapes.
        let start = Instant::now();
        h.step();
        let shapes = h.output().shapes.clone();
        let ppp = h.output().pixels_per_point;
        let meshes = h.ctx.tessellate(shapes, ppp);
        times.push(start.elapsed());
        std::hint::black_box(meshes);
        if h.state().session.perf().enabled() {
            // Observation/export is untimed. The timed UI pass includes collection itself.
            workloads.push(serde_json::to_value(h.state().session.perf().report().workload).unwrap());
        }
    }
    let raw_ns = times.iter().map(|d| u64::try_from(d.as_nanos()).unwrap()).collect();
    times.sort();
    let mean = times.iter().sum::<Duration>() / u32::try_from(count).unwrap();
    let p95 = times.get(count * 95 / 100).copied().unwrap_or_default();
    let report = h.state().session.perf().enabled().then(|| serde_json::to_value(h.state().session.perf().report()).unwrap());
    Stats { mean, p95, max: times.last().copied().unwrap_or_default(), raw_ns, workloads, report }
}

fn settle(h: &mut Harness<'static, NodeCraftApp>, zoom: f32) {
    h.state_mut().view = CanvasView { pan: Vec2::new(40.0, 40.0), zoom };
    for _ in 0..10 {
        h.step();
    }
}

#[test]
#[ignore = "release-mode frame-time benchmark for #65; run with --release -- --ignored --nocapture"]
fn canvas_frame_times() {
    for ppp in [1.0, 2.0] {
        for count in [200, 1000] {
            let mut h = graph(count, ppp);
            // Steady panning at fixed zooms: 0.1 is the minimum (every node of the 200 graph and
            // most of the 1,000 on screen), 0.6 is just above the zoom where card bodies appear.
            for zoom in [0.1, 0.2, 0.6, 1.0] {
                settle(&mut h, zoom);
                let stats = frames(&mut h, 120, |h, frame| {
                    let phase = if (frame / 20) % 2 == 0 { 1.0 } else { -1.0 };
                    h.state_mut().view.pan += Vec2::new(6.0, 3.0) * phase;
                });
                eprintln!("ppp {ppp} {count:>5} nodes  pan at {zoom:.1}: {:>4} cards  {stats}", h.state().painted.len());
            }
            // A zoom sweep: every frame a new zoom, so text layouts and glyphs change.
            settle(&mut h, 0.1);
            let stats = frames(&mut h, 160, |h, frame| {
                let factor = if frame < 80 { 1.03 } else { 1.0 / 1.03 };
                let origin = h.state().viewport.min;
                h.state_mut().view.zoom_at(origin, Pos2::new(800.0, 500.0), factor);
            });
            eprintln!("ppp {ppp} {count:>5} nodes  zoom sweep 0.1-1.0: {stats}");
            // The frame after an edit (every cached schema is checked against the new revision).
            settle(&mut h, 0.1);
            let canvas = h.state().canvas;
            let stats = frames(&mut h, 60, |h, frame| {
                let id = NodeId::from_bytes(u128::try_from(frame % 50 + 2).unwrap().to_be_bytes());
                let pos = json!({"x": (frame % 50) as f32 * 400.0, "y": 10.0 * (frame % 3) as f32});
                h.state_mut().session.dispatch("node.move", &json!({"canvas": canvas, "id": id, "pos": pos})).unwrap();
            });
            eprintln!("ppp {ppp} {count:>5} nodes  frame after an edit at 0.1: {stats}");
        }
    }
}

#[test]
fn a_graph_built_like_a_user_would_undoes_to_empty() {
    let mut h = super::tests::harness();
    let canvas = h.state().canvas;
    let original = h.state().session.document().clone();
    let before = original.canvases().get(&canvas).unwrap().nodes().len();
    let id = |n: u32| NodeId::from_bytes(u128::from(n + 1000).to_be_bytes());
    // 200 creates, 199 edits and 200 moves: about 600 undo steps, well past a few hundred.
    for n in 0..200u32 {
        let pos = json!({"x": (n % 20) as f32 * 400.0, "y": (n / 20) as f32 * 320.0});
        h.state_mut().session.dispatch("node.create", &json!({"canvas": canvas, "id": id(n), "kind": "source.text", "pos": pos})).unwrap();
    }
    for n in 1..200u32 {
        h.state_mut()
            .session
            .dispatch("node.param_set", &json!({"canvas": canvas, "id": id(n), "name": "text", "value": {"type": "text", "value": format!("{n}")}}))
            .unwrap();
    }
    for n in 0..200u32 {
        let pos = json!({"x": (n % 20) as f32 * 400.0 + 40.0, "y": (n / 20) as f32 * 320.0});
        h.state_mut().session.dispatch("node.move", &json!({"canvas": canvas, "id": id(n), "pos": pos})).unwrap();
    }
    h.run();
    assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().len(), before + 200);
    let mut steps = 0;
    while h.state().session.can_undo() {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.step();
        steps += 1;
        assert!(steps <= 1000, "undo never ends");
    }
    h.run();
    assert_eq!(steps, 599);
    assert_eq!(h.state().session.document(), &original);
}

mod capture371;
mod save;

mod regression152;

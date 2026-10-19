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
//! not include windowing, texture uploads or GPU work.
use super::*;
use egui_kittest::Harness;
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
use nodecraft_types::{Choice, Modality, Number, ParamControl, ParamSchema, ParamSpec, PortType};
use std::time::{Duration, Instant};

struct Relay(NodeDescriptor);
impl NodeKind for Relay {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        None
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![])
    }
}

/// A relay with the controls a generator card shows: a toggle, a slider, a choice and a note.
fn relay_kind() -> NodeDescriptor {
    let text = PortType { modality: Modality::Text, many: false };
    let port = |name: &str| nodecraft_graph::Port { name: name.into(), port_type: text };
    let spec = |name: &str, control: ParamControl, default: Value| ParamSpec {
        name: name.into(),
        label: name.into(),
        description: None,
        control,
        required: false,
        default: Some(default),
        visible_if: None,
    };
    let options = vec![Choice { value: "draft".into(), label: "Draft".into() }, Choice { value: "best".into(), label: "Best".into() }];
    NodeDescriptor {
        id: "perf.relay".into(),
        label: "Relay".into(),
        category: Category::Action,
        inputs: vec![port("in")],
        outputs: vec![port("out")],
        params: ParamSchema {
            params: vec![
                spec("enabled", ParamControl::Toggle, Value::Bool(true)),
                spec(
                    "strength",
                    ParamControl::Slider { min: Number::new(0.0).ok(), max: Number::new(1.0).ok(), step: Number::new(0.05).unwrap() },
                    Value::Number(Number::new(0.5).unwrap()),
                ),
                spec("quality", ParamControl::Choice { options }, Value::Text("best".into())),
                spec("note", ParamControl::Text { multiline: true, max_bytes: 4096 }, Value::Text("Keep the palette warm and the light low.".into())),
            ],
        },
        quick_add: None,
    }
}

/// `count` nodes in a square grid: each row starts with a Text node holding a short prompt and
/// continues with relays, each wired to the one before.
fn graph(count: usize, ppp: f32) -> Harness<'static, NodeCraftApp> {
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    kinds.register(Box::new(Relay(relay_kind()))).unwrap();
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Perf").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
    let id = |n: usize| NodeId::from_bytes(u128::try_from(n + 1).unwrap().to_be_bytes());
    let mut changes = Vec::new();
    let cols = (1..).find(|c: &usize| c * c >= count).unwrap_or(1);
    for n in 0..count {
        let (row, col) = (n / cols, n % cols);
        let mut node = if col == 0 {
            let mut values = BTreeMap::new();
            values.insert("text".to_owned(), Value::Text(format!("Prompt {n}: a quiet forest at dawn, soft light through the trees.")));
            kinds.get("source.text").unwrap().descriptor().instantiate(id(n), &values).unwrap()
        } else {
            kinds.get("perf.relay").unwrap().descriptor().instantiate(id(n), &BTreeMap::new()).unwrap()
        };
        node.pos = nodecraft_graph::Position { x: col as f32 * 400.0, y: row as f32 * 420.0 };
        changes.push(Change::AddNode { canvas, node });
        if col > 0 {
            let edge = nodecraft_graph::Edge {
                id: nodecraft_types::EdgeId::from_bytes(u128::try_from(n + 1).unwrap().to_be_bytes()),
                from: nodecraft_graph::Endpoint { node: id(n - 1), port: "out".into() },
                to: nodecraft_graph::Endpoint { node: id(n), port: "in".into() },
                order: 0,
            };
            changes.push(Change::Connect { canvas, edge });
        }
    }
    nodecraft_graph::Changeset::new(changes).apply(&mut document).unwrap();
    let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
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
    }
    times.sort();
    let mean = times.iter().sum::<Duration>() / u32::try_from(count).unwrap();
    let p95 = times.get(count * 95 / 100).copied().unwrap_or_default();
    Stats { mean, p95, max: times.last().copied().unwrap_or_default() }
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

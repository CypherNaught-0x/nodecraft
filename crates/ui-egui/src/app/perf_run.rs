//! Presented-frame timing in a real window (#526): `nodecraft --perf-run <out.json>` opens a
//! generated graph, pans and zooms it while pumping frames, and writes one performance report
//! per workload. Unlike the kittest benchmark (`perf.rs`), these frames go through eframe, the
//! GPU and the compositor, so the presented interval includes everything a user waits for.
use super::{CanvasView, NodeCraftApp};
use egui::{Pos2, Vec2};
use nodecraft_engine::{Session, perf::PresentationSource};
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Port, Position, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
use nodecraft_types::{CanvasId, Choice, EdgeId, Error, Modality, NodeId, Number, ParamControl, ParamSchema, ParamSpec, PortType, ProjectId, Value};
use serde_json::json;
use std::collections::BTreeMap;

/// A relay with the controls a generator card shows: a toggle, a slider, a choice and a note.
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

fn relay_kind() -> Result<NodeDescriptor, Error> {
    let text = PortType { modality: Modality::Text, many: false };
    let port = |name: &str| Port { name: name.into(), port_type: text };
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
    let slider = ParamControl::Slider { min: Number::new(0.0).ok(), max: Number::new(1.0).ok(), step: Number::new(0.05)? };
    Ok(NodeDescriptor {
        id: "perf.relay".into(),
        label: "Relay".into(),
        category: Category::Action,
        inputs: vec![port("in")],
        outputs: vec![port("out")],
        params: ParamSchema {
            params: vec![
                spec("enabled", ParamControl::Toggle, Value::Bool(true)),
                spec("strength", slider, Value::Number(Number::new(0.5)?)),
                spec("quality", ParamControl::Choice { options }, Value::Text("best".into())),
                spec("note", ParamControl::Text { multiline: true, max_bytes: 4096 }, Value::Text("Keep the palette warm and the light low.".into())),
            ],
        },
        quick_add: None,
    })
}

/// The id of the `n`th generated node (and of the edge into it).
pub(super) fn grid_id(n: usize) -> u128 {
    u128::try_from(n).map_or(u128::MAX, |n| n.saturating_add(1))
}

/// `count` nodes in a square grid: each row starts with a Text node holding a short prompt and
/// continues with relays, each wired to the one before. At most 10,000 nodes.
pub(super) fn grid_session(count: usize) -> Result<(Session, CanvasId), Error> {
    let count = count.min(10_000);
    let mut kinds = nodecraft_nodes::Registry::builtins()?;
    kinds.register(Box::new(Relay(relay_kind()?)))?;
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Perf")?;
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document)?;
    let id = |n: usize| NodeId::from_bytes(grid_id(n).to_be_bytes());
    let cols = (1..=count).find(|c| c.saturating_mul(*c) >= count).unwrap_or(1);
    let mut changes = Vec::new();
    for n in 0..count {
        let (row, col) = (n / cols, n % cols);
        let mut node = if col == 0 {
            let mut values = BTreeMap::new();
            values.insert("text".to_owned(), Value::Text(format!("Prompt {n}: a quiet forest at dawn, soft light through the trees.")));
            kinds.get("source.text")?.descriptor().instantiate(id(n), &values)?
        } else {
            kinds.get("perf.relay")?.descriptor().instantiate(id(n), &BTreeMap::new())?
        };
        node.pos = Position { x: col as f32 * 400.0, y: row as f32 * 420.0 };
        changes.push(Change::AddNode { canvas, node });
        if col > 0 {
            let from = Endpoint { node: id(n - 1), port: "out".into() };
            let to = Endpoint { node: id(n), port: "in".into() };
            changes.push(Change::Connect { canvas, edge: Edge { id: EdgeId::from_bytes(grid_id(n).to_be_bytes()), from, to, order: 0 } });
        }
    }
    Changeset::new(changes).apply(&mut document)?;
    let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins()?, kinds)?;
    Ok((session, canvas))
}

/// Graph sizes the run opens, in order.
const GRAPHS: [usize; 2] = [200, 1000];
/// Untimed frames before each workload, so layouts and glyphs settle.
const SETTLE: usize = 30;
/// Timed frames per workload: ten seconds at 60 Hz.
const FRAMES: usize = 600;

#[derive(Clone, Copy, Debug)]
enum Workload {
    /// Steady panning at zoom 0.2, reversing every 20 frames.
    Pan,
    /// A zoom sweep between 0.1 and about 1.0 and back, so text layouts change every frame.
    Zoom,
}
const WORKLOADS: [Workload; 2] = [Workload::Pan, Workload::Zoom];

impl Workload {
    fn name(self) -> &'static str {
        match self {
            Self::Pan => "pan",
            Self::Zoom => "zoom",
        }
    }
    fn start(self) -> CanvasView {
        CanvasView { pan: Vec2::new(40.0, 40.0), zoom: if matches!(self, Self::Pan) { 0.2 } else { 0.1 } }
    }
    /// Moves the view ahead of timed frame `frame`.
    fn step(self, app: &mut NodeCraftApp, frame: usize) {
        match self {
            Self::Pan => {
                let phase = if (frame / 20).is_multiple_of(2) { 1.0 } else { -1.0 };
                app.view.pan += Vec2::new(6.0, 3.0) * phase;
            }
            Self::Zoom => {
                let factor = if frame % 160 < 80 { 1.03 } else { 1.0 / 1.03 };
                let anchor = app.viewport.center();
                let origin = app.viewport.min;
                app.view.zoom_at(origin, if anchor.is_finite() { anchor } else { Pos2::ZERO }, factor);
            }
        }
    }
}

/// The run's window: one graph at a time, each workload settled then timed.
pub struct PerfRun {
    out: std::path::PathBuf,
    source: Option<PresentationSource>,
    renderer: Option<String>,
    /// Graph sizes to open, in order, and timed frames per workload.
    graphs: Vec<usize>,
    frames: usize,
    graph: usize,
    workload: usize,
    frame: usize,
    app: Option<NodeCraftApp>,
    runs: Vec<serde_json::Value>,
    done: bool,
    outcome: Outcome,
}

/// Where the run's results went, for the host to report once the window closes: the file
/// written, or why it wasn't. `None` until the run writes.
pub type Outcome = std::sync::Arc<std::sync::Mutex<Option<Result<std::path::PathBuf, String>>>>;

impl PerfRun {
    /// A run writing to `out`. `source` names how the window presents; pass it only when
    /// presenting waits for vsync, otherwise presentation is reported unsupported.
    pub fn new(out: std::path::PathBuf, source: Option<PresentationSource>, renderer: Option<String>, outcome: Outcome) -> Self {
        Self::sized(out, source, renderer, GRAPHS.to_vec(), FRAMES, outcome)
    }

    pub(super) fn sized(
        out: std::path::PathBuf,
        source: Option<PresentationSource>,
        renderer: Option<String>,
        graphs: Vec<usize>,
        frames: usize,
        outcome: Outcome,
    ) -> Self {
        Self { out, source, renderer, graphs, frames, graph: 0, workload: 0, frame: 0, app: None, runs: Vec::new(), done: false, outcome }
    }

    /// The reports so far, as written to the output file.
    pub fn results(&self) -> serde_json::Value {
        json!({"schema": "nodecraft.perf-run/1", "frames": self.frames, "complete": self.done, "runs": self.runs})
    }

    /// Opens the next graph, or `None` once every graph has run.
    fn open(&mut self) -> Option<&mut NodeCraftApp> {
        while self.app.is_none() {
            let count = *self.graphs.get(self.graph)?;
            match grid_session(count).and_then(|(session, canvas)| NodeCraftApp::from_session(session, canvas)) {
                Ok(mut app) => {
                    if let Some(renderer) = &self.renderer {
                        app.set_renderer(renderer.clone());
                    }
                    self.app = Some(app);
                }
                Err(e) => {
                    self.runs.push(json!({"nodes": count, "error": e.message}));
                    self.graph = self.graph.saturating_add(1);
                }
            }
        }
        self.app.as_mut()
    }

    /// One frame of the run; returns `false` once the run is over.
    fn frame(&mut self, ui: &mut egui::Ui) -> bool {
        let (frame, source, frames) = (self.frame, self.source, self.frames);
        let Some(workload) = WORKLOADS.get(self.workload).copied() else {
            self.app = None;
            self.graph = self.graph.saturating_add(1);
            self.workload = 0;
            return true;
        };
        let Some(app) = self.open() else {
            return false;
        };
        if frame < SETTLE {
            // Every settle frame: a freshly opened canvas centres its view on its first frame.
            app.view = workload.start();
        } else if frame == SETTLE {
            for id in ["perf.clear", "perf.start"] {
                if let Err(e) = app.session.dispatch(id, &json!({})) {
                    log::warn!("{id}: {}", e.message);
                }
            }
            app.set_frame_pump(source);
        } else if frame > SETTLE {
            workload.step(app, frame - SETTLE);
        }
        app.show(ui);
        if frame >= SETTLE.saturating_add(frames) {
            let report = app.session.dispatch("perf.report", &json!({})).unwrap_or_else(|e| json!({"error": e.message}));
            let _ = app.session.dispatch("perf.stop", &json!({}));
            app.set_frame_pump(None);
            let nodes = self.graphs.get(self.graph).copied().unwrap_or_default();
            self.runs.push(json!({"nodes": nodes, "workload": workload.name(), "report": report}));
            self.workload = self.workload.saturating_add(1);
            self.frame = 0;
        } else {
            self.frame = frame.saturating_add(1);
        }
        true
    }

    fn write(&self) {
        let written =
            serde_json::to_vec_pretty(&self.results()).map_err(|e| e.to_string()).and_then(|bytes| std::fs::write(&self.out, bytes).map_err(|e| e.to_string()));
        let outcome = written.map(|()| self.out.clone()).map_err(|e| format!("{}: {e}", self.out.display()));
        match self.outcome.lock() {
            Ok(mut slot) => *slot = Some(outcome),
            Err(poisoned) => *poisoned.into_inner() = Some(outcome),
        }
    }
}

impl eframe::App for PerfRun {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    /// A window closed before the run finished still writes what it has, marked incomplete.
    fn on_exit(&mut self) {
        if !self.done {
            self.write();
        }
    }
}

impl PerfRun {
    /// One window frame: the run's next frame, then closing the window once it's over.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        if self.done {
            return;
        }
        if !self.frame(ui) {
            self.done = true;
            self.write();
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests;

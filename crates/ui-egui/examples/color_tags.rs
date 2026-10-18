//! Public-safe native sample for inspecting colour tagging, filtering and undo.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
use nodecraft_graph::{Change, Changeset};
use nodecraft_types::{Error, ErrorKind, NodeId, Value};
use nodecraft_ui_egui::NodeCraftApp;
fn sample() -> Result<NodeCraftApp, Error> {
    let base = NodeCraftApp::headless()?;
    let c = base.canvas;
    let mut p = base.session.document().clone();
    let first =
        p.canvases().get(&c).and_then(|c| c.nodes().values().next()).cloned().ok_or_else(|| Error::new(ErrorKind::NotFound, "sample node unavailable"))?;
    let mut changes = Vec::new();
    for (n, x, text) in [(3, -400.0, "Sunrise study"), (4, 0.0, "Mountain study"), (5, 400.0, "Ocean study")] {
        let mut node = first.clone();
        node.id = NodeId::from_bytes([n; 16]);
        node.title = format!("Study {}", n - 2);
        node.pos.x = x;
        node.params.insert("text".into(), Value::Text(text.into()));
        changes.push(if n == 3 { Change::SetNode { canvas: c, node } } else { Change::AddNode { canvas: c, node } });
    }
    Changeset::new(changes).apply(&mut p)?;
    NodeCraftApp::from_session(nodecraft_engine::Session::new(p)?, c)
}
#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    eframe::run_native(
        "NodeCraft · Colour tags",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_title("NodeCraft · Colour tags").with_inner_size([1440.0, 900.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            nodecraft_ui_egui::theme::apply(&cc.egui_ctx, nodecraft_ui_egui::theme::ThemeKind::Dark);
            Ok(Box::new(sample()?))
        }),
    )
}
#[cfg(target_arch = "wasm32")]
fn main() {
    let _ = sample();
}

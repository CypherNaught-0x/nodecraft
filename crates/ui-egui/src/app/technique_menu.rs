//! A technique node's card actions (#94): view the version it runs, edit the technique it came
//! from, upgrade it to the latest version, or detach it into the nodes it runs.
use super::NodeCraftApp;
use crate::ids;
use nodecraft_graph::{Node, Project};
use nodecraft_nodes::technique;
use nodecraft_types::{CanvasId, NodeId};
use serde_json::json;

/// What a technique node's context menu asked for, applied after the frame is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TechniqueAction {
    View(CanvasId),
    Edit(CanvasId),
    Upgrade(NodeId, CanvasId),
    Detach(NodeId),
    /// Open the node as a form (#95).
    App(NodeId),
}

/// What the menu offers for one technique node.
struct Offer {
    version: u32,
    /// The pinned version's canvas, while it isn't removed.
    view: Option<CanvasId>,
    /// The canvas the technique was published from, while it is still in the project.
    source: Option<CanvasId>,
    /// The newest version after the pinned one, and its number.
    upgrade: Option<(CanvasId, u32)>,
}

fn offer(project: &Project, node: &Node) -> Option<Offer> {
    let interface = technique::interface(node).ok()?;
    let canvases = project.canvases();
    let pinned = canvases.get(&interface.canvas).and_then(|canvas| canvas.technique_info()).filter(|info| info.id == interface.technique);
    let view = pinned.filter(|info| !info.retired).map(|_| interface.canvas);
    // A removed version keeps its source, so its node can still be edited from it.
    let source = pinned.and_then(|info| info.source).filter(|source| canvases.contains_key(source));
    let upgrade = canvases
        .values()
        .filter_map(|canvas| canvas.technique_info().map(|info| (canvas.id(), info)))
        .filter(|(_, info)| info.id == interface.technique && !info.retired && info.version > interface.version)
        .max_by_key(|(_, info)| info.version)
        .map(|(id, info)| (id, info.version));
    Some(Offer { version: interface.version, view, source, upgrade })
}

/// The technique items of `node`'s context menu; nothing for any other kind of node.
pub(super) fn menu(ui: &mut egui::Ui, project: &Project, node: &Node) -> Option<TechniqueAction> {
    if node.kind != technique::ID {
        return None;
    }
    let offer = offer(project, node)?;
    let mut action = None;
    ui.separator();
    ui.label(format!("Technique · v{}", offer.version));
    if ui.button("Open as app").on_hover_text("Fill its inputs, set it up and run it from a simple form").clicked() {
        action = Some(TechniqueAction::App(node.id));
    }
    let view = ui.add_enabled(offer.view.is_some(), egui::Button::new("View workflow"));
    if let Some(canvas) = offer.view
        && view.on_hover_text("Open the version this node runs; it is read-only").clicked()
    {
        action = Some(TechniqueAction::View(canvas));
    }
    let edit = ui.add_enabled(offer.source.is_some(), egui::Button::new("Edit technique"));
    if let Some(canvas) = offer.source
        && edit.on_hover_text("Open the canvas it was published from, to change and publish it again").clicked()
    {
        action = Some(TechniqueAction::Edit(canvas));
    }
    if let Some((canvas, version)) = offer.upgrade
        && ui.button(format!("Upgrade to v{version}")).on_hover_text("Run the newest version; settings and wires that no longer fit come off").clicked()
    {
        action = Some(TechniqueAction::Upgrade(node.id, canvas));
    }
    if ui.button("Detach").on_hover_text("Replace this node with the nodes it runs").clicked() {
        action = Some(TechniqueAction::Detach(node.id));
    }
    if action.is_some() {
        ui.close();
    }
    action
}

impl NodeCraftApp {
    /// Applies what a technique node's context menu asked for.
    pub(super) fn technique_action(&mut self, action: TechniqueAction) {
        let canvas = self.canvas;
        let result = match action {
            TechniqueAction::View(open) | TechniqueAction::Edit(open) => {
                self.open_canvas(open);
                Ok(())
            }
            TechniqueAction::App(node) => {
                self.open_app_mode(node);
                Ok(())
            }
            TechniqueAction::Upgrade(node, version) => {
                self.session.dispatch("technique.upgrade", &json!({"canvas": canvas, "node": node, "version": version})).map(|_| ())
            }
            TechniqueAction::Detach(node) => ids::random_id(|_| false).map(NodeId::from_bytes).and_then(|seed| {
                let detached = self.session.dispatch("technique.detach", &json!({"canvas": canvas, "node": node, "seed": seed}))?;
                let nodes: Vec<NodeId> = serde_json::from_value(detached["nodes"].clone()).unwrap_or_default();
                self.selection.replace(nodes.into_iter().collect());
                Ok(())
            }),
        };
        self.notice = result.err().map(|error| error.message);
    }
}

//! Trained styles (#167) from the canvas: a Train style card's context menu starts an image card
//! with its style (`style.card`), and "Use style" in a styleable card's menu lists the canvas's
//! Train style cards and wires the chosen one in (`style.use`).
use super::NodeCraftApp;
use crate::ids;
use nodecraft_engine::generation_catalog::GenerationCatalog;
use nodecraft_engine::style_cmds;
use nodecraft_graph::{Canvas, Node};
use nodecraft_types::NodeId;
use serde_json::json;

/// How far right of the Train style card its new image card lands, in world units.
const GAP: f32 = 420.0;

/// What a card's context menu asked for, applied after the frame is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StyleAction {
    /// Wire this Train style card into the target card.
    Use { target: NodeId, style: NodeId },
    /// Start an image card with this Train style card's style.
    Card(NodeId),
}

/// The style items of `node`'s context menu: "New image card with this style" on a trained Train
/// style card, "Use style" on a card that takes styles, nothing otherwise.
pub(super) fn menu(ui: &mut egui::Ui, canvas: &Canvas, catalog: Option<&GenerationCatalog>, node: &Node) -> Option<StyleAction> {
    if node.kind == nodecraft_nodes::generation::STYLE_KIND {
        let r = ui
            .add_enabled(style_cmds::trained(node), egui::Button::new("New image card with this style"))
            .on_disabled_hover_text("Run this card to train its style first");
        if r.clicked() {
            ui.close();
            return Some(StyleAction::Card(node.id));
        }
        return None;
    }
    use_menu(ui, canvas, catalog, node).map(|style| StyleAction::Use { target: node.id, style })
}

/// The "Use style" submenu of `node`'s context menu, for a card whose model can take a style;
/// the chosen Train style card, if any.
fn use_menu(ui: &mut egui::Ui, canvas: &Canvas, catalog: Option<&GenerationCatalog>, node: &Node) -> Option<NodeId> {
    if !style_cmds::takes_styles(node) {
        return None;
    }
    let mut chosen = None;
    ui.menu_button("Use style", |ui| {
        let offers = style_cmds::offers(canvas, catalog, node);
        if offers.is_empty() {
            ui.add_enabled(false, egui::Button::new("No Train style cards on this canvas"));
        }
        for offer in offers {
            // Highlighted rather than ticked: the UI font has no check mark.
            let button = ui.add_enabled(offer.refusal.is_none() && !offer.wired, egui::Button::new(offer.label).selected(offer.wired));
            let button = match &offer.refusal {
                Some(why) => button.on_disabled_hover_text(why.as_str()),
                None if offer.wired => button.on_disabled_hover_text("This card uses this style"),
                None => button,
            };
            if button.clicked() {
                chosen = Some(offer.style);
                ui.close();
            }
        }
    });
    chosen
}

impl NodeCraftApp {
    pub(super) fn style_action(&mut self, action: StyleAction) {
        match action {
            StyleAction::Use { target, style } => self.use_style(target, style),
            StyleAction::Card(style) => self.style_card(style),
        }
    }

    /// Adds an image card with the trained style of `style`, beside it, and selects it.
    fn style_card(&mut self, style: NodeId) {
        let document = self.session.document();
        let Some(source) = document.canvases().get(&self.canvas).and_then(|canvas| canvas.nodes().get(&style)) else { return };
        let revision = self.session.revision();
        let (Some(id), Some(edge)) = (ids::node_id(document, ("style-card", revision)), ids::edge_id(document, ("style-card-wire", revision))) else {
            self.notice = Some("Couldn't allocate node ids; try again.".into());
            return;
        };
        let params = json!({"canvas": self.canvas, "style": style, "id": id, "edge": edge, "pos": {"x": source.pos.x + GAP, "y": source.pos.y}});
        match self.session.dispatch("style.card", &params) {
            Ok(_) => {
                self.notice = None;
                self.selection.only(id);
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// Wires the trained style of `style` into `target`'s style input, replacing the one there.
    fn use_style(&mut self, target: NodeId, style: NodeId) {
        let document = self.session.document();
        let Some(edge) = ids::edge_id(document, ("style-wire", self.session.revision(), target)) else {
            self.notice = Some("Couldn't allocate an edge id; try again.".into());
            return;
        };
        let params = json!({"canvas": self.canvas, "style": style, "target": target, "edge": edge});
        match self.session.dispatch("style.use", &params) {
            Ok(_) => self.notice = None,
            Err(error) => self.notice = Some(error.message),
        }
    }
}

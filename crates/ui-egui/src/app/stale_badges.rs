//! Stale badges (#211): a card whose stored result no longer matches its inputs shows a badge,
//! and clicking it quotes a refresh of that card with whatever upstream it needs. The set comes
//! from `canvas.stale`, asked once per document revision and canvas, never per frame.
use super::*;

/// At most this many badges are kept; a larger report keeps the first ones.
const MAX_BADGES: usize = 4096;

pub(super) struct StaleBadges {
    session: Arc<()>,
    canvas: CanvasId,
    revision: u64,
    pub(super) nodes: BTreeSet<NodeId>,
}

impl NodeCraftApp {
    /// Asks the engine which cards are stale when the document or canvas changed since last
    /// asked. A failed check shows no badges rather than guessing.
    pub(super) fn refresh_stale_badges(&mut self) {
        let session = self.session.view_identity();
        let revision = self.session.revision();
        if self.stale_badges.as_ref().is_some_and(|b| Arc::ptr_eq(&b.session, &session) && b.canvas == self.canvas && b.revision == revision) {
            return;
        }
        let nodes = match self.session.dispatch("canvas.stale", &json!({"canvas": self.canvas})) {
            Ok(report) => report
                .get("nodes")
                .and_then(serde_json::Value::as_array)
                .map(|nodes| nodes.iter().take(MAX_BADGES).filter_map(|id| serde_json::from_value::<NodeId>(id.clone()).ok()).collect())
                .unwrap_or_default(),
            Err(_) => BTreeSet::new(),
        };
        self.stale_badges = Some(StaleBadges { session, canvas: self.canvas, revision, nodes });
    }
}

/// Paints the badge under a stale card's bottom-right corner, in world space like the run
/// status opposite it. Returns whether it was clicked.
pub(super) fn badge(ui: &egui::Ui, painter: &egui::Painter, view: &CanvasView, viewport: Rect, card: Rect, id: NodeId, t: &Tokens) -> bool {
    let galley = painter.layout_no_wrap("Stale ↻".to_owned(), egui::FontId::proportional(t.label_size), t.card_fill);
    let size = galley.size() + Vec2::new(t.control_gap * 2.0, t.control_gap);
    let rect = Rect::from_min_size(card.right_bottom() + Vec2::new(-size.x, t.control_gap), size);
    painter.rect_filled(rect, size.y / 2.0, t.stale);
    painter.galley(rect.center() - galley.size() / 2.0, galley, t.card_fill);
    let response = ui.interact(view.rect_to_screen(viewport.min, rect), ui.id().with(("stale_badge", id)), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Stale result: refresh"));
    response.on_hover_text("This result no longer matches its inputs. Click to refresh it.").clicked()
}

//! Stale badges (#211): a card whose stored result no longer matches its inputs shows a badge,
//! and clicking it quotes a refresh of that card with whatever upstream it needs. The set comes
//! from `canvas.stale`, asked once per document revision and canvas, never per frame.
//! The first card whose inputs can't be hashed shows a "can't check" badge instead, with the
//! engine's bounded reason on hover (#254).
use super::stale::{MAX_REASON, kind, normalized};
use super::*;

/// At most this many badges are kept; a larger report keeps the first ones.
const MAX_BADGES: usize = 4096;

pub(super) struct StaleBadges {
    session: Arc<()>,
    canvas: CanvasId,
    revision: u64,
    pub(super) nodes: BTreeSet<NodeId>,
    /// The first checked card whose inputs couldn't be hashed, and the hover text naming why.
    pub(super) unchecked: Option<(NodeId, String)>,
}

/// The hover text for the report's first unhashable card, or `None` when there is none or the
/// diagnostic is malformed: every field is checked and the reason bounded and sanitised, as the
/// palette's summary does.
pub(super) fn unchecked(report: &serde_json::Value) -> Option<(NodeId, String)> {
    let diagnostic = report.get("unknown").filter(|d| !d.is_null())?;
    let uuid = |field: &str| diagnostic.get(field).and_then(serde_json::Value::as_str).filter(|s| s.len() == 36)?.parse::<NodeId>().ok();
    let node = uuid("node")?;
    let cause = uuid("cause")?;
    let category = diagnostic.get("kind").and_then(serde_json::Value::as_str).and_then(kind)?;
    let reason = diagnostic.get("reason").and_then(serde_json::Value::as_str).filter(|s| s.len() <= MAX_REASON)?;
    let others = report.get("unknown_total").and_then(serde_json::Value::as_u64).map_or(0, |n| n.saturating_sub(1));
    let mut text = String::new();
    text.push_str("Can't check whether this result matches its inputs, so it may be out of date. ");
    if cause == node {
        text.push_str("This card");
    } else {
        text.push_str("Input node ");
        text.push_str(&cause.to_string());
    }
    text.push_str(" failed (");
    text.push_str(category);
    text.push_str("): ");
    normalized(&mut text, reason).ok()?;
    if others > 0 {
        text.push_str(&format!("\n{others} more checked results can't be checked either; List stale results counts them."));
    }
    Some((node, text))
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
        let (nodes, unchecked) = match self.session.dispatch("canvas.stale", &json!({"canvas": self.canvas})) {
            Ok(report) => (
                report
                    .get("nodes")
                    .and_then(serde_json::Value::as_array)
                    .map(|nodes| nodes.iter().take(MAX_BADGES).filter_map(|id| serde_json::from_value::<NodeId>(id.clone()).ok()).collect())
                    .unwrap_or_default(),
                unchecked(&report),
            ),
            Err(_) => (BTreeSet::new(), None),
        };
        self.stale_badges = Some(StaleBadges { session, canvas: self.canvas, revision, nodes, unchecked });
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

/// Paints the "can't check" badge where a stale badge would be, with `hover` naming the failing
/// input and why. It offers no refresh: a run would hit the same failure.
#[allow(clippy::too_many_arguments)]
pub(super) fn unchecked_badge(ui: &egui::Ui, painter: &egui::Painter, view: &CanvasView, viewport: Rect, card: Rect, id: NodeId, hover: &str, t: &Tokens) {
    let galley = painter.layout_no_wrap("Can't check".to_owned(), egui::FontId::proportional(t.label_size), t.card_fill);
    let size = galley.size() + Vec2::new(t.control_gap * 2.0, t.control_gap);
    let rect = Rect::from_min_size(card.right_bottom() + Vec2::new(-size.x, t.control_gap), size);
    painter.rect_filled(rect, size.y / 2.0, t.danger);
    painter.galley(rect.center() - galley.size() / 2.0, galley, t.card_fill);
    let response = ui.interact(view.rect_to_screen(viewport.min, rect), ui.id().with(("unchecked_badge", id)), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, "Inputs can't be checked"));
    response.on_hover_text(hover);
}

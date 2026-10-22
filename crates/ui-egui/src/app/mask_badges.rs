//! Mask badges (#546): a mask whose image changed since its strokes were painted shows a badge,
//! and clicking it opens the mask editor, where the strokes are kept or fitted. The set comes
//! from `mask.status`, asked once per document revision and canvas, never per frame.
use super::*;

/// At most this many badges are kept; the engine lists no more.
const MAX_BADGES: usize = 4096;

/// How a mask's strokes relate to the image under it, as `mask.status` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Drift {
    /// The image changed but kept its shape.
    Changed,
    /// The image changed shape: a run using the mask is blocked until the strokes are kept or fitted.
    Reshaped,
}

pub(super) struct MaskBadges {
    session: Arc<()>,
    canvas: CanvasId,
    revision: u64,
    pub(super) masks: BTreeMap<NodeId, Drift>,
}

impl NodeCraftApp {
    /// Asks the engine which masks drifted from their image when the document or canvas changed
    /// since last asked. A failed check shows no badges rather than guessing.
    pub(super) fn refresh_mask_badges(&mut self) {
        let session = self.session.view_identity();
        let revision = self.session.revision();
        if self.mask_badges.as_ref().is_some_and(|b| Arc::ptr_eq(&b.session, &session) && b.canvas == self.canvas && b.revision == revision) {
            return;
        }
        let masks = match self.session.dispatch("mask.status", &json!({"canvas": self.canvas})) {
            Ok(report) => report
                .get("masks")
                .and_then(serde_json::Value::as_array)
                .map(|masks| {
                    masks
                        .iter()
                        .take(MAX_BADGES)
                        .filter_map(|m| {
                            let id = serde_json::from_value::<NodeId>(m.get("id")?.clone()).ok()?;
                            let drift = match m.get("binding")?.as_str()? {
                                "changed" => Drift::Changed,
                                "reshaped" => Drift::Reshaped,
                                _ => return None,
                            };
                            Some((id, drift))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => BTreeMap::new(),
        };
        self.mask_badges = Some(MaskBadges { session, canvas: self.canvas, revision, masks });
    }
}

/// Paints the badge under a drifted mask's bottom-right corner, where a stale badge would be.
/// Returns whether it was clicked.
#[allow(clippy::too_many_arguments)]
pub(super) fn badge(ui: &egui::Ui, painter: &egui::Painter, view: &CanvasView, viewport: Rect, card: Rect, id: NodeId, drift: Drift, t: &Tokens) -> bool {
    let (text, fill, label, hover) = match drift {
        Drift::Changed => (
            "Image changed",
            t.stale,
            "Mask image changed: check the strokes",
            "The image under this mask changed since it was painted. Click to check the strokes still cover the right area.",
        ),
        Drift::Reshaped => (
            "Image reshaped",
            t.danger,
            "Mask image reshaped: keep or fit the strokes",
            "The image under this mask changed shape, so its strokes would stretch. Click to keep them or fit them onto the new image.",
        ),
    };
    let galley = painter.layout_no_wrap(text.to_owned(), egui::FontId::proportional(t.label_size), t.card_fill);
    let size = galley.size() + Vec2::new(t.control_gap * 2.0, t.control_gap);
    let rect = Rect::from_min_size(card.right_bottom() + Vec2::new(-size.x, t.control_gap), size);
    painter.rect_filled(rect, size.y / 2.0, fill);
    painter.galley(rect.center() - galley.size() / 2.0, galley, t.card_fill);
    let response = ui.interact(view.rect_to_screen(viewport.min, rect), ui.id().with(("mask_badge", id)), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    response.on_hover_text(hover).clicked()
}

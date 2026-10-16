//! Node cards on the canvas.
//!
//! M0 draws a stub card from plain data ([`StubCard`]); in M1 the same painter takes a
//! `nodecraft_graph::Node` and its kind's body renderer.

use egui::{Align2, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::canvas::CanvasView;
use crate::theme::Tokens;

/// Below this zoom a card shows its label only (level of detail).
pub const BODY_MIN_ZOOM: f32 = 0.35;
/// Text smaller than this on screen is not drawn at all.
pub const MIN_TEXT_PX: f32 = 5.0;

/// A placeholder node: a label, some text, and a world-space rectangle.
#[derive(Clone, Debug, PartialEq)]
pub struct StubCard {
    pub id: u64,
    /// Shown small and uppercase above the card.
    pub label: String,
    pub body: String,
    /// Position and size in world coordinates.
    pub rect: Rect,
}

/// Font size on screen for a token size at `zoom`, rounded to whole points so continuous zooming
/// does not rasterise a new glyph atlas entry every frame. `None` when too small to read.
pub fn text_px(size: f32, zoom: f32) -> Option<f32> {
    let px = (size * zoom).round();
    (px.is_finite() && px >= MIN_TEXT_PX).then_some(px)
}

/// Screen position of the `+` output handle: centred on the card's right edge.
pub fn handle_center(screen_rect: Rect) -> Pos2 {
    screen_rect.right_center()
}

/// Paint `card` and handle its pointer input. Dragging the card moves it (in world units);
/// returns the card's response so the caller can track selection.
pub fn show(ui: &mut egui::Ui, origin: Pos2, view: &CanvasView, card: &mut StubCard, selected: bool, t: &Tokens) -> egui::Response {
    let screen = view.rect_to_screen(origin, card.rect);
    let response = ui.interact(screen, ui.id().with(("card", card.id)), Sense::click_and_drag());
    if response.dragged_by(egui::PointerButton::Primary) {
        let delta = response.drag_delta() / view.zoom;
        if delta.is_finite() {
            card.rect = card.rect.translate(delta);
        }
    }
    let screen = view.rect_to_screen(origin, card.rect);
    let painter = ui.painter();
    if !painter.clip_rect().intersects(screen.expand(t.handle_radius * view.zoom + 24.0)) {
        return response;
    }

    // Label above the card.
    if let Some(px) = text_px(t.label_size, view.zoom) {
        let pos = screen.left_top() - Vec2::new(0.0, t.label_gap * view.zoom);
        painter.text(pos, Align2::LEFT_BOTTOM, card.label.to_uppercase(), FontId::monospace(px), t.text_muted);
    }

    // Body.
    let stroke = if selected {
        t.card_stroke_selected
    } else if response.hovered() {
        t.card_stroke_hover
    } else {
        t.card_stroke
    };
    let radius = (f32::from(t.card_radius) * view.zoom).round().clamp(0.0, 255.0) as u8;
    painter.rect(screen, radius, t.card_fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
    if view.zoom >= BODY_MIN_ZOOM
        && let Some(px) = text_px(t.body_size, view.zoom)
    {
        let pad = t.card_padding * view.zoom;
        let inner = screen.shrink(pad);
        if inner.width() > 0.0 && inner.height() > 0.0 {
            let galley = painter.layout(card.body.clone(), FontId::proportional(px), t.text, inner.width());
            painter.with_clip_rect(inner.intersect(painter.clip_rect())).galley(inner.min, galley, t.text);
        }
    }

    // `+` output handle on the right edge. Fixed minimum size so it stays a target when zoomed out.
    let c = handle_center(screen);
    let r = (t.handle_radius * view.zoom).max(5.0);
    painter.circle(c, r, t.handle_fill, Stroke::new(1.5, t.handle_glyph));
    let arm = r * 0.5;
    painter.line_segment([c - Vec2::X * arm, c + Vec2::X * arm], Stroke::new(1.5, t.handle_glyph));
    painter.line_segment([c - Vec2::Y * arm, c + Vec2::Y * arm], Stroke::new(1.5, t.handle_glyph));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_sizes_are_whole_points_and_vanish_when_tiny() {
        assert_eq!(text_px(13.0, 1.0), Some(13.0));
        assert_eq!(text_px(13.0, 1.07), Some(14.0));
        assert_eq!(text_px(13.0, 0.3), None);
        assert_eq!(text_px(13.0, f32::NAN), None);
        assert_eq!(text_px(10.5, 4.0), Some(42.0));
    }

    #[test]
    fn handle_sits_on_the_right_edge() {
        let r = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(200.0, 100.0));
        assert_eq!(handle_center(r), Pos2::new(210.0, 70.0));
    }
}

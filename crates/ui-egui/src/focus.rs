//! A visible ring on painted canvas widgets that have keyboard focus (#1108).
//!
//! A card, a field header, a badge or the minimap takes focus from a click as well as from Tab,
//! and Enter acts on the focused one, which on a generation card asks for a priced quote. The
//! ring shows which one that is whenever focus came from the keyboard (Tab, the arrow keys or a
//! screen reader), in `accent`, `focus_ring_gap` outside the widget and `focus_ring_width` wide,
//! clear of the selected border inside it and the presence ring further out.
use crate::theme::Tokens;
use egui::{Rect, Stroke, StrokeKind};

/// The widget a pointer last pressed, so a focus it took from that press shows no ring.
fn pointer_focus_id() -> egui::Id {
    egui::Id::new("nodecraft.focus.pointer")
}

/// Does `response` have focus that a person moved there from the keyboard (or a screen reader)
/// rather than with a click or drag? Call once per frame for each focusable painted widget.
pub fn keyboard_focused(ui: &egui::Ui, response: &egui::Response) -> bool {
    let ctx = ui.ctx();
    // Only a real pointer press counts: `clicked()` is also true for Enter, Space and a screen
    // reader's activate on the focused widget, which must keep the ring.
    if response.is_pointer_button_down_on() || response.clicked_by(egui::PointerButton::Primary) || response.drag_started_by(egui::PointerButton::Primary) {
        ctx.data_mut(|data| data.insert_temp(pointer_focus_id(), Some(response.id)));
    }
    // The keys egui moves focus with: Tab and Shift+Tab, and the bare arrow keys.
    let moved = ctx.input(|i| {
        (i.key_pressed(egui::Key::Tab) && (i.modifiers.is_none() || i.modifiers.shift_only()))
            || (i.modifiers.is_none()
                && [egui::Key::ArrowUp, egui::Key::ArrowDown, egui::Key::ArrowLeft, egui::Key::ArrowRight].into_iter().any(|key| i.key_pressed(key)))
    });
    if moved {
        ctx.data_mut(|data| data.insert_temp::<Option<egui::Id>>(pointer_focus_id(), None));
    }
    let by_pointer = ctx.data(|data| data.get_temp::<Option<egui::Id>>(pointer_focus_id())).flatten() == Some(response.id);
    // Once the pressed widget loses focus, a later focus by keyboard or screen reader counts.
    if by_pointer && response.lost_focus() {
        ctx.data_mut(|data| data.insert_temp::<Option<egui::Id>>(pointer_focus_id(), None));
    }
    response.has_focus() && !by_pointer
}

/// Paints the focus ring around `rect` (corner radius `radius`, in the painter's units). `scale`
/// converts screen points to those units: `1 / zoom` for a painter in world space, `1` on screen.
pub fn paint_ring(painter: &egui::Painter, rect: Rect, radius: f32, scale: f32, t: &Tokens) {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let gap = t.focus_ring_gap * scale;
    painter.rect_stroke(rect.expand(gap), radius + gap, Stroke::new(t.focus_ring_width * scale, t.accent), StrokeKind::Outside);
}

/// Paints the ring just inside `rect`, for a widget whose painter clips at its own edge (the
/// minimap).
pub fn paint_ring_inside(painter: &egui::Painter, rect: Rect, radius: f32, t: &Tokens) {
    let inset = t.focus_ring_gap + t.focus_ring_width;
    paint_ring(painter, rect.shrink(inset), (radius - inset).max(0.0), 1.0, t);
}

/// [`keyboard_focused`] then [`paint_ring`]: the one call a painted widget makes.
pub fn ring_if_focused(ui: &egui::Ui, painter: &egui::Painter, response: &egui::Response, rect: Rect, radius: f32, scale: f32, t: &Tokens) -> bool {
    let focused = keyboard_focused(ui, response);
    if focused {
        paint_ring(painter, rect, radius, scale, t);
    }
    focused
}

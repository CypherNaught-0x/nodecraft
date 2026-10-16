//! The infinite canvas: view transform (pan and zoom), the dot grid, and background navigation.
//!
//! World coordinates are canvas points at 100% zoom; screen coordinates are egui points.
//! `screen = origin + pan + world * zoom`, where `origin` is the top-left of the canvas rect.

use egui::{Pos2, Rect, Vec2};

use nodecraft_engine::settings::{CanvasSettings, ScrollAction};

use crate::theme::Tokens;

pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 4.0;
/// Zoom factor of one keyboard zoom step (⌘= / ⌘-).
pub const KEY_ZOOM_STEP: f32 = 1.25;
/// Grid dots closer together than this on screen are thinned out (every other dot).
pub const MIN_DOT_SPACING: f32 = 12.0;
/// Upper bound on dots painted per frame, whatever the window size.
pub const MAX_DOTS: usize = 120_000;
/// With scroll-to-zoom, scrolling this many points doubles (or halves) the zoom at zoom speed 1:
/// 200·ln 2, the same rate as egui's ⌘-scroll zoom, so both feel alike.
pub const SCROLL_POINTS_PER_DOUBLING: f32 = 138.6;

/// Pan and zoom of a canvas. Pure data: no egui state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasView {
    /// Screen offset of the world origin from the canvas rect's top-left.
    pub pan: Vec2,
    pub zoom: f32,
}

impl Default for CanvasView {
    fn default() -> Self {
        Self { pan: Vec2::ZERO, zoom: 1.0 }
    }
}

impl CanvasView {
    pub fn to_screen(&self, origin: Pos2, world: Pos2) -> Pos2 {
        origin + self.pan + world.to_vec2() * self.zoom
    }

    pub fn to_world(&self, origin: Pos2, screen: Pos2) -> Pos2 {
        ((screen - origin - self.pan) / self.zoom).to_pos2()
    }

    pub fn rect_to_screen(&self, origin: Pos2, world: Rect) -> Rect {
        Rect::from_min_max(self.to_screen(origin, world.min), self.to_screen(origin, world.max))
    }

    /// The part of the world visible in the screen rect `viewport`.
    pub fn visible_world(&self, viewport: Rect) -> Rect {
        Rect::from_min_max(self.to_world(viewport.min, viewport.min), self.to_world(viewport.min, viewport.max))
    }

    /// Multiply the zoom by `factor`, keeping the world point under `anchor` (screen) in place.
    /// The zoom is clamped to [`MIN_ZOOM`]..=[`MAX_ZOOM`]; non-finite or non-positive factors
    /// are ignored.
    pub fn zoom_at(&mut self, origin: Pos2, anchor: Pos2, factor: f32) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let world = self.to_world(origin, anchor);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan = anchor - origin - world.to_vec2() * self.zoom;
    }

    /// Move the view by a screen-space delta. Non-finite deltas are ignored.
    pub fn pan_by(&mut self, delta: Vec2) {
        if delta.is_finite() {
            self.pan += delta;
        }
    }

    /// 100% zoom with the world point `center` in the middle of `viewport`.
    pub fn reset(&mut self, viewport: Rect, center: Pos2) {
        self.zoom = 1.0;
        self.pan = viewport.size() * 0.5 - center.to_vec2();
    }
}

/// World distance between painted grid dots: the token spacing, doubled until the dots are at
/// least [`MIN_DOT_SPACING`] apart on screen, so zooming out never turns the grid into a haze.
pub fn grid_step(spacing: f32, zoom: f32) -> f32 {
    let mut step = spacing.max(1.0);
    let zoom = if zoom.is_finite() && zoom > 0.0 { zoom } else { 1.0 };
    while step * zoom < MIN_DOT_SPACING {
        step *= 2.0;
    }
    step
}

/// Paint the dot grid into `viewport`. Dots stay a fixed screen size, so the grid reads as
/// texture rather than as content. At this size a square is indistinguishable from a circle and
/// tessellates to 4 vertices instead of a fan, which matters at 100k dots.
pub fn paint_grid(painter: &egui::Painter, viewport: Rect, view: &CanvasView, t: &Tokens) {
    let step = grid_step(t.grid_spacing, view.zoom);
    let world = view.visible_world(viewport);
    let first = Pos2::new((world.min.x / step).floor() * step, (world.min.y / step).floor() * step);
    let cols = ((world.max.x - first.x) / step).ceil().max(0.0) as usize + 1;
    let rows = ((world.max.y - first.y) / step).ceil().max(0.0) as usize + 1;
    if cols.saturating_mul(rows) > MAX_DOTS {
        return;
    }
    for row in 0..rows {
        for col in 0..cols {
            let w = Pos2::new(first.x + col as f32 * step, first.y + row as f32 * step);
            let r = Rect::from_center_size(view.to_screen(viewport.min, w), Vec2::splat(t.grid_dot_radius * 2.0));
            painter.rect_filled(r, 0.0, t.grid_dot);
        }
    }
}

/// The zoom factor and pan for one frame's scroll and pinch input, under the user's preferences.
/// `scroll` is the frame's scroll delta in points and `pinch` egui's zoom delta (pinch and
/// ⌘/Ctrl-scroll). Returns `(zoom factor, pan delta)`.
pub fn scroll_to_view_change(scroll: Vec2, pinch: f32, prefs: &CanvasSettings) -> (f32, Vec2) {
    let speed = prefs.zoom_speed();
    let mut zoom = if pinch.is_finite() && pinch > 0.0 { pinch.powf(speed) } else { 1.0 };
    let pan = match prefs.scroll {
        ScrollAction::Pan => scroll,
        ScrollAction::Zoom => {
            if scroll.y.is_finite() {
                zoom *= (scroll.y * speed / SCROLL_POINTS_PER_DOUBLING).exp2();
            }
            Vec2::new(scroll.x, 0.0)
        }
    };
    (zoom, pan)
}

/// Text editors own document/canvas keyboard shortcuts while focused.
pub fn text_input_focused(ctx: &egui::Context) -> bool {
    ctx.memory(|memory| memory.focused()).is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some())
}

/// Background navigation for one frame: drag with the primary button on empty canvas, or with the
/// middle or secondary button anywhere, pans; scroll (two-finger trackpad, wheel) pans, or zooms
/// with [`ScrollAction::Zoom`]; pinch or ⌘/Ctrl-scroll zooms around the pointer; ⌘0 resets, ⌘= and
/// ⌘- zoom around the centre.
///
/// `background` is the response of the canvas-filling background widget.
///
/// `primary_pans` is false while a primary drag on the background is a box select instead.
pub fn navigate(
    ui: &egui::Ui,
    background: &egui::Response,
    view: &mut CanvasView,
    home: Pos2,
    prefs: &CanvasSettings,
    allow_scroll_pan: bool,
    primary_pans: bool,
) {
    let viewport = background.rect;
    let origin = viewport.min;
    let panning = (primary_pans && background.dragged_by(egui::PointerButton::Primary))
        || [egui::PointerButton::Middle, egui::PointerButton::Secondary].into_iter().any(|b| background.dragged_by(b));
    if panning {
        view.pan_by(background.drag_delta());
    }
    let (scroll, zoom, pointer, reset, zoom_in, zoom_out) = ui.input(|i| {
        let cmd = egui::Modifiers::COMMAND;
        (
            i.smooth_scroll_delta,
            i.zoom_delta(),
            i.pointer.hover_pos(),
            i.modifiers.matches_logically(cmd) && i.key_pressed(egui::Key::Num0),
            i.modifiers.matches_logically(cmd) && (i.key_pressed(egui::Key::Equals) || i.key_pressed(egui::Key::Plus)),
            i.modifiers.matches_logically(cmd) && i.key_pressed(egui::Key::Minus),
        )
    });
    let hovered = pointer.is_some_and(|p| viewport.contains(p));
    if hovered {
        let (zoom, pan) = scroll_to_view_change(if allow_scroll_pan { scroll } else { Vec2::ZERO }, zoom, prefs);
        view.pan_by(pan);
        if zoom != 1.0
            && let Some(p) = pointer
        {
            view.zoom_at(origin, p, zoom);
        }
    }
    let keyboard_free = !text_input_focused(ui.ctx());
    if reset && keyboard_free {
        view.reset(viewport, home);
    }
    if zoom_in && keyboard_free {
        view.zoom_at(origin, viewport.center(), KEY_ZOOM_STEP);
    }
    if zoom_out && keyboard_free {
        view.zoom_at(origin, viewport.center(), 1.0 / KEY_ZOOM_STEP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const O: Pos2 = Pos2::new(40.0, 30.0);

    fn close(a: Pos2, b: Pos2) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn screen_world_round_trip() {
        let v = CanvasView { pan: Vec2::new(-120.0, 75.5), zoom: 0.37 };
        for w in [Pos2::ZERO, Pos2::new(1000.0, -50.0), Pos2::new(-3.25, 8.0)] {
            assert!(close(v.to_world(O, v.to_screen(O, w)), w));
        }
    }

    #[test]
    fn zoom_keeps_the_anchor_fixed() {
        let mut v = CanvasView { pan: Vec2::new(10.0, -20.0), zoom: 1.0 };
        let anchor = Pos2::new(300.0, 200.0);
        let before = v.to_world(O, anchor);
        v.zoom_at(O, anchor, 1.7);
        assert!((v.zoom - 1.7).abs() < 1e-6);
        assert!(close(v.to_world(O, anchor), before));
    }

    #[test]
    fn zoom_is_clamped_and_ignores_bad_factors() {
        let mut v = CanvasView::default();
        v.zoom_at(O, O, 1e9);
        assert_eq!(v.zoom, MAX_ZOOM);
        v.zoom_at(O, O, 1e-9);
        assert_eq!(v.zoom, MIN_ZOOM);
        for bad in [0.0, -2.0, f32::NAN, f32::INFINITY] {
            let before = v;
            v.zoom_at(O, Pos2::new(5.0, 5.0), bad);
            assert_eq!(v, before, "{bad}");
        }
        v.pan_by(Vec2::new(f32::NAN, 1.0));
        assert!(v.pan.is_finite());
    }

    #[test]
    fn reset_centres_the_home_point_at_100_percent() {
        let mut v = CanvasView { pan: Vec2::new(999.0, 5.0), zoom: 3.0 };
        let viewport = Rect::from_min_size(O, Vec2::new(800.0, 600.0));
        v.reset(viewport, Pos2::new(100.0, 50.0));
        assert_eq!(v.zoom, 1.0);
        assert!(close(v.to_screen(O, Pos2::new(100.0, 50.0)), viewport.center()));
    }

    #[test]
    fn scroll_pans_or_zooms_by_preference() {
        let pan = CanvasSettings::default();
        assert_eq!(scroll_to_view_change(Vec2::new(3.0, -40.0), 1.0, &pan), (1.0, Vec2::new(3.0, -40.0)));

        let zoom = CanvasSettings { scroll: ScrollAction::Zoom, ..CanvasSettings::default() };
        let (f, p) = scroll_to_view_change(Vec2::new(3.0, SCROLL_POINTS_PER_DOUBLING), 1.0, &zoom);
        assert!((f - 2.0).abs() < 1e-5, "{f}");
        assert_eq!(p, Vec2::new(3.0, 0.0), "horizontal scroll still pans");
        let (f, _) = scroll_to_view_change(Vec2::new(0.0, -SCROLL_POINTS_PER_DOUBLING), 1.0, &zoom);
        assert!((f - 0.5).abs() < 1e-5, "{f}");
    }

    #[test]
    fn zoom_speed_scales_pinch_and_ignores_hostile_input() {
        let fast = CanvasSettings { zoom_speed: 2.0, ..CanvasSettings::default() };
        let (f, _) = scroll_to_view_change(Vec2::ZERO, 1.5, &fast);
        assert!((f - 2.25).abs() < 1e-5, "{f}");
        let zoom = CanvasSettings { scroll: ScrollAction::Zoom, zoom_speed: f32::NAN };
        for (scroll, pinch) in [(Vec2::new(0.0, f32::NAN), 1.0), (Vec2::ZERO, f32::NAN), (Vec2::ZERO, -1.0), (Vec2::ZERO, 0.0)] {
            let (f, _) = scroll_to_view_change(scroll, pinch, &zoom);
            assert_eq!(f, 1.0, "{scroll:?} {pinch}");
        }
    }

    #[test]
    fn grid_thins_out_when_zoomed_out() {
        assert_eq!(grid_step(20.0, 1.0), 20.0);
        assert_eq!(grid_step(20.0, 0.5), 40.0);
        assert_eq!(grid_step(20.0, 0.1), 160.0);
        assert_eq!(grid_step(20.0, 4.0), 20.0);
        for zoom in [MIN_ZOOM, 0.13, 0.33, 0.61, 1.0, MAX_ZOOM] {
            assert!(grid_step(20.0, zoom) * zoom >= MIN_DOT_SPACING, "{zoom}");
        }
        // Hostile inputs terminate.
        assert_eq!(grid_step(20.0, f32::NAN), 20.0);
        assert_eq!(grid_step(0.0, 1.0), 16.0);
    }

    #[test]
    fn dots_per_frame_are_bounded_on_a_large_screen() {
        let t = &crate::theme::DARK;
        // A 5K display at one pixel per point: the largest canvas in points we expect.
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(5120.0, 2880.0));
        for zoom in [MIN_ZOOM, 0.6, 1.0, MAX_ZOOM] {
            let step = grid_step(t.grid_spacing, zoom) * zoom;
            let dots = (viewport.width() / step + 2.0) * (viewport.height() / step + 2.0);
            assert!(dots < MAX_DOTS as f32, "zoom {zoom}: {dots}");
        }
    }
}

//! Floating panels (#957): one rule for how every panel over the canvas moves, resizes, clamps
//! and stacks. `docs/ui-design.md` ("Overlays and stacking") states the rule and lists each
//! overlay against it.
//!
//! Every floating panel is an `egui::Window` built by [`panel`]: moved by its header, resized by
//! its right and bottom edges and corner, kept inside the panel area (re-clamped every frame, so a smaller window
//! pulls it back in), drawn in [`PANEL_ORDER`] under menus, popups and notices, and brought to
//! the front when it opens or is pressed. Where panels sit, how big they are and which is in front
//! is view state held in egui's memory; none of it is a document change.
use egui::{Align2, Context, Id, Pos2, Rect, Vec2};

use crate::theme::Tokens;

/// The layer every floating panel draws in. Menus, popups, dialogs and notices use
/// `Order::Foreground` (or above), so they always sit over panels; the canvas is below.
pub const PANEL_ORDER: egui::Order = egui::Order::Middle;

/// Where a panel first opens, relative to the shell's [`Bounds::home`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Home {
    /// Its top-right corner at `home`: the panels the application row opens.
    RightTop,
    /// Its top edge centred in the panel area at `home`'s height: viewers and reviews.
    CenterTop,
}

/// Where floating panels may sit this frame, published by the shell once per frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    /// Panels stay inside this: the window below the application row.
    pub area: Rect,
    /// The top-right corner of a panel opened at [`Home::RightTop`]: below the row and search,
    /// left of an open discussion panel, so it never covers the control that opened it.
    pub home: Pos2,
}

fn bounds_id() -> Id {
    Id::new("nodecraft.floating.bounds")
}

fn shown_id() -> Id {
    Id::new("nodecraft.floating.shown")
}

/// Publishes this frame's panel area. Call before drawing any panel.
pub fn set_bounds(ctx: &Context, bounds: Bounds) {
    ctx.data_mut(|data| data.insert_temp(bounds_id(), bounds));
}

/// This frame's panel area; a host that never published one gets the whole window.
pub fn bounds(ctx: &Context, t: &Tokens) -> Bounds {
    ctx.data(|data| data.get_temp::<Bounds>(bounds_id())).unwrap_or_else(|| {
        let area = ctx.content_rect();
        Bounds { area, home: area.right_top() + Vec2::new(-t.navigation_margin, t.navigation_margin) }
    })
}

/// The layer a floating panel with this id draws in.
pub fn layer(id: Id) -> egui::LayerId {
    egui::LayerId::new(PANEL_ORDER, id)
}

/// The panels drawn recently, with the pass each was last drawn in.
type Shown = Vec<(Id, u64)>;

/// Records a fixed surface drawn in the panel layer this pass (the Shared parameters panel), so
/// a panel opening over it steps below its header like it does for any open panel.
pub fn occupy(ctx: &Context, id: Id) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|data| {
        let shown = data.get_temp_mut_or_default::<Shown>(shown_id());
        shown.retain(|(shown, at)| *shown != id && at.saturating_add(1) >= pass);
        shown.push((id, pass));
    });
}

fn header_id(id: Id) -> Id {
    id.with("nodecraft.floating.header")
}

fn header_height_id(id: Id) -> Id {
    id.with("nodecraft.floating.header_height")
}

fn moved_id(id: Id) -> Id {
    id.with("nodecraft.floating.moved")
}

/// A floating panel: an `egui::Window` whose position, layer, movement, resizing and clamping
/// come from this module. Callers add only `.open(..)`, sizes and content.
#[must_use = "a panel is drawn by `show`"]
pub struct Panel<'open> {
    window: egui::Window<'open>,
    id: Id,
}

impl<'open> Panel<'open> {
    /// Shows the header's close button and closes by clearing `open`.
    pub fn open(mut self, open: &'open mut bool) -> Self {
        self.window = self.window.open(open);
        self
    }

    pub fn default_width(mut self, width: f32) -> Self {
        self.window = self.window.default_width(width);
        self
    }

    pub fn default_height(mut self, height: f32) -> Self {
        self.window = self.window.default_height(height);
        self
    }

    pub fn show<R>(self, ctx: &Context, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> Option<egui::InnerResponse<Option<R>>> {
        let id = self.id;
        self.window.show(ctx, |ui| {
            header_drag(ui, id);
            add_contents(ui)
        })
    }
}

/// The header's move handle: the title strip above the content, left of the close button.
/// Only this senses a drag, so pressing or dragging inside the body never moves the panel.
fn header_drag(ui: &mut egui::Ui, id: Id) {
    let Some(area) = ui.ctx().memory(|m| m.area_rect(id)) else { return };
    let top = ui.max_rect().top();
    let height = top - area.top();
    if !(height.is_finite() && height > 0.0 && area.is_finite()) {
        return;
    }
    // Measured for the cascade: everything above the content is title and close button.
    ui.ctx().data_mut(|data| data.insert_temp(header_height_id(id), height));
    // The close button sits at the right end of the title row, within one header height.
    let rect = Rect::from_min_max(area.min, Pos2::new((area.right() - height).max(area.left()), top));
    let response = ui.interact(rect, header_id(id), egui::Sense::drag()).on_hover_cursor(egui::CursorIcon::Grab);
    let ctx = ui.ctx().clone();
    let held = ctx.data(|data| data.get_temp::<Drag>(moved_id(id)));
    let pointer = ctx.input(|i| i.pointer.interact_pos()).filter(|pos| pos.is_finite());
    let drag = match (held, pointer) {
        // The release pass still moves it to where the pointer let go.
        (Some(drag), Some(pointer)) if response.dragged() || response.drag_stopped() => Some(Drag { to: drag.start + (pointer - drag.grab), ..drag }),
        (None, Some(pointer)) if response.dragged() => {
            // Pivot at the drag's start plus the drag so far: a pass egui repeats lands in the same place.
            let start = egui::AreaState::load(&ctx, id).and_then(|state| state.pivot_pos).filter(|pos| pos.is_finite());
            let grab = pointer - response.total_drag_delta().filter(|delta| delta.is_finite()).unwrap_or_default();
            start.map(|start| Drag { start, grab, to: start + (pointer - grab) })
        }
        _ => None,
    };
    let Some(drag) = drag else {
        ctx.data_mut(|data| data.remove::<Drag>(moved_id(id)));
        return;
    };
    ctx.data_mut(|data| data.insert_temp(moved_id(id), drag));
    if held.map(|held| held.to) != Some(drag.to) {
        // The move is applied before the next pass lays the window out.
        ctx.request_repaint();
    }
    if response.dragged() {
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    }
}

/// A header drag in progress: the pivot and pointer where it started, and where the pivot is now.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drag {
    start: Pos2,
    grab: Pos2,
    to: Pos2,
}

/// A floating panel's window. Every panel behaves the same: see the module documentation.
pub fn panel<'open>(ctx: &Context, title: impl Into<egui::WidgetText>, id: Id, home: Home, t: &Tokens) -> Panel<'open> {
    let bounds = bounds(ctx, t);
    let pass = ctx.cumulative_pass_nr();
    let mut shown: Shown = ctx.data(|data| data.get_temp::<Shown>(shown_id())).unwrap_or_default();
    // A panel drawn in this or the previous pass is open; anything older has closed.
    shown.retain(|(_, at)| at.saturating_add(1) >= pass);
    let opening = !shown.iter().any(|(shown, _)| *shown == id);
    let others: Vec<Rect> = shown
        .iter()
        .filter(|(other, _)| *other != id)
        .filter_map(|(other, _)| egui::AreaState::load(ctx, *other).map(|state| state.rect()))
        .filter(|rect| rect.is_finite() && rect.is_positive())
        .collect();
    // Step past the tallest measured header among the open panels; the token is the floor.
    let step = shown
        .iter()
        .filter(|(other, _)| *other != id)
        .filter_map(|(other, _)| ctx.data(|data| data.get_temp::<f32>(header_height_id(*other))))
        .filter(|height| height.is_finite())
        .fold(t.panel_cascade, f32::max);
    shown.retain(|(shown, _)| *shown != id);
    shown.push((id, pass));
    ctx.data_mut(|data| data.insert_temp(shown_id(), shown));
    // A header drag in progress, applied before the window lays out (and is clamped).
    let moved = ctx.data(|data| data.get_temp::<Drag>(moved_id(id))).map(|drag| drag.to).filter(|to| to.is_finite());

    let frame = egui::Frame::NONE
        .inner_margin(t.card_padding)
        .fill(t.panel_fill)
        .stroke(egui::Stroke::new(t.handle_stroke, t.panel_stroke))
        .corner_radius(t.panel_radius);
    // egui's own window move is off: it would also claim drags that start in the body. The pivot
    // is the top-left corner, so a non-movable window resizes from its right and bottom edges.
    let window = egui::Window::new(title)
        .id(id)
        .order(PANEL_ORDER)
        .collapsible(false)
        .movable(false)
        .resizable(true)
        .constrain_to(bounds.area)
        .pivot(Align2::LEFT_TOP)
        .frame(frame);
    let remembered = egui::AreaState::load(ctx, id);
    let size = remembered.and_then(|state| state.size).filter(|size| size.is_finite() && size.x > 0.0 && size.y > 0.0);
    let placing = ctx.data(|data| data.get_temp::<Placing>(placing_id(id)));
    let start = match (opening, placing, size) {
        // Its first pass has no size yet: egui measures it unseen, and the next pass places it.
        (true, _, None) => {
            let from = remembered.and_then(|state| state.pivot_pos).filter(|pos| pos.is_finite());
            ctx.data_mut(|data| data.insert_temp(placing_id(id), Placing(from)));
            let estimate = Vec2::new(t.search_width, t.panel_cascade);
            return Panel { window: window.default_pos(from.unwrap_or_else(|| home_corner(home, &bounds, estimate))), id };
        }
        (true, _, Some(_)) => remembered.map(|state| state.rect().min),
        (false, Some(Placing(from)), Some(size)) => Some(from.unwrap_or_else(|| home_corner(home, &bounds, size))),
        (false, _, _) => None,
    };
    let pos = match (start, size) {
        (Some(start), Some(size)) => {
            ctx.data_mut(|data| data.remove::<Placing>(placing_id(id)));
            // Step down past any open panel's header it would cover, so every open panel keeps
            // its title and close button in view.
            Some(cascade(start, Align2::LEFT_TOP, size, &others, bounds.area, step))
        }
        _ => moved,
    };
    let window = match pos {
        Some(pos) => window.current_pos(pos),
        None => window,
    };
    Panel { window, id }
}

/// A panel's top-left corner at its home, for a panel of `size`.
fn home_corner(home: Home, bounds: &Bounds, size: Vec2) -> Pos2 {
    match home {
        Home::RightTop => Pos2::new(bounds.home.x - size.x, bounds.home.y),
        Home::CenterTop => Pos2::new(bounds.area.center().x - size.x / 2.0, bounds.home.y),
    }
}

/// A panel that opened without a size, waiting for egui's measuring pass: where it was left, or
/// `None` to open at home.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placing(Option<Pos2>);

fn placing_id(id: Id) -> Id {
    id.with("nodecraft.floating.placing")
}

/// Where a panel of `size` pivoted at `start` opens so it covers no header in `others`.
/// Each step moves it below the header it covers; bounded by the number of panels.
pub fn cascade(start: Pos2, pivot: Align2, size: Vec2, others: &[Rect], area: Rect, step: f32) -> Pos2 {
    if !(step.is_finite() && step > 0.0) {
        return start;
    }
    let mut pos = start;
    for _ in 0..=others.len() {
        let rect = pivot.anchor_size(pos, size);
        let covered = others.iter().filter(|other| rect.intersect(header(**other, step)).is_positive()).map(|other| other.top() + step).reduce(f32::max);
        let Some(below) = covered else { return pos };
        let next = Pos2::new(pos.x, pos.y + (below - rect.top()).max(step));
        // Out of room below: stop rather than push it off the area; clamping keeps it inside.
        if pivot.anchor_size(next, size).top() + step > area.bottom() {
            return pos;
        }
        pos = next;
    }
    pos
}

/// The strip holding a panel's title and close button.
fn header(rect: Rect, step: f32) -> Rect {
    Rect::from_min_max(rect.min, Pos2::new(rect.max.x, (rect.min.y + step).min(rect.max.y)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panel_opening_over_another_steps_below_its_header() {
        let area = Rect::from_min_max(Pos2::new(0.0, 50.0), Pos2::new(1000.0, 800.0));
        let open = Rect::from_min_size(Pos2::new(600.0, 60.0), Vec2::new(380.0, 300.0));
        let pos = cascade(Pos2::new(980.0, 60.0), Align2::RIGHT_TOP, Vec2::new(300.0, 200.0), &[open], area, 40.0);
        assert_eq!(pos, Pos2::new(980.0, 100.0));
    }

    #[test]
    fn panels_that_do_not_touch_keep_their_place() {
        let area = Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 800.0));
        let open = Rect::from_min_size(Pos2::new(0.0, 60.0), Vec2::new(200.0, 300.0));
        let start = Pos2::new(980.0, 60.0);
        assert_eq!(cascade(start, Align2::RIGHT_TOP, Vec2::new(300.0, 200.0), &[open], area, 40.0), start);
    }

    #[test]
    fn three_panels_cascade_in_order_and_hostile_steps_are_ignored() {
        let area = Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 800.0));
        let a = Rect::from_min_size(Pos2::new(600.0, 60.0), Vec2::new(380.0, 300.0));
        let b = Rect::from_min_size(Pos2::new(600.0, 100.0), Vec2::new(380.0, 300.0));
        let start = Pos2::new(980.0, 60.0);
        assert_eq!(cascade(start, Align2::RIGHT_TOP, Vec2::new(380.0, 300.0), &[a, b], area, 40.0), Pos2::new(980.0, 140.0));
        for step in [f32::NAN, f32::INFINITY, 0.0, -4.0] {
            assert_eq!(cascade(start, Align2::RIGHT_TOP, Vec2::new(380.0, 300.0), &[a, b], area, step), start);
        }
    }

    #[test]
    fn a_full_area_stops_the_cascade_instead_of_pushing_it_out() {
        let area = Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 120.0));
        let a = Rect::from_min_size(Pos2::new(600.0, 60.0), Vec2::new(380.0, 50.0));
        let start = Pos2::new(980.0, 60.0);
        assert_eq!(cascade(start, Align2::RIGHT_TOP, Vec2::new(380.0, 100.0), &[a], area, 40.0), start);
    }
}

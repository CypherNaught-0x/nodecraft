//! Floating panels (#957): one rule for how every panel over the canvas moves, resizes, clamps
//! and stacks. `docs/ui-design.md` ("Overlays and stacking") states the rule and lists each
//! overlay against it.
//!
//! Every floating panel is an `egui::Window` built by [`panel`]: moved by its header, resized by
//! its right and bottom edges and corner, kept inside the panel area (re-clamped every frame, so a smaller window
//! pulls it back in), drawn in [`PANEL_ORDER`] under menus, popups and notices, and brought to
//! the front when it opens or is pressed. Where panels sit, how big they are and which is in front
//! is view state held in egui's memory; none of it is a document change.
//!
//! The rule for dialogs (#1116): a *question* the person answers before continuing is a
//! [`question`] (an `egui::Modal`: Enter takes the primary action, Escape cancels); a *tool* the
//! person works alongside is a [`panel`], or [`pinned`] for the fixed Shared parameters panel.
//! This module is the only place that builds an `egui::Window`.
use egui::{Align2, Context, Id, Pos2, Rect, Vec2};

use crate::theme::Tokens;

/// The layer every floating panel draws in. Menus, popups, dialogs and notices use
/// `Order::Foreground` (or above), so they always sit over panels; the canvas is below.
pub const PANEL_ORDER: egui::Order = egui::Order::Middle;

/// Where a panel first opens, relative to the shell's [`Bounds::home`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Home {
    /// Its top-right corner at `home`: the panels the application row opens.
    RightTop,
    /// Its top edge centred in the panel area at `home`'s height: viewers and reviews.
    CenterTop,
    /// Its top-left corner `navigation_margin` in from the panel area's left edge, no higher
    /// than this: the panel the File button at the window's top left opens (Project files), just
    /// below the button.
    LeftBelow(f32),
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

/// The panels drawn in this or the previous pass, with their rectangles (#959 tests).
#[cfg(test)]
pub fn shown(ctx: &Context) -> Vec<(Id, egui::Rect)> {
    let pass = ctx.cumulative_pass_nr();
    let shown: Shown = ctx.data(|data| data.get_temp::<Shown>(shown_id())).unwrap_or_default();
    shown
        .into_iter()
        .filter(|(_, at)| at.saturating_add(1) >= pass)
        .filter_map(|(id, _)| egui::AreaState::load(ctx, id).map(|state| (id, state.rect())))
        .collect()
}

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

/// The height a panel's body can take without the panel leaving the floating panel area: the
/// area's height less what sits above the body (the measured gap from the panel's top edge to its
/// content: stroke, title and top padding; the cascade token until measured) and below it (the
/// bottom padding and stroke). Never negative.
pub fn body_room(ctx: &Context, id: Id, t: &Tokens) -> f32 {
    let above = ctx.data(|data| data.get_temp::<f32>(header_height_id(id))).filter(|height| height.is_finite()).unwrap_or(t.panel_cascade);
    (bounds(ctx, t).area.height() - above - t.card_padding - t.handle_stroke).max(0.0)
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

    /// The tallest the panel may be, so it can open below the control that opened it.
    pub fn max_height(mut self, height: f32) -> Self {
        if height.is_finite() && height > 0.0 {
            self.window = self.window.max_height(height);
        }
        self
    }

    /// Scrolls the body when it is taller than the room the panel has.
    pub fn vscroll(mut self, vscroll: bool) -> Self {
        self.window = self.window.vscroll(vscroll);
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

/// A *question* (#1116): something the person answers before continuing, such as a confirmation
/// or a rename. It is an `egui::Modal` centred over a dimmed window, in the shared panel frame
/// and no wider than the window less its margins. Its caller reads [`answer_keys`] before
/// showing it, so Enter takes the primary action and Escape cancels, and neither reaches the
/// canvas. A *tool* the person works alongside is a [`panel`]; nothing in the shell builds a raw
/// `egui::Window` outside this module (`no_raw_windows_outside_floating` checks).
///
/// `title` names it for screen readers: its area is an AccessKit `Dialog` labelled `title`, with
/// the question's controls inside it (callers still show `title` as a heading).
pub fn question(ctx: &Context, id: Id, title: &str, t: &Tokens) -> egui::Modal {
    // The area's own node (`<id>.with("move")` in egui 0.36), made before its content so the
    // content is placed inside it rather than at the root of the tree.
    // Its bounds are where it was last drawn: it stays centred, so they only lag on its first pass.
    let last = egui::AreaState::load(ctx, id).map(|state| state.rect()).filter(|rect| rect.is_finite());
    ctx.accesskit_node_builder(id.with("move"), |node| {
        node.set_role(egui::accesskit::Role::Dialog);
        node.set_label(title);
        node.set_modal();
        if let Some(rect) = last {
            node.set_bounds(egui::accesskit::Rect { x0: rect.min.x.into(), y0: rect.min.y.into(), x1: rect.max.x.into(), y1: rect.max.y.into() });
        }
    });
    let screen = ctx.content_rect();
    let room = Vec2::new(screen.width() - t.navigation_margin * 2.0, screen.height() - t.navigation_margin * 2.0).max(Vec2::splat(t.navigation_margin));
    let area = egui::Modal::default_area(id).constrain_to(screen).default_width(t.dialog_width.min(room.x));
    egui::Modal::new(id).area(area).frame(crate::theme::panel_frame(t))
}

/// The widest a [`question`]'s content may be: `dialog_width`, or less in a narrow window.
pub fn question_width(ctx: &Context, t: &Tokens) -> f32 {
    let room = ctx.content_rect().width() - t.navigation_margin * 2.0 - t.card_padding * 2.0;
    t.dialog_width.min(room).max(t.navigation_margin)
}

/// The tallest a [`question`]'s content may be, so its buttons stay on the screen.
pub fn question_height(ctx: &Context, t: &Tokens) -> f32 {
    let room = ctx.content_rect().height() - t.navigation_margin * 2.0 - t.card_padding * 2.0;
    room.max(t.navigation_margin)
}

/// A question's keys this frame, consumed so nothing else acts on them: a fresh press of Enter
/// (the primary action) and of Escape (cancel), each without modifiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub enter: bool,
    pub escape: bool,
}

/// Which keys a question answers, for [`answer_keys`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keys {
    /// Enter takes the primary action and Escape cancels.
    EnterAndEscape,
    /// Escape cancels; Enter is left alone (a question whose only action loses work).
    EscapeOnly,
}

/// Whether Enter may answer a question yet, and the pass it was last asked in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EnterArmed {
    pass: u64,
    armed: bool,
}

/// Takes this frame's Enter and Escape for the open question `id`; see [`question`]. Call it
/// before showing the question, every pass it is shown.
///
/// Only a fresh press counts: a key repeat never answers, and Enter answers only once it has been
/// up since the question opened, so the Enter held to choose something in the step before
/// can't also answer this. While one of the question's own controls other than `field` (its text
/// field, if it has one) has the keyboard focus, Enter is left to that control: a focused Cancel
/// cancels. Repeats of a key the question takes are consumed too, so none reaches the canvas;
/// with [`Keys::EscapeOnly`], Enter is consumed without answering.
pub fn answer_keys(ctx: &Context, id: Id, field: Option<Id>, keys: Keys) -> Answer {
    let pass = ctx.cumulative_pass_nr();
    let state_id = id.with("nodecraft.question.enter");
    let held = ctx.input(|i| i.key_down(egui::Key::Enter));
    let last = ctx.data(|data| data.get_temp::<EnterArmed>(state_id)).filter(|last| last.pass.saturating_add(1) >= pass);
    // Asked in the previous pass and armed then: a press now answers.
    let ready = last.is_some_and(|last| last.armed);
    if last.is_none() {
        // Just opened: the next pass arms Enter, so a press soon after isn't lost while idle.
        ctx.request_repaint();
    }
    ctx.data_mut(|data| data.insert_temp(state_id, EnterArmed { pass, armed: last.is_some() && (ready || !held) }));
    let layer = egui::LayerId::new(egui::Order::Foreground, id);
    let own_control = ctx
        .memory(|m| m.focused())
        .filter(|focused| Some(*focused) != field)
        .and_then(|focused| ctx.read_response(focused))
        .is_some_and(|response| response.layer_id == layer);
    // A question that answers only Escape still takes Enter (doing nothing), so no panel drawn
    // after it gets a keyboard click behind the modal.
    let take_enter = !own_control;
    ctx.input_mut(|i| {
        let mut answer = Answer::default();
        i.events.retain(|event| match event {
            egui::Event::Key { key: egui::Key::Escape, pressed: true, repeat, modifiers, .. } if modifiers.is_none() => {
                answer.escape |= !repeat;
                false
            }
            egui::Event::Key { key: egui::Key::Enter, pressed: true, repeat, modifiers, .. } if take_enter && modifiers.is_none() => {
                answer.enter |= !repeat && ready && keys == Keys::EnterAndEscape;
                false
            }
            _ => true,
        });
        answer
    })
}

/// A fixed tool pinned to the panel area's right edge, `down` points below its top (the Shared
/// parameters and Row settings panels): it neither moves nor resizes, draws in [`PANEL_ORDER`],
/// is no wider than the window less its margins, and panels opening over it step below its
/// header ([`occupy`]). It is dismissed by changing the selection, not by a close button.
pub fn pinned<'open>(ctx: &Context, title: impl Into<egui::WidgetText>, id: Id, down: f32, t: &Tokens) -> egui::Window<'open> {
    occupy(ctx, id);
    let screen = ctx.content_rect();
    let room = (screen.width() - t.navigation_margin * 2.0).max(t.navigation_margin);
    let frame =
        egui::Frame::window(&ctx.global_style()).fill(t.panel_fill).stroke(egui::Stroke::new(t.handle_stroke, t.panel_stroke)).corner_radius(t.panel_radius);
    egui::Window::new(title)
        .id(id)
        .order(PANEL_ORDER)
        .constrain_to(screen)
        .max_width(room)
        .default_width(t.card_width.min(room))
        .anchor(Align2::RIGHT_TOP, [-t.navigation_margin, down])
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .frame(frame)
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
            return Panel { window: window.default_pos(from.unwrap_or_else(|| home_corner(home, &bounds, estimate, t.navigation_margin))), id };
        }
        (true, _, Some(_)) => remembered.map(|state| state.rect().min),
        (false, Some(Placing(from)), Some(size)) => Some(from.unwrap_or_else(|| home_corner(home, &bounds, size, t.navigation_margin))),
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
fn home_corner(home: Home, bounds: &Bounds, size: Vec2, margin: f32) -> Pos2 {
    match home {
        Home::RightTop => Pos2::new(bounds.home.x - size.x, bounds.home.y),
        Home::CenterTop => Pos2::new(bounds.area.center().x - size.x / 2.0, bounds.home.y),
        Home::LeftBelow(top) => Pos2::new(bounds.area.left() + margin, if top.is_finite() { top.max(bounds.home.y) } else { bounds.home.y }),
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

    /// The 1-based lines of `text` that build or name an `egui::Window`, outside its
    /// `#[cfg(test)]` modules (skipped by brace depth, so code after them is still read):
    /// `Window::new` called or passed (`.map(egui::Window::new)`), any `egui::Window` path, and
    /// `Window` brought in by a `use egui…` (an alias included).
    /// `text` with its comments and string and char literals removed, newlines kept, so braces
    /// and names inside them are never counted.
    fn code_only(text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        let at = |i: usize| chars.get(i).copied();
        while let Some(c) = at(i) {
            let ident_before = i > 0 && at(i - 1).is_some_and(|p| p.is_alphanumeric() || p == '_');
            if c == '/' && at(i + 1) == Some('/') {
                while at(i).is_some_and(|c| c != '\n') {
                    i += 1;
                }
            } else if c == '/' && at(i + 1) == Some('*') {
                let mut depth = 0;
                while let Some(c) = at(i) {
                    if c == '/' && at(i + 1) == Some('*') {
                        depth += 1;
                        i += 2;
                    } else if c == '*' && at(i + 1) == Some('/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        if c == '\n' {
                            out.push('\n');
                        }
                        i += 1;
                    }
                }
            } else if c == 'r' && !ident_before && matches!(at(i + 1), Some('"' | '#')) {
                let mut hashes = 0;
                let mut j = i + 1;
                while at(j) == Some('#') {
                    hashes += 1;
                    j += 1;
                }
                if at(j) != Some('"') {
                    out.push(c);
                    i += 1;
                    continue;
                }
                i = j + 1;
                while let Some(c) = at(i) {
                    if c == '"' && (1..=hashes).all(|k| at(i + k) == Some('#')) {
                        i += 1 + hashes;
                        break;
                    }
                    if c == '\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
            } else if c == '"' {
                i += 1;
                while let Some(c) = at(i) {
                    if c == '\\' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    if c == '"' {
                        break;
                    }
                    if c == '\n' {
                        out.push('\n');
                    }
                }
            } else if c == '\'' && at(i + 1) == Some('\\') {
                // An escaped char literal such as '\n' or '\u{7b}'.
                i += 2;
                while at(i).is_some_and(|c| c != '\'' && c != '\n') {
                    i += 1;
                }
                i += 1;
            } else if c == '\'' && at(i + 2) == Some('\'') {
                i += 3;
            } else {
                out.push(c);
                i += 1;
            }
        }
        out
    }

    /// The 1-based lines of `text` that build or name an `egui::Window`, outside its
    /// `#[cfg(test)]` modules (skipped by brace depth, so code after them is still read):
    /// `Window::new` called or passed (`.map(egui::Window::new)`), any `egui::Window` path, and
    /// `Window` brought in by a `use` of egui (`use eframe::egui::…` and aliases included).
    /// Comments and literals are ignored.
    fn raw_window_lines(text: &str) -> Vec<usize> {
        let mut found = Vec::new();
        let mut test_attr = false;
        let mut skip_depth: Option<i64> = None;
        let mut in_use = false;
        for (n, line) in code_only(text).lines().enumerate() {
            let code = line.trim();
            if let Some(depth) = &mut skip_depth {
                *depth += code.matches('{').count() as i64 - code.matches('}').count() as i64;
                if *depth <= 0 {
                    skip_depth = None;
                }
                continue;
            }
            if code == "#[cfg(test)]" {
                test_attr = true;
                continue;
            }
            if test_attr && code.contains("mod ") && code.ends_with('{') {
                test_attr = false;
                skip_depth = Some(1);
                continue;
            }
            if !code.is_empty() {
                test_attr = false;
            }
            let statement = code.trim_start_matches("pub(crate) ").trim_start_matches("pub(super) ").trim_start_matches("pub ");
            if statement.starts_with("use ") && statement.contains("egui") {
                in_use = true;
            }
            let word = code.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| w == "Window");
            if code.contains("Window::new") || code.contains("egui::Window") || (in_use && word) {
                found.push(n + 1);
            }
            if in_use && code.contains(';') {
                in_use = false;
            }
        }
        found
    }

    #[test]
    fn the_raw_window_scan_finds_every_spelling_and_skips_only_test_modules() {
        let text = "use egui::{Area, Window as Win};\nfn a() { let _ = names.map(egui::Window::new); }\nfn b() -> web_sys::Window { todo }\n#[cfg(test)]\nmod tests {\n    fn t() { egui::Window::new(\"x\"); }\n}\nfn c() { Win::new(\"y\"); egui::Window::new(\"z\"); }\nuse egui::{\n    Pos2,\n    Window,\n};\n";
        assert_eq!(raw_window_lines(text), vec![1, 2, 8, 11]);
        // Braces and names in literals and comments don't count; a module after a test module
        // with a brace in a string is still read; `use eframe::egui` starts an import too.
        let text = "#[cfg(test)]\nmod tests {\n    const OPEN: char = '{';\n    const S: &str = \"}}}\";\n    // }\n}\nfn d() { egui::Window::new(\"w\"); }\nuse eframe::egui::{\n    Window,\n};\nconst T: &str = \"egui::Window::new\"; // egui::Window::new\nconst R: &str = r#\"Window::new\"#;\nfn e<'a>(x: &'a str) {}\n";
        assert_eq!(raw_window_lines(text), vec![7, 9]);
    }

    /// #1116: a question is a `question` (a modal) and a tool a `panel` (or `pinned`); nothing
    /// outside this module builds a raw `egui::Window`, which would skip the panel area's
    /// clamping, cascade and stacking. Test code may still build fixtures.
    #[test]
    fn no_raw_windows_outside_floating() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![root];
        let mut found = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                if !name.ends_with(".rs")
                    || name == "floating.rs"
                    || name == "tests.rs"
                    || name.ends_with("_tests.rs")
                    || path.components().any(|c| c.as_os_str() == "tests")
                {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                found.extend(raw_window_lines(&text).into_iter().map(|n| format!("{}:{n}", path.display())));
            }
        }
        assert!(found.is_empty(), "use floating::question or floating::panel, not egui::Window: {found:#?}");
    }

    #[test]
    fn a_question_fits_a_small_window() {
        let ctx = Context::default();
        let t = crate::theme::DARK;
        let mut sizes = Vec::new();
        for size in [Vec2::new(640.0, 400.0), Vec2::new(300.0, 200.0), Vec2::new(1600.0, 900.0)] {
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), ..Default::default() };
            let mut output = ctx.run_ui(input, |ui| sizes.push((question_width(ui.ctx(), &t), question_height(ui.ctx(), &t))));
            output.textures_delta.clear();
            let (width, height) = sizes.last().copied().unwrap();
            assert!(width <= t.dialog_width && width <= size.x, "{size:?}: {width}");
            assert!(height <= size.y && height > 0.0, "{size:?}: {height}");
        }
        assert_eq!(sizes.last().map(|s| s.0), Some(t.dialog_width), "a large window keeps the full width");
    }

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

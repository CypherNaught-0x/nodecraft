//! Scroll chaining between the canvas and the scrollable regions on its cards (#413).
//!
//! The rule: a two-finger scroll or wheel delta goes to the innermost scrollable region under the
//! pointer first. A region takes only what it can use before it reaches its limit in that
//! direction; whatever is left moves on to the region around it and finally to the canvas, which
//! pans (or, with Scroll = Zoom, zooms) by the remainder. Being a card is not a reason to swallow
//! the gesture; only "consumed, and not at a limit" is.
//!
//! On top of that, a gesture belongs to whoever took its first delta. A swipe that started on the
//! canvas keeps panning while the pointer crosses a card mid-swipe; one that started in a card's
//! body keeps chaining from there. The gesture ends when egui stops reporting it as scrolling (the
//! trackpad's end phase, or a short pause after wheel notches).
//!
//! Regions on cards report what they consumed by writing the remainder back into
//! [`egui::InputState::smooth_scroll_delta`], as egui's own scroll areas do. egui only scrolls an
//! area on the layer it finds under the pointer, which a card's transformed sub-layer never is, so
//! cards call [`take`] after showing each of their scroll areas.

use egui::Vec2;
use egui::scroll_area::ScrollAreaOutput;

/// The new offset on one axis after scrolling by `delta`, and the part of `delta` left over;
/// `None` when the region is already at its limit in that direction (or the input is unusable).
fn axis(offset: f32, delta: f32, max: f32) -> Option<(f32, f32)> {
    if !offset.is_finite() || !delta.is_finite() || !max.is_finite() || delta == 0.0 {
        return None;
    }
    let next = (offset - delta).clamp(0.0, max.max(0.0));
    let used = offset - next;
    // An offset left past a limit that shrank snaps back further than `delta`: nothing is left
    // over then, and a move against the scroll direction is not taking it.
    let rest = if used.abs() >= delta.abs() { 0.0 } else { delta - used };
    (used != 0.0 && used.signum() == delta.signum()).then_some((next, rest))
}

/// Let one scroll area on a card take the part of this frame's scroll it can use, leaving the
/// remainder for the regions around it and the canvas. `hovered` is whether the pointer is over
/// the area (from the card layer's hit test); `axes` are the directions the area scrolls in.
/// Returns whether it took anything.
///
/// The delta is in screen points but a card's layer is scaled by the canvas zoom, so the area
/// scrolls by the delta in its own units and hands back the remainder in screen points: the
/// content under the finger moves exactly as far as the finger, at any zoom.
pub fn take<R>(ui: &egui::Ui, hovered: bool, out: &ScrollAreaOutput<R>, axes: [bool; 2]) -> bool {
    if !hovered || !ui.is_enabled() || ui.ctx().dragged_id().is_some() {
        return false;
    }
    let scale = ui.ctx().layer_transform_to_global(ui.layer_id()).map_or(1.0, |to_global| to_global.scaling);
    if !scale.is_finite() || scale <= 0.0 {
        return false;
    }
    let delta = ui.input(|i| i.smooth_scroll_delta);
    let local = delta / scale;
    let max = out.content_size - out.inner_rect.size();
    let mut state = out.state;
    let mut left = delta;
    let mut took = false;
    if axes[0]
        && let Some((next, rest)) = axis(state.offset.x, local.x, max.x)
    {
        state.offset.x = next;
        left.x = rest * scale;
        took = true;
    }
    if axes[1]
        && let Some((next, rest)) = axis(state.offset.y, local.y, max.y)
    {
        state.offset.y = next;
        left.y = rest * scale;
        took = true;
    }
    if took {
        state.store(ui.ctx(), out.id);
        ui.input_mut(|i| i.smooth_scroll_delta = left);
        ui.ctx().request_repaint();
    }
    took
}

/// Who a scroll gesture belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// It started where nothing on a card could scroll: every delta pans the canvas.
    Canvas,
    /// A region on a card took its first delta: regions scroll first, the canvas gets the rest.
    Card,
}

/// Tracks the scroll gesture in progress across frames, around the pass that draws the cards.
#[derive(Debug, Default)]
pub struct Gesture {
    owner: Option<Owner>,
    /// This frame's delta before the card pass, while [`Self::begin`] has armed it.
    before: Option<Vec2>,
}

impl Gesture {
    /// Call before drawing what is on the canvas. `live` is false while something else owns
    /// canvas input this frame; the delta is then left alone and nothing reaches the canvas.
    pub fn begin(&mut self, ctx: &egui::Context, live: bool) {
        let (scrolling, started, delta) = ctx.input(|i| {
            let started = i.events.iter().any(|event| matches!(event, egui::Event::MouseWheel { phase: egui::TouchPhase::Start, .. }));
            (i.is_scrolling(), started, i.smooth_scroll_delta)
        });
        // egui goes straight from smoothing (or momentum) to a new touch on the trackpad's start
        // phase without a frame that isn't scrolling, so a start is a new gesture too.
        if !scrolling || started {
            self.owner = None;
        }
        if !live || !delta.is_finite() {
            self.before = None;
            return;
        }
        self.before = Some(delta);
        if self.owner == Some(Owner::Canvas) && delta != Vec2::ZERO {
            // The canvas's swipe: hide it from the regions the pointer crosses.
            ctx.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
        }
    }

    /// Call after drawing what is on the canvas. Returns the scroll the canvas should apply this
    /// frame, and restores a delta hidden by [`Self::begin`] for anything drawn later.
    pub fn finish(&mut self, ctx: &egui::Context) -> Vec2 {
        let Some(before) = self.before.take() else {
            return Vec2::ZERO;
        };
        let after = ctx.input(|i| i.smooth_scroll_delta);
        match self.owner {
            Some(Owner::Canvas) => {
                if before != Vec2::ZERO {
                    ctx.input_mut(|i| i.smooth_scroll_delta = before);
                }
                before
            }
            Some(Owner::Card) => after,
            None => {
                if before != Vec2::ZERO {
                    self.owner = Some(if after == before { Owner::Canvas } else { Owner::Card });
                }
                after
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_region_takes_what_it_can_and_passes_on_the_rest() {
        // Room to scroll down (negative delta moves the content up).
        assert_eq!(axis(0.0, -40.0, 100.0), Some((40.0, 0.0)));
        // Ten points from the bottom: takes ten, passes thirty on.
        assert_eq!(axis(90.0, -40.0, 100.0), Some((100.0, -30.0)));
        // At the limit in that direction: nothing to take.
        assert_eq!(axis(100.0, -40.0, 100.0), None);
        assert_eq!(axis(0.0, 40.0, 100.0), None);
        // Nothing to scroll at all.
        assert_eq!(axis(0.0, 40.0, -20.0), None);
        assert_eq!(axis(0.0, -40.0, 0.0), None);
    }

    #[test]
    fn hostile_numbers_take_nothing() {
        for (offset, delta, max) in [(f32::NAN, 1.0, 10.0), (0.0, f32::NAN, 10.0), (0.0, -1.0, f32::INFINITY), (0.0, f32::NEG_INFINITY, 10.0), (0.0, 0.0, 10.0)]
        {
            assert_eq!(axis(offset, delta, max), None, "{offset} {delta} {max}");
        }
    }

    #[test]
    fn an_offset_past_a_shrunk_limit_scrolls_back_inside_it() {
        // Content shrank under the offset: scrolling up still moves toward it, never further out.
        assert_eq!(axis(150.0, 40.0, 100.0), Some((100.0, 0.0)));
        assert_eq!(axis(150.0, -40.0, 100.0), None);
    }
}

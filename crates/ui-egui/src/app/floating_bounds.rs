//! The shell's panel area for floating panels (#957): below the application row, home under
//! search and left of an open discussion panel. View state only; see `crate::floating`.
use super::*;

impl NodeCraftApp {
    /// Where floating panels may sit this frame, from the top bar already drawn.
    pub(super) fn floating_bounds(&self) -> crate::floating::Bounds {
        let t = self.theme.tokens();
        let row = self.topbar_rect().bottom();
        let top = (row + t.navigation_gap).min(self.viewport.bottom());
        let area = Rect::from_min_max(Pos2::new(self.viewport.left(), top), self.viewport.max);
        let below = self.search_rect().bottom().max(row) + t.navigation_gap;
        // An open discussion panel is pinned chrome above panels: open beside it, not under it.
        let right =
            if self.comments.open && !self.on_sheet() { self.discussion_rect().left() - t.navigation_gap } else { self.viewport.right() - t.navigation_margin };
        let home = Pos2::new(right.max(area.left()), below.max(area.top()).min(area.bottom()));
        crate::floating::Bounds { area, home }
    }

    /// Publishes [`Self::floating_bounds`]; call once a frame before drawing any panel.
    pub(super) fn publish_floating_bounds(&self, ctx: &egui::Context) {
        crate::floating::set_bounds(ctx, self.floating_bounds());
    }
}

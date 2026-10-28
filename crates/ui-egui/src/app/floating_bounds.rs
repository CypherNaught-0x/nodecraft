//! The shell's panel area for floating panels (#957): below the application row and clear of
//! open Search and Discussions. View state only; see `crate::floating`.
use super::*;

impl NodeCraftApp {
    /// Where floating panels may sit this frame, from the top bar already drawn. Open Search
    /// and Discussions are pinned chrome above panels, so the area leaves them clear and
    /// clamping moves a panel out from under them rather than hiding its header.
    pub(super) fn floating_bounds(&self) -> crate::floating::Bounds {
        let t = self.theme.tokens();
        let mut top = self.topbar_rect().bottom() + t.navigation_gap;
        let mut right = self.viewport.right();
        let mut home = self.viewport.right() - t.navigation_margin;
        if !self.on_sheet() {
            if self.search.open {
                top = top.max(self.search_rect().bottom() + t.navigation_gap);
            }
            if self.comments.open {
                let left = self.discussion_rect().left() - t.navigation_gap;
                right = right.min(left);
                home = home.min(left);
            }
        }
        let top = top.min(self.viewport.bottom());
        let right = right.max(self.viewport.left());
        let area = Rect::from_min_max(Pos2::new(self.viewport.left(), top), Pos2::new(right, self.viewport.bottom()));
        crate::floating::Bounds { area, home: Pos2::new(home.max(area.left()).min(area.right()), top) }
    }

    /// Publishes [`Self::floating_bounds`]; call once a frame before drawing any panel.
    pub(super) fn publish_floating_bounds(&self, ctx: &egui::Context) {
        crate::floating::set_bounds(ctx, self.floating_bounds());
    }
}

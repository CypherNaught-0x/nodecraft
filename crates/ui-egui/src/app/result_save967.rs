//! The hover toolbar for result cards that have no generation composer (#967).
use super::*;

#[derive(Clone)]
struct MenuTargets {
    session: Arc<()>,
    node: NodeId,
    frame: u64,
    targets: Vec<crate::result_save::Target>,
}

impl NodeCraftApp {
    pub(super) fn result_save_toolbar(&mut self, ui: &mut egui::Ui, viewport: Rect, interactive: bool) -> Vec<(NodeId, Intent)> {
        let ids = self.selection.nodes();
        let (Some(&id), 1, true) = (ids.first(), ids.len(), self.selection.edges().is_empty()) else { return Vec::new() };
        if self.drag.is_some() || self.marquee.is_some() || self.wiring.is_some() {
            return Vec::new();
        }
        let (Some(rect), Some(node), Some(cached)) = (
            self.painted.get(&id).copied(),
            self.session.document().canvases().get(&self.canvas).and_then(|canvas| canvas.nodes().get(&id)),
            self.schemas.get(&id),
        ) else {
            return Vec::new();
        };
        if card::generate::has_composer(node, &cached.descriptor) {
            return Vec::new();
        }
        let t = self.theme.tokens();
        let screen = self.view.rect_to_screen(viewport.min, rect);
        if !screen.intersects(viewport) {
            return Vec::new();
        }
        // Keep the toolbar outside its card even near the viewport edge. Clamping an above-
        // card position to the top can cover the card's context click (for example Place from
        // the Elements library). Reserve a conservative measured envelope for the one control.
        let label_width = ui.fonts_mut(|fonts| fonts.layout_no_wrap("Save result".into(), egui::FontId::proportional(t.body_size), t.text).size().x);
        let size = Vec2::new(label_width + 2.0 * (ui.spacing().button_padding.x + t.control_gap) + t.body_size, t.toolbar_height + 2.0 * t.control_gap);
        let insets = card::outer_insets(t, self.view.zoom, true);
        let x = screen.left().clamp(viewport.left(), (viewport.right() - size.x).max(viewport.left()));
        let y = screen.top().clamp(viewport.top(), (viewport.bottom() - size.y).max(viewport.top()));
        let candidates = [
            Pos2::new(x, screen.top() - insets.1 * self.view.zoom - t.toolbar_gap - size.y),
            Pos2::new(x, screen.bottom() + t.toolbar_gap),
            Pos2::new(screen.right() + insets.2 * self.view.zoom + t.toolbar_gap, y),
            Pos2::new(screen.left() - t.toolbar_gap - size.x, y),
        ];
        let Some(position) = candidates.into_iter().find(|position| {
            let bounds = Rect::from_min_size(*position, size);
            viewport.contains_rect(bounds) && !bounds.intersects(screen)
        }) else {
            return Vec::new();
        };
        let targets_key = egui::Id::new("result_save_toolbar_targets");
        let identity = self.session.view_identity();
        let mut intent = None;
        egui::Area::new(egui::Id::new(("result_save_toolbar", id))).fixed_pos(position).movable(false).order(egui::Order::Middle).show(ui.ctx(), |ui| {
            ui.add_enabled_ui(interactive, |ui| {
                egui::Frame::new()
                    .fill(t.panel_fill)
                    .stroke(egui::Stroke::new(t.handle_stroke, t.panel_stroke))
                    .corner_radius(t.panel_radius)
                    .inner_margin(t.control_gap)
                    .show(ui, |ui| {
                        let menu = ui.menu_button("Save result", |ui| {
                            let frame = ui.ctx().cumulative_frame_nr();
                            let cached = ui.ctx().data_mut(|data| data.get_temp::<MenuTargets>(targets_key));
                            if cached
                                .as_ref()
                                .is_some_and(|cached| cached.node == id && frame <= cached.frame.saturating_add(1) && !Arc::ptr_eq(&cached.session, &identity))
                            {
                                // A replacement document never inherits authority from an already-open popup.
                                ui.close();
                                return;
                            }
                            let mut cached = cached
                                .filter(|cached| cached.node == id && Arc::ptr_eq(&cached.session, &identity) && frame <= cached.frame.saturating_add(1))
                                .unwrap_or_else(|| MenuTargets { session: identity.clone(), node: id, frame, targets: crate::result_save::active(node) });
                            cached.frame = frame;
                            let targets = cached.targets.clone();
                            ui.ctx().data_mut(|data| data.insert_temp(targets_key, cached));
                            intent = crate::result_save::menu(ui, &targets, t);
                        });
                        if menu.inner.is_none() {
                            ui.ctx().data_mut(|data| data.remove::<MenuTargets>(targets_key));
                        }
                    });
            });
        });
        intent.filter(|_| interactive).map(|intent| vec![(id, Intent::History(intent))]).unwrap_or_default()
    }
}

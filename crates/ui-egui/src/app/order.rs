//! Drawing and dragging the input order badges (#64); the pure parts are in `crate::order`.
use crate::{canvas::CanvasView, noodle, order, theme::Tokens};
use egui::{FontId, Pos2, Rect, Sense, Stroke, Vec2};
use nodecraft_graph::Canvas;
use nodecraft_types::{CanvasId, EdgeId, Modality};
use serde_json::json;
use std::collections::BTreeMap;

/// What the badges need from the app: borrowed apart from the session, which the canvas borrows.
pub(super) struct Badges<'a> {
    pub canvas_id: CanvasId,
    pub view: &'a CanvasView,
    /// The badge being dragged: its list input and edge.
    pub reordering: &'a mut Option<(order::Input, EdgeId)>,
}

impl Badges<'_> {
    /// Numbered badges on the noodles into each list input fed by more than one. Dragging a badge
    /// onto another's place returns the `edge.reorder` that moves it there; middle- or right-
    /// dragging one pans the canvas, by the returned screen delta, as dragging a card does.
    pub(super) fn show(
        self,
        ui: &egui::Ui,
        canvas: &Canvas,
        kinds: &nodecraft_nodes::Registry,
        curves: &[(EdgeId, [Pos2; 4], Modality)],
        viewport: Rect,
        t: &Tokens,
    ) -> (Option<(&'static str, serde_json::Value)>, Vec2) {
        let Badges { canvas_id, view, reordering } = self;
        let groups = order::ordered_inputs(canvas);
        if groups.is_empty() {
            *reordering = None;
            return (None, Vec2::ZERO);
        }
        if reordering.as_ref().is_some_and(|(input, edge)| !groups.get(input).is_some_and(|edges| edges.contains(edge))) {
            *reordering = None;
        }
        let mut pan = Vec2::ZERO;
        let by_edge: BTreeMap<EdgeId, (&[Pos2; 4], Modality)> = curves.iter().map(|(id, curve, modality)| (*id, (curve, *modality))).collect();
        let radius = t.body_size * 0.75;
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let painter = ui.painter_at(viewport);
        let mut command = None;
        for (input, edges) in &groups {
            let spots: Vec<(EdgeId, Pos2, Modality)> = edges
                .iter()
                .filter_map(|id| by_edge.get(id).map(|(curve, modality)| (*id, view.to_screen(viewport.min, noodle::point(curve, order::BADGE_AT)), *modality)))
                .collect();
            for (index, (id, at, modality)) in spots.iter().enumerate() {
                let rect = Rect::from_center_size(*at, Vec2::splat(radius * 2.0));
                if !viewport.intersects(rect) {
                    continue;
                }
                let response = ui.interact(rect, ui.id().with(("input_order", *id)), Sense::drag());
                let total = spots.len();
                let into = canvas.nodes().get(&input.0).map_or_else(String::new, |node| {
                    let descriptor = kinds.get(&node.kind).ok().map(|kind| kind.descriptor());
                    format!(" into {} of {}", input.1, crate::card::title(node, descriptor))
                });
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Input {} of {total}{into}, drag to reorder", index + 1))
                });
                if response.dragged_by(egui::PointerButton::Middle) || response.dragged_by(egui::PointerButton::Secondary) {
                    pan += response.drag_delta();
                }
                if response.drag_started_by(egui::PointerButton::Primary) {
                    *reordering = Some((input.clone(), *id));
                }
                let dragging = reordering.as_ref().is_some_and(|(i, e)| i == input && e == id);
                let centre = if dragging { pointer.unwrap_or(*at) } else { *at };
                painter.circle(centre, radius, t.panel_fill, Stroke::new(t.port_stroke, t.wire(*modality)));
                painter.text(centre, egui::Align2::CENTER_CENTER, format!("{}", index + 1), FontId::proportional(radius * 1.2), t.text);
                if dragging && response.drag_stopped() {
                    *reordering = None;
                    // The badge whose place it was dropped nearest to.
                    let target = pointer.and_then(|p| spots.iter().enumerate().min_by(|a, b| a.1.1.distance(p).total_cmp(&b.1.1.distance(p))).map(|(i, _)| i));
                    if let Some(edges) = target.and_then(|target| order::moved(edges, *id, target)) {
                        command = Some(("edge.reorder", json!({"canvas": canvas_id, "to": {"node": input.0, "port": input.1}, "edges": edges})));
                    }
                }
            }
        }
        // A release the loop didn't see (the badge's own spot was off screen, after a wheel pan
        // or zoom mid-drag) still ends the drag.
        if !ui.input(|i| i.pointer.primary_down()) {
            *reordering = None;
        }
        (command, pan)
    }
}

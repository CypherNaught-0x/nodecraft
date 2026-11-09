//! New wires dragged from an unconnected input; connected inputs retain their reroute path.
use super::*;

impl NodeCraftApp {
    pub(super) fn wire_from_input(
        &self,
        ui: &egui::Ui,
        canvas: &Canvas,
        wiring: &Wiring,
        pointer: Pos2,
        t: &Tokens,
    ) -> (Option<(&'static str, serde_json::Value)>, Option<String>, Option<TurnInto>) {
        let Some(target) = canvas.nodes().get(&wiring.anchor.node) else { return (None, None, None) };
        let inputs = self.input_ports.get(target);
        let Some((index, input)) = inputs.iter().enumerate().find(|(_, input)| input.name == wiring.anchor.port) else { return (None, None, None) };
        let rect = card_geometry(&self.painted, self.session.kinds(), target, t);
        let end = noodle::anchor(rect, Side::Input, index, inputs.len());
        let mut rules = wire_rules::FromOutputs::new(canvas, self.session.kinds(), wiring, input.port_type);
        let mut accepts = |node: &Node, output: &nodecraft_graph::Port| rules.accepts(node, output);
        let origin = self.viewport.min;
        let world = self.view.to_world(origin, pointer);
        let reach = card::PORT_HIT / self.view.zoom + t.handle_radius;
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.id().with("wiring"))).with_clip_rect(self.viewport);
        let mut best: Option<(bool, f32, &Node, &nodecraft_graph::Port, Pos2)> = None;
        for (node, rect) in drop_targets(canvas, &self.painted) {
            for (index, output) in node.outputs.iter().enumerate() {
                let at = noodle::anchor(rect, Side::Output, index, node.outputs.len());
                let ok = accepts(node, output).is_ok();
                if ok {
                    let ring = t.handle_radius * self.view.zoom + t.drop_ring_gap;
                    painter.circle_stroke(self.view.to_screen(origin, at), ring, egui::Stroke::new(t.drop_ring_width, t.wire(output.port_type.modality)));
                }
                let distance = at.distance(world);
                if distance <= reach && best.is_none_or(|(best_ok, distance_to_best, ..)| (ok && !best_ok) || (ok == best_ok && distance < distance_to_best)) {
                    best = Some((ok, distance, node, output, at));
                }
            }
        }
        let source = best.map(|(_, _, node, output, at)| (node, output, at));
        let start = source.filter(|(node, output, _)| accepts(node, output).is_ok()).map_or(world, |(_, _, at)| at);
        let points = noodle::bezier(start, end).map(|p| self.view.to_screen(origin, p));
        painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
            points,
            false,
            egui::Color32::TRANSPARENT,
            egui::Stroke::new(t.wire_width, t.wire(input.port_type.modality)),
        ));
        if ui.input(|i| i.pointer.primary_down()) {
            return (None, None, None);
        }
        let Some((node, output, _)) = source else {
            let on_card = drop_targets(canvas, &self.painted).any(|(_, rect)| rect.contains(world));
            let on_canvas = self.viewport.contains(pointer) && ui.ctx().layer_id_at(pointer).is_none_or(|layer| layer == ui.layer_id());
            if on_card || !on_canvas {
                return (None, None, None);
            }
            return (None, None, Some(TurnInto::feeding(self.session.kinds(), wiring.anchor.clone(), input.port_type, world, pointer)));
        };
        if let Err(message) = accepts(node, output) {
            return (None, Some(message), None);
        }
        match self.connect_from(canvas, wiring, &rules, node, output) {
            Ok(command) => (command, None, None),
            Err(message) => (None, Some(message), None),
        }
    }
}

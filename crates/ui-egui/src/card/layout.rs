//! Logical card geometry. Never depends on widget output, camera scale or transient drafts.
use super::*;

/// Reserved body sections in world points. The same metrics drive all geometry consumers.
pub(super) struct Metrics {
    pub parameters: f32,
    pub preview: f32,
    pub answer: f32,
    pub history: f32,
    port_minimum: f32,
}

// Graph payload and descriptor validation both admit at most 256 ports per side.
const MAX_LAYOUT_PORTS: usize = 256;

fn port_minimum(count: usize, radius: f32, t: &Tokens) -> f32 {
    if count <= 1 {
        return 0.0;
    }
    let pitch = (radius + PORT_HIT) * 2.0 + t.control_gap;
    (count.min(MAX_LAYOUT_PORTS).saturating_add(1) as f32) * pitch
}

impl Metrics {
    pub fn of(node: &Node, descriptor: Option<&NodeDescriptor>, t: &Tokens) -> Self {
        let composer = descriptor.is_some_and(|d| generate::has_composer(node, d));
        // Reserve declared conditional controls too: changing a value must not move ports/wires.
        // Unlike the old per-field multiplier, each control pays only for the rows it uses.
        let controls = descriptor.map_or(t.control_row_height, |d| {
            d.params
                .params
                .iter()
                .filter(|spec| {
                    !(composer && spec.name == nodecraft_nodes::generation::PROMPT
                        || node.kind == nodecraft_nodes::comfy_workflow::ID && spec.name == nodecraft_nodes::comfy_workflow::MODEL)
                })
                .map(|spec| {
                    if matches!(spec.control, ParamControl::Asset { .. }) {
                        let remove = node.params.get(&spec.name).or(spec.default.as_ref()).is_some();
                        return t.body_size * 2.0 + t.control_gap * 4.0 + if remove { t.body_size + t.control_gap * 2.0 } else { 0.0 };
                    }
                    let control = match spec.control {
                        ParamControl::Toggle | ParamControl::Choice { .. } => t.body_size + t.control_gap,
                        ParamControl::Text { multiline: true, .. } => t.body_size * t.text_rows as f32,
                        ParamControl::Slider { min: Some(_), max: Some(_), .. } => t.control_row_height / 2.0 + t.body_size + t.control_gap * 2.0,
                        ParamControl::Model => t.body_size * 2.0 + t.control_gap * 3.0,
                        _ => t.control_row_height / 2.0,
                    };
                    t.body_size + t.control_gap + control + t.control_gap * 2.0
                })
                .sum::<f32>()
        });
        let composer = if composer { t.composer_body_height - t.card_padding * 2.0 } else { 0.0 };
        let parameters = (controls + composer).max(t.control_row_height / 2.0).min(t.card_max_height - t.card_padding * 2.0);
        let shows = nodecraft_engine::split::image_of(node).is_some()
            // A folder of images shows the images it gives.
            || node.kind == nodecraft_nodes::folder::ID && nodecraft_engine::split::image_rows(node).1 > 0
            // The app previews a PDF source's first page through the image renderer too.
            || node.kind == nodecraft_nodes::document::SOURCE && matches!(node.params.get("asset"), Some(Value::Media(_)))
            || crate::video::video_of(node).is_some()
            || crate::audio::audio_of(node).is_some()
            || crate::mesh_view::mesh_of(node).is_some()
            || node.kind == nodecraft_nodes::generation::AUDIO_KIND
            // Live image-action previews can arrive before the first recorded result. Reserve
            // their one region from the declared output, never from transient renderer state.
            || descriptor.is_some_and(|d| d.category == nodecraft_nodes::Category::Action
                && d.outputs.iter().any(|port| port.port_type.modality == nodecraft_types::Modality::Image));
        Self {
            parameters,
            // Ordinary anchors need enough logical room for their nominal hit envelopes. Take
            // the larger saved/declared side, not renderer state or camera scale. Migration-only
            // input previews are also protected by the renderer's disjoint anchor cells.
            port_minimum: port_minimum(node.inputs.len().max(descriptor.map_or(0, |d| d.inputs.len())), t.port_radius.max(t.port_min_radius), t)
                .max(port_minimum(node.outputs.len().max(descriptor.map_or(0, |d| d.outputs.len())), t.handle_radius.max(t.handle_min_screen_radius), t)),
            preview: if shows { t.card_preview_height } else { 0.0 },
            answer: if crate::text_preview::eligible(descriptor) {
                t.card_preview_height * 2.0
            } else if node.kind == nodecraft_nodes::generation::TEXT_KIND {
                t.card_preview_height
            } else {
                0.0
            },
            // Two compact history button rows, plus separation from the parameters.
            history: if node.history.is_empty() { 0.0 } else { history::height(t) + t.control_gap },
        }
    }

    pub fn size(&self, t: &Tokens) -> Vec2 {
        Vec2::new(t.card_width, (t.card_padding * 2.0 + self.parameters + self.preview + self.answer + self.history).max(self.port_minimum))
    }
}

/// Cheap broadphase for admitted instances and placeholders. Parameter budgets are capped;
/// both output regions can coexist, and history is independent. Admission requires the
/// resolved ports to equal the saved ports, so these counts also bound instance schemas.
pub(super) fn broadphase_size(node: &Node, t: &Tokens) -> Vec2 {
    let ports = port_minimum(node.inputs.len(), t.port_radius.max(t.port_min_radius), t).max(port_minimum(
        node.outputs.len(),
        t.handle_radius.max(t.handle_min_screen_radius),
        t,
    ));
    let body = t.card_max_height + t.card_preview_height * 2.0 + if node.history.is_empty() { 0.0 } else { history::height(t) + t.control_gap };
    Vec2::new(t.card_width, body.max(ports))
}

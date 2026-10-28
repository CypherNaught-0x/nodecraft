//! Connected media on a text card (#84): the images, PDFs, clips and sounds wired into the card,
//! drawn as a row of small thumbnails above its answer, so the person sees what the model reads.
//! View only: what a run sends is decided by the planner. The row takes its room from the answer
//! area, so wiring media never changes the card's size or moves its ports.
use super::*;
use nodecraft_graph::{Canvas, Endpoint};
use nodecraft_types::AssetRef;

/// Most connected assets one card counts; any past these are not looked at.
pub const MAX_COUNTED: usize = 256;
/// Most thumbnails the host prepares; the card draws as many as fit its width.
pub const MAX_SHOWN: usize = 8;
/// Most values looked at while walking one card's nested list results.
const MAX_VALUES: usize = 4096;

/// What a connected asset is, which decides its tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Document,
    Video,
    Audio,
    Other,
}

impl MediaKind {
    pub fn of(asset: &AssetRef) -> Self {
        let essence = asset.mime.essence();
        if essence == "application/pdf" {
            Self::Document
        } else if essence.starts_with("image/") {
            Self::Image
        } else if essence.starts_with("video/") {
            Self::Video
        } else if essence.starts_with("audio/") {
            Self::Audio
        } else {
            Self::Other
        }
    }
    /// Whether its tile is a picture: an image, or a PDF's first page.
    pub fn previews(self) -> bool {
        matches!(self, Self::Image | Self::Document)
    }
    /// Its name in the tile's accessible label.
    fn name(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Document => "PDF",
            Self::Video => "video",
            Self::Audio => "sound",
            Self::Other => "file",
        }
    }
    /// The word drawn on a tile that has no picture.
    fn word(self) -> &'static str {
        match self {
            Self::Image => "Image",
            Self::Document => "PDF",
            Self::Video => "Video",
            Self::Audio => "Sound",
            Self::Other => "File",
        }
    }
}

/// One tile: what the asset is, and its picture when it has one.
#[derive(Clone, Copy)]
pub struct Thumb {
    pub kind: MediaKind,
    pub preview: Option<crate::preview::Preview>,
}

/// The media wired into `node`'s inputs, in port order then wire order: each wire's value, from
/// the upstream card's usable active result or, for a source card, its asset. Text ports are
/// left out (connected text shows in the prompt instead). At most [`MAX_COUNTED`].
pub fn assets<'a>(canvas: &'a Canvas, node: &Node) -> Vec<&'a AssetRef> {
    let mut wires: Vec<(usize, u32, &nodecraft_graph::Edge)> = canvas
        .edges()
        .values()
        .filter(|edge| edge.to.node == node.id)
        .filter_map(|edge| {
            let index = node.inputs.iter().position(|port| port.name == edge.to.port)?;
            let port = node.inputs.get(index)?;
            (port.port_type.modality != Modality::Text).then_some((index, edge.order, edge))
        })
        .collect();
    wires.sort_by_key(|(index, order, edge)| (*index, *order, edge.id));
    let mut found = Vec::new();
    let mut budget = MAX_VALUES;
    for (_, _, edge) in wires {
        let Some(value) = value_on(canvas, &edge.from) else { continue };
        let mut stack = vec![value];
        while let Some(value) = stack.pop() {
            if found.len() >= MAX_COUNTED || budget == 0 {
                return found;
            }
            budget -= 1;
            match value {
                Value::Media(asset) => found.push(asset),
                // Reversed onto the stack so items come off in their own order.
                Value::List(items) => stack.extend(items.iter().rev()),
                _ => {}
            }
        }
    }
    found
}

/// The value a wire from `from` carries, as far as the canvas already knows it.
fn value_on<'a>(canvas: &'a Canvas, from: &Endpoint) -> Option<&'a Value> {
    let upstream = canvas.nodes().get(&from.node)?;
    // Results are positional; one recorded for another port layout, or cut short, isn't used.
    let usable =
        upstream.active.and_then(|id| upstream.history.iter().find(|g| g.id == id)).filter(|g| g.outputs.len() == upstream.outputs.len() && !g.is_partial());
    match usable {
        Some(result) => result.outputs.get(upstream.outputs.iter().position(|port| port.name == from.port)?),
        None if upstream.kind.starts_with("source.") => upstream.params.get("asset"),
        None => None,
    }
}

/// Draws the tiles in a row along the top of `area`, as many as fit; when they don't all fit the
/// last one counts the rest. `count` is every connected asset, shown or not. Returns the room
/// left under the row (all of `area` when nothing is connected).
pub(super) fn show(ui: &mut egui::Ui, area: Rect, thumbs: &[Thumb], count: usize, t: &Tokens) -> Rect {
    if count == 0 || area.height() <= t.connected_thumb + t.control_gap {
        return area;
    }
    let side = t.connected_thumb;
    let gap = t.control_gap;
    // Finite and small: the card's width over a positive token.
    let fit = ((area.width() + gap) / (side + gap)).floor().max(1.0) as usize;
    let mut shown = thumbs.len().min(count).min(fit);
    if count > shown && shown == fit {
        shown = fit.saturating_sub(1);
    }
    let painter = ui.painter().with_clip_rect(area.intersect(ui.clip_rect()));
    let tile = |i: usize| Rect::from_min_size(area.min + Vec2::new(i as f32 * (side + gap), 0.0), Vec2::splat(side));
    for (i, thumb) in thumbs.iter().take(shown).enumerate() {
        let rect = tile(i);
        painter.rect_filled(rect, t.card_radius / 2, t.canvas_bg);
        let mut label = format!("Connected {} {} of {count}", thumb.kind.name(), i.saturating_add(1));
        match thumb.preview {
            Some(crate::preview::Preview::Ready { texture, size, source_size }) => {
                painter.image(texture, rect, cover(size), egui::Color32::WHITE);
                label.push_str(&format!(", {}×{}", source_size[0], source_size[1]));
            }
            Some(crate::preview::Preview::Pending) => {
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, "…", FontId::proportional(t.label_size), t.text_muted);
            }
            Some(crate::preview::Preview::Unavailable(_)) | None => {
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, thumb.kind.word(), FontId::proportional(t.label_size), t.text_muted);
            }
        }
        painter.rect_stroke(rect, t.card_radius / 2, Stroke::new(1.0, t.card_stroke), StrokeKind::Inside);
        let response = ui.interact(rect, ui.id().with(("connected", i)), Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, label.clone()));
        response.on_hover_text(label);
    }
    let rest = count.saturating_sub(shown);
    if rest > 0 {
        let rect = tile(shown);
        painter.rect_filled(rect, t.card_radius / 2, t.canvas_bg);
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, format!("+{rest}"), FontId::proportional(t.label_size), t.text);
        let label = if shown == 0 { format!("{rest} connected inputs") } else { format!("{rest} more connected inputs") };
        let response = ui.interact(rect, ui.id().with("connected_more"), Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label.clone()));
        response.on_hover_text(label);
    }
    Rect::from_min_max(Pos2::new(area.left(), area.top() + side + gap), area.max)
}

/// The middle square of a `size` picture, so a tile is filled without stretching it.
fn cover(size: [u32; 2]) -> Rect {
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let (fx, fy) = if w > h { (h / w, 1.0) } else { (1.0, w / h) };
    Rect::from_min_max(Pos2::new((1.0 - fx) / 2.0, (1.0 - fy) / 2.0), Pos2::new((1.0 + fx) / 2.0, (1.0 + fy) / 2.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_middle_square_of_a_picture_fills_a_tile() {
        let wide = cover([200, 100]);
        assert_eq!((wide.min.x, wide.max.x, wide.min.y, wide.max.y), (0.25, 0.75, 0.0, 1.0));
        let tall = cover([50, 100]);
        assert_eq!((tall.min.y, tall.max.y, tall.min.x, tall.max.x), (0.25, 0.75, 0.0, 1.0));
        assert_eq!(cover([0, 0]), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), "a degenerate size is read as square");
    }
}

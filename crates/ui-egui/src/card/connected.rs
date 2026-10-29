//! Connected media on a text card (#84): the images, PDFs, clips and sounds wired into the card,
//! drawn as a row of small thumbnails above its answer, so the person sees what the model reads.
//! View only: what a run sends is decided by the planner. The row takes its room from the answer
//! area, so wiring media never changes the card's size or moves its ports.
use super::*;
use nodecraft_graph::{Canvas, Edge};
use nodecraft_types::{AssetRef, NodeId};

/// Most connected assets one card counts; any past these are not looked at, and the count says
/// "or more".
pub const MAX_COUNTED: usize = 256;
/// Most values looked at while walking one card's nested list results.
const MAX_VALUES: usize = 4096;
/// The most a thumbnail's request grows for a wide or tall picture, whose middle square is shown.
const MAX_STRETCH: f32 = 4.0;

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

/// The wires into each text card on `canvas`, built once a frame rather than once a card.
pub fn incoming(canvas: &Canvas) -> BTreeMap<NodeId, Vec<&Edge>> {
    let mut wires: BTreeMap<NodeId, Vec<&Edge>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        if canvas.nodes().get(&edge.to.node).is_some_and(|node| node.kind == nodecraft_nodes::generation::TEXT_KIND) {
            wires.entry(edge.to.node).or_default().push(edge);
        }
    }
    wires
}

/// The media `wires` bring into `node`'s inputs, in port order then wire order. Each wire carries
/// what a run would read from it ([`nodecraft_exec::planner::value_without_running`]): a source
/// card's (or an Element's) current value, else the upstream card's usable active result. Text
/// ports are left out (connected text shows in the prompt instead). At most [`MAX_COUNTED`].
pub fn assets(canvas: &Canvas, kinds: &nodecraft_nodes::Registry, node: &Node, wires: &[&Edge]) -> Vec<AssetRef> {
    let mut wires: Vec<(usize, u32, &Edge)> = wires
        .iter()
        .filter(|edge| edge.to.node == node.id)
        .filter_map(|edge| {
            let index = node.inputs.iter().position(|port| port.name == edge.to.port)?;
            let port = node.inputs.get(index)?;
            (port.port_type.modality != Modality::Text).then_some((index, edge.order, *edge))
        })
        .collect();
    wires.sort_by_key(|(index, order, edge)| (*index, *order, edge.id));
    let mut found = Vec::new();
    let mut budget = MAX_VALUES;
    for (_, _, edge) in wires {
        let Some(value) = canvas.nodes().get(&edge.from.node).and_then(|up| nodecraft_exec::planner::value_without_running(kinds, up, &edge.from.port)) else {
            continue;
        };
        let mut stack = vec![&value];
        while let Some(value) = stack.pop() {
            if found.len() >= MAX_COUNTED || budget == 0 {
                return found;
            }
            budget -= 1;
            match value {
                Value::Media(asset) => found.push(asset.clone()),
                // Reversed onto the stack so items come off in their own order.
                Value::List(items) => stack.extend(items.iter().rev()),
                _ => {}
            }
        }
    }
    found
}

/// The width of a text card's answer area, which the row spans.
pub fn row_width(t: &Tokens) -> f32 {
    (t.card_width - t.card_padding * 2.0).max(0.0)
}

/// How many pictures a row `width` wide shows of `count` connected assets; when they don't all
/// fit, the last square counts the rest instead. The host asks for only these previews.
pub fn shown(width: f32, count: usize, t: &Tokens) -> usize {
    let (side, gap) = (t.connected_thumb, t.control_gap);
    // Finite and small: the card's width over a positive token.
    let fit = if width.is_finite() && side + gap > 0.0 { ((width + gap) / (side + gap)).floor().max(1.0) as usize } else { 1 };
    if count > fit { fit.saturating_sub(1) } else { count }
}

/// The edge to request a thumbnail at: `side` screen pixels for its middle square, more for a
/// wide or tall picture (the preview's longer side is what is bucketed), up to [`MAX_STRETCH`].
pub fn request_edge(asset: &AssetRef, side: f32) -> f32 {
    let stretch = match (asset.meta.width, asset.meta.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (w.max(h) as f32 / w.min(h) as f32).min(MAX_STRETCH),
        _ => 1.0,
    };
    side * stretch
}

/// Draws the tiles in a row along the top of `area`: the pictures in `thumbs` (as many as fit),
/// then, when `count` connected assets don't all fit, a square counting the rest. Strokes keep
/// one screen pixel at `zoom`. Returns the room left under the row (all of `area` when nothing
/// is connected).
pub(super) fn show(ui: &mut egui::Ui, area: Rect, thumbs: &[Thumb], count: usize, zoom: f32, t: &Tokens) -> Rect {
    if count == 0 || area.height() <= t.connected_thumb + t.control_gap {
        return area;
    }
    let side = t.connected_thumb;
    let gap = t.control_gap;
    let shown = shown(area.width(), count, t).min(thumbs.len());
    let stroke = Stroke::new(if zoom.is_finite() && zoom > 0.0 { 1.0 / zoom } else { 1.0 }, t.card_stroke);
    let painter = ui.painter().with_clip_rect(area.intersect(ui.clip_rect()));
    let tile = |i: usize| Rect::from_min_size(area.min + Vec2::new(i as f32 * (side + gap), 0.0), Vec2::splat(side));
    let total = if count >= MAX_COUNTED { format!("{count} or more") } else { count.to_string() };
    for (i, thumb) in thumbs.iter().take(shown).enumerate() {
        let rect = tile(i);
        painter.rect_filled(rect, t.card_radius / 2, t.canvas_bg);
        let mut label = format!("Connected {} {} of {total}", thumb.kind.name(), i.saturating_add(1));
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
        painter.rect_stroke(rect, t.card_radius / 2, stroke, StrokeKind::Inside);
        let response = ui.interact(rect, ui.id().with(("connected", i)), Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, label.clone()));
        response.on_hover_text(label);
    }
    let rest = count.saturating_sub(shown);
    if rest > 0 {
        let rect = tile(shown);
        let capped = count >= MAX_COUNTED;
        painter.rect_filled(rect, t.card_radius / 2, t.canvas_bg);
        let text = if capped { format!("+{rest}…") } else { format!("+{rest}") };
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, FontId::proportional(t.label_size), t.text);
        let rest = if capped { format!("{rest} or more") } else { rest.to_string() };
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

    #[test]
    fn a_row_shows_what_fits_and_saves_a_square_for_the_rest() {
        let t = crate::theme::DARK;
        let width = row_width(&t);
        // 288 points hold six 40-point squares 6 apart.
        assert_eq!((shown(width, 0, &t), shown(width, 6, &t), shown(width, 7, &t), shown(width, 300, &t)), (0, 6, 5, 5));
        assert_eq!(shown(10.0, 3, &t), 0, "one square, which counts them all");
        assert_eq!(shown(f32::NAN, 3, &t), 0);
    }

    #[test]
    fn a_wide_picture_is_requested_big_enough_for_its_middle_square() {
        let asset = |width, height| AssetRef {
            hash: nodecraft_types::Blake3::from_bytes([1; 32]),
            mime: "image/png".parse().unwrap(),
            meta: nodecraft_types::MediaMeta { width, height, ..Default::default() },
        };
        assert_eq!(request_edge(&asset(Some(300), Some(100)), 40.0), 120.0);
        assert_eq!(request_edge(&asset(Some(100), Some(1000)), 40.0), 160.0, "at most four times");
        assert_eq!(request_edge(&asset(None, Some(100)), 40.0), 40.0, "unknown sizes are taken as square");
        assert_eq!(request_edge(&asset(Some(0), Some(100)), 40.0), 40.0);
    }
}

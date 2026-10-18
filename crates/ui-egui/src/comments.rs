//! Bounded local discussion view state. Accepted text remains borrowed in the graph.
use nodecraft_graph::{MAX_COMMENT_BYTES, MAX_REPLY_BYTES, Position};
use nodecraft_types::{CanvasId, CommentId, Error, ErrorKind, NodeId, ProjectId};
use std::collections::BTreeMap;
pub const MAX_DRAFT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_DRAFTS: usize = 8;
pub const MESSAGE_PAGE: usize = 16;
pub const CHUNK_BYTES: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Create(CommentId),
    Root(CommentId),
    Reply(CommentId, CommentId),
    NewReply(CommentId, CommentId),
}
impl Target {
    pub fn root(self) -> CommentId {
        match self {
            Self::Create(id) | Self::Root(id) | Self::Reply(id, _) | Self::NewReply(id, _) => id,
        }
    }
}
pub struct Draft {
    pub session: std::sync::Arc<()>,
    pub project: ProjectId,
    pub canvas: CanvasId,
    pub revision: u64,
    pub target: Target,
    pub text: String,
    pub editor_height: f32,
    pub pos: Position,
    pub node: Option<NodeId>,
    pub dirty: bool,
    pub error: Option<String>,
}
impl Draft {
    pub fn limit(&self) -> usize {
        if matches!(self.target, Target::Reply(..) | Target::NewReply(..)) { MAX_REPLY_BYTES } else { MAX_COMMENT_BYTES }
    }
}
#[derive(Default)]
pub struct Discussions {
    pub open: bool,
    pub active: Option<CommentId>,
    pub show_resolved: bool,
    pub page: usize,
    pub message: Option<CommentId>,
    pub chunk: usize,
    pub drafts: BTreeMap<u64, Draft>,
    pub editing: Option<u64>,
    pub last_focus: Option<egui::Id>,
    pub focus_pending: Option<u64>,
    pub drag: Option<PinDrag>,
    /// Display-only displacement retained after dropping an active pin; invalidated by a
    /// different anchor, thread, session or closing the panel. Never persisted.
    pub placement: Option<(CommentId, Position, egui::Vec2)>,
    pub gesture_cancelled: bool,
    pub session: Option<std::sync::Arc<()>>,
    pub pins: BTreeMap<CommentId, Position>,
    pub marker_ids: Vec<egui::Id>,
    pub stamp: Option<(ProjectId, CanvasId, u64)>,
    next_draft: u64,
}
pub struct PinDrag {
    pub session: std::sync::Arc<()>,
    pub canvas: CanvasId,
    pub id: CommentId,
    pub revision: u64,
    pub pos: Position,
    pub node: Option<NodeId>,
    pub start: egui::Pos2,
    pub origin: Position,
    pub offset: egui::Vec2,
    pub display_origin: egui::Pos2,
    pub cancelled: bool,
}
impl Discussions {
    pub fn bytes(&self) -> usize {
        self.drafts.values().map(|draft| draft.text.capacity()).sum()
    }
    pub fn add_draft(&mut self, draft: Draft) -> Result<u64, Error> {
        if draft.text.len() > draft.limit().saturating_add(4) {
            return Err(Error::new(ErrorKind::InvalidInput, "draft exceeds message byte limit"));
        }
        if self.drafts.len() >= MAX_DRAFTS || self.bytes().saturating_add(draft.text.capacity()) > MAX_DRAFT_BYTES {
            return Err(Error::new(ErrorKind::Conflict, "Unsaved draft limit reached. Save or explicitly discard another draft first."));
        }
        let id = self.next_draft.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "draft identifier limit reached"))?;
        self.next_draft = id;
        self.drafts.insert(id, draft);
        self.editing = Some(id);
        self.open = true;
        Ok(id)
    }
    pub fn close(&mut self) {
        self.open = false;
        self.placement = None;
        self.editing = None;
        self.drafts.retain(|_, draft| draft.dirty || draft.error.is_some());
    }
    pub(crate) fn reset_session_view(&mut self) {
        self.close();
        self.active = None;
        self.message = None;
        self.page = 0;
        self.chunk = 0;
        self.last_focus = None;
        self.focus_pending = None;
        self.drag = None;
        self.gesture_cancelled = false;
        self.session = None;
        self.pins.clear();
        self.marker_ids.clear();
        self.stamp = None;
    }
    /// Fresh UUIDv4 checked against live roots/replies and retained draft IDs. Native
    /// entropy and browser Web Crypto share a fallible shell-only boundary.
    pub fn allocate_comment(&self, project: &nodecraft_graph::Project) -> Result<CommentId, Error> {
        let mut used = std::collections::BTreeSet::new();
        for canvas in project.canvases().values() {
            for root in canvas.comments().values() {
                used.insert(*root.id.as_bytes());
                used.extend(root.replies.iter().map(|reply| *reply.id.as_bytes()));
            }
        }
        for draft in self.drafts.values() {
            used.insert(*draft.target.root().as_bytes());
            if let Target::Reply(_, id) | Target::NewReply(_, id) = draft.target {
                used.insert(*id.as_bytes());
            }
        }
        crate::ids::random_id(|bytes| used.contains(bytes)).map(CommentId::from_bytes)
    }
}
pub fn message_count(count: usize) -> String {
    format!("{count} {}", if count == 1 { "message" } else { "messages" })
}

pub(crate) fn checkbox_visuals(visuals: &mut egui::Visuals, t: &crate::theme::Tokens) {
    // The shared widget fill matches the panel; an explicit contrasting outline is
    // essential for an unchecked checkbox, not just its checked glyph.
    for widget in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered, &mut visuals.widgets.active] {
        widget.bg_stroke = egui::Stroke::new(t.handle_stroke, t.text);
        widget.fg_stroke = egui::Stroke::new(t.handle_stroke, t.text);
        widget.corner_radius = egui::CornerRadius::same(t.comment_marker_radius);
    }
}

pub(crate) fn pin_bubble(ui: &mut egui::Ui, t: &crate::theme::Tokens, resolved: bool, active: bool, messages: usize) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(t.comment_marker_size), egui::Sense::click_and_drag());
    let body = rect.shrink(t.comment_marker_ring);
    let body = egui::Rect::from_min_max(body.min, body.max - egui::vec2(0.0, t.comment_marker_tail));
    let fill = if resolved { t.panel_fill } else { t.text };
    let glyph = if resolved { t.text } else { t.canvas_bg };
    let stroke = egui::Stroke::new(t.handle_stroke, t.text);
    let painter = ui.painter();
    painter.rect(body, t.comment_marker_radius, fill, stroke, egui::StrokeKind::Inside);
    painter.add(egui::Shape::convex_polygon(
        vec![
            body.left_bottom() + egui::vec2(t.comment_marker_tail, 0.0),
            body.left_bottom() + egui::vec2(t.comment_marker_tail, t.comment_marker_tail),
            body.center_bottom(),
        ],
        fill,
        stroke,
    ));
    if resolved {
        // Geometry stays recognizable with the actual bundled font on every platform.
        let mark = body.shrink(t.comment_marker_tail);
        painter.add(egui::Shape::line(vec![mark.left_center(), mark.center_bottom(), mark.right_top()], egui::Stroke::new(t.handle_stroke, glyph)));
    } else {
        painter.text(body.center(), egui::Align2::CENTER_CENTER, messages.to_string(), egui::FontId::proportional(t.body_size), glyph);
    }
    if active || response.has_focus() {
        // A second outline is a non-colour active cue in both themes.
        painter.rect_stroke(rect, t.comment_marker_radius, stroke, egui::StrokeKind::Inside);
    }
    response
}
pub fn preview(text: &str) -> String {
    let mut out: String = text.chars().take(128).collect();
    if text.chars().nth(128).is_some() {
        out.push('…');
    }
    out
}
pub fn chunk(text: &str, offset: usize) -> (&str, usize) {
    let mut start = offset.min(text.len());
    while !text.is_char_boundary(start) {
        start = start.saturating_sub(1);
    }
    let mut end = start.saturating_add(CHUNK_BYTES).min(text.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (text.get(start..end).unwrap_or(""), end)
}
/// Reject an insertion as a unit instead of silently truncating a pasted message. Existing
/// accepted1MiB bodies remain editable. Inactive previews do not clone the full body;
/// egui's focused editor still makes bounded transient full-text/layout copies each frame.
pub struct Buffer<'a> {
    pub text: &'a mut String,
    pub ceiling: usize,
    pub rejected: &'a mut bool,
    deleted: Option<(usize, String)>,
}
impl<'a> Buffer<'a> {
    pub fn new(text: &'a mut String, ceiling: usize, rejected: &'a mut bool) -> Self {
        Self { text, ceiling, rejected, deleted: None }
    }
}
impl Buffer<'_> {
    fn reject(&mut self) {
        if let Some((at, deleted)) = self.deleted.take()
            && self.text.is_char_boundary(at)
        {
            self.text.insert_str(at, &deleted);
        }
        *self.rejected = true;
    }
}
impl egui::TextBuffer for Buffer<'_> {
    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<Buffer<'static>>()
    }
    fn is_mutable(&self) -> bool {
        true
    }
    fn as_str(&self) -> &str {
        self.text
    }
    fn insert_text(&mut self, text: &str, index: egui::text::CharIndex) -> usize {
        let total = self.text.len().saturating_add(text.len());
        if total > self.ceiling {
            self.reject();
            return 0;
        }
        let Some(byte) =
            self.text.char_indices().nth(index.0).map(|(byte, _)| byte).or_else(|| (index.0 == self.text.chars().count()).then_some(self.text.len()))
        else {
            return 0;
        };
        if self.text.capacity() < total {
            let mut next = String::new();
            if next.try_reserve_exact(total).is_err() || next.capacity() > self.ceiling {
                self.reject();
                return 0;
            }
            next.push_str(self.text.get(..byte).unwrap_or(""));
            next.push_str(text);
            next.push_str(self.text.get(byte..).unwrap_or(""));
            *self.text = next;
        } else {
            self.text.insert_str(byte, text);
        }
        self.deleted = None;
        text.chars().count()
    }
    fn replace_with(&mut self, text: &str) {
        if text.len() > self.ceiling {
            self.reject();
            return;
        }
        if text.len() > self.text.capacity() {
            let mut next = String::new();
            if next.try_reserve_exact(text.len()).is_err() || next.capacity() > self.ceiling {
                self.reject();
                return;
            }
            next.push_str(text);
            *self.text = next;
        } else {
            self.text.clear();
            self.text.push_str(text);
        }
        self.deleted = None;
    }
    fn delete_char_range(&mut self, range: std::ops::Range<egui::text::CharIndex>) {
        let byte = |index: usize| self.text.char_indices().nth(index).map_or(self.text.len(), |(byte, _)| byte);
        let start = byte(range.start.0);
        let end = byte(range.end.0);
        if start <= end {
            self.deleted = self.text.get(start..end).map(|text| (start, text.to_owned()));
            self.text.replace_range(start..end, "");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::TextBuffer;
    fn draft(text: String) -> Draft {
        Draft {
            session: std::sync::Arc::new(()),
            project: ProjectId::from_bytes([1; 16]),
            canvas: CanvasId::from_bytes([2; 16]),
            revision: 0,
            target: Target::Root(CommentId::from_bytes([3; 16])),
            text,
            editor_height: 0.0,
            pos: Position::default(),
            node: None,
            dirty: true,
            error: None,
        }
    }
    #[test]
    fn resolved_pin_paints_check_geometry_without_font_and_retains_active_outline() {
        for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
            for resolved in [false, true] {
                for active in [false, true] {
                    let ctx = egui::Context::default();
                    let tokens = theme.tokens();
                    let mut bubble = egui::Rect::NOTHING;
                    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                        bubble = pin_bubble(ui, tokens, resolved, active, 7).rect;
                    });
                    output.textures_delta.clear();
                    let paths: Vec<_> = output
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Path(path) if !path.closed && path.points.len() == 3 => Some(path),
                            _ => None,
                        })
                        .collect();
                    let texts: Vec<_> = output
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                            _ => None,
                        })
                        .collect();
                    if resolved {
                        assert_eq!(paths.len(), 1, "resolved state must paint a stroked check, independent of font glyphs");
                        let path = paths[0];
                        assert!(path.points.iter().all(|point| bubble.contains(*point)));
                        assert!(path.points[0].x < path.points[1].x && path.points[1].x < path.points[2].x);
                        assert!(path.points[1].y > path.points[0].y && path.points[0].y > path.points[2].y);
                        assert_eq!(path.stroke.width, tokens.handle_stroke);
                        assert!(texts.is_empty(), "resolved marker must not depend on text rendering");
                    } else {
                        assert!(paths.is_empty());
                        assert_eq!(texts, ["7"], "unresolved message count remains visible");
                    }
                    let outlines = output.shapes.iter().filter(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.rect == bubble)).count();
                    assert_eq!(outlines, usize::from(active), "active marker retains a separate outer outline");
                }
            }
        }
    }

    #[test]
    fn aggregate_draft_limits_refuse_new_buffers_without_eviction() {
        let mut state = Discussions::default();
        state.add_draft(draft("x".repeat(1024 * 1024))).unwrap();
        state.add_draft(draft("y".repeat(1024 * 1024))).unwrap();
        assert!(state.add_draft(draft("z".into())).is_err());
        assert_eq!(state.bytes(), MAX_DRAFT_BYTES);
        assert_eq!(state.drafts.len(), 2);
        for draft in state.drafts.values_mut() {
            draft.text.clear();
        }
        assert_eq!(state.bytes(), MAX_DRAFT_BYTES, "deleting text does not erase its retained allocation");
        assert!(state.add_draft(draft("z".into())).is_err());
        let mut state = Discussions::default();
        for _ in 0..MAX_DRAFTS {
            state.add_draft(draft(String::new())).unwrap();
        }
        assert!(state.add_draft(draft(String::new())).is_err());
        assert_eq!(state.drafts.len(), MAX_DRAFTS);
    }
    #[test]
    fn unicode_chunks_and_nontruncating_paste_preserve_selected_tail() {
        let text = "x".repeat(CHUNK_BYTES - 1) + "é-tail";
        let (first, end) = chunk(&text, 0);
        assert_eq!(first.len(), CHUNK_BYTES - 1);
        assert_eq!(chunk(&text, end).0, "é-tail");
        let mut text = "é-tail".to_owned();
        let mut rejected = false;
        {
            let mut buffer = Buffer::new(&mut text, 8, &mut rejected);
            buffer.delete_char_range(egui::text::CharIndex(0)..egui::text::CharIndex(1));
            assert_eq!(buffer.insert_text("oversized paste", egui::text::CharIndex(0)), 0);
        }
        assert!(rejected);
        assert_eq!(text, "é-tail");
    }
}

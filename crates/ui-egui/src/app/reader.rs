//! The PDF reader (#590): a window over one PDF source with a column of page thumbnails and the
//! current page, paged with Left/Right, Page Up/Down, Home and End, zoomed with the slider. All of
//! that is view state and never saved. Dragging over the page selects a region; its text, or the
//! whole page's, becomes a new Text node, and "Extract page" copies the page into a new PDF node.
//! Opened from a PDF to images or PDF to text node, it also sets that node's page range
//! (`document.use_pages`, undoable, marking its results stale).
use super::NodeCraftApp;
use crate::{card, ids, preview::Preview};
use egui::{Key, Modifiers, Pos2, Rect, Sense, Vec2};
use nodecraft_graph::{Node, Position};
use nodecraft_types::{AssetRef, NodeId, Value};
use serde_json::json;

/// Zoom steps the reader allows, as a share of the window's width.
const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 4.0;

/// What the reader shows; view state only.
pub(super) struct Reader {
    /// The PDF source node read.
    pub(super) node: NodeId,
    /// The PDF to images or PDF to text node whose page range the reader sets, if opened from one.
    pub(super) target: Option<NodeId>,
    /// The page shown, from 0.
    pub(super) page: u32,
    pub(super) zoom: f32,
    /// The selected region of the page as fractions (left, top, right, bottom), and where a drag
    /// started, as a fraction of the page.
    pub(super) region: Option<[f32; 4]>,
    drag: Option<Pos2>,
    /// Set when a key moved the page, so the thumbnail column scrolls to it.
    follow: bool,
    /// The page pane's width last frame, so the shown page is asked for before the thumbnails.
    width: f32,
    /// Where the page was drawn last frame, once drawn at its real proportions.
    pub(super) area: Option<Rect>,
}

impl Reader {
    pub(super) fn new(node: NodeId, target: Option<NodeId>, page: u32) -> Self {
        Self { node, target, page, zoom: 1.0, region: None, drag: None, follow: true, width: 0.0, area: None }
    }

    /// Moves to page `page` (clamped to the document), dropping the selection when it changes.
    pub(super) fn go(&mut self, page: i64, pages: u32) {
        let last = i64::from(pages.max(1)) - 1;
        let page = u32::try_from(page.clamp(0, last)).unwrap_or(0);
        if page != self.page {
            self.page = page;
            self.region = None;
            self.drag = None;
        }
    }
}

/// The PDF a node holds: its asset when it is a PDF source.
fn pdf_of(node: &Node) -> Option<&AssetRef> {
    match node.params.get("asset") {
        Some(Value::Media(asset)) if node.kind == nodecraft_nodes::document::SOURCE => Some(asset),
        _ => None,
    }
}

/// The PDF source wired into a PDF to images or PDF to text node.
pub(super) fn source_of(canvas: &nodecraft_graph::Canvas, node: NodeId) -> Option<NodeId> {
    canvas
        .edges()
        .values()
        .filter(|edge| edge.to.node == node && edge.to.port == "document")
        .find_map(|edge| canvas.nodes().get(&edge.from.node).and_then(pdf_of).map(|_| edge.from.node))
}

/// Whether `node` is a PDF to images or PDF to text node.
pub(super) fn reads_pages(node: &Node) -> bool {
    node.kind == nodecraft_nodes::document::PAGES || node.kind == nodecraft_nodes::document::TEXT
}

/// A page number param of `node` (from 1), or `default`.
fn page_param(node: &Node, name: &str, default: u64) -> u64 {
    match node.params.get(name) {
        Some(Value::Unsigned(n)) => n.0,
        Some(Value::Integer(n)) => u64::try_from(n.0).unwrap_or(default),
        _ => default,
    }
}

/// What the reader's buttons asked for this frame.
enum Ask {
    Text,
    Page,
    /// A page range for the target node (first, last; from 1, last 0 = the end).
    Range(u64, u64),
}

impl NodeCraftApp {
    /// Opens the reader on `node`: a PDF source, or a PDF to images or PDF to text node (which
    /// reads the PDF wired into it, starting at its first page).
    pub(super) fn open_reader(&mut self, node: NodeId) {
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else {
            return;
        };
        let Some(found) = canvas.nodes().get(&node) else {
            return;
        };
        if pdf_of(found).is_some() {
            self.reader = Some(Reader::new(node, None, 0));
        } else if reads_pages(found) {
            match source_of(canvas, node) {
                Some(source) => {
                    let first = page_param(found, "first", 1).saturating_sub(1);
                    self.reader = Some(Reader::new(source, Some(node), u32::try_from(first).unwrap_or(0)));
                }
                None => self.notice = Some("Connect a PDF to this node to choose its pages.".into()),
            }
        }
    }

    /// The reader window, when open. It closes when its PDF goes.
    pub(super) fn pdf_reader(&mut self, ctx: &egui::Context) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let t = *self.theme.tokens();
        let canvas = self.session.document().canvases().get(&self.canvas);
        let found = canvas.and_then(|canvas| canvas.nodes().get(&reader.node));
        let Some((asset, origin)) = found.and_then(|node| pdf_of(node).map(|asset| (asset.clone(), node.pos))) else {
            self.reader = None;
            return;
        };
        let target = reader.target.and_then(|id| canvas.and_then(|canvas| canvas.nodes().get(&id))).filter(|node| reads_pages(node));
        let range = target.map(|node| (page_param(node, "first", 1), page_param(node, "last", 0)));
        let target_title = target.map(|node| node.title.clone());
        // The count comes from the project file: capped, so a hostile one can't stall the window.
        let pages = asset.meta.page_count.unwrap_or(1).clamp(1, u32::try_from(nodecraft_nodes::document::MAX_PAGE).unwrap_or(u32::MAX));
        reader.go(i64::from(reader.page), pages);
        // Paging keys, unless another widget has the keyboard.
        if ctx.memory(|m| m.focused()).is_none() {
            let page = i64::from(reader.page);
            let to = ctx.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::ArrowLeft) || i.consume_key(Modifiers::NONE, Key::PageUp) {
                    Some(page - 1)
                } else if i.consume_key(Modifiers::NONE, Key::ArrowRight) || i.consume_key(Modifiers::NONE, Key::PageDown) {
                    Some(page + 1)
                } else if i.consume_key(Modifiers::NONE, Key::Home) {
                    Some(0)
                } else if i.consume_key(Modifiers::NONE, Key::End) {
                    Some(i64::from(pages) - 1)
                } else {
                    None
                }
            });
            if let Some(to) = to {
                reader.go(to, pages);
                reader.follow = true;
            }
        }
        let assets = self.session.assets();
        let previews = &mut self.previews;
        let ppp = ctx.pixels_per_point();
        let mut ask = None;
        let mut open = true;
        let title = match pages {
            1 => "PDF".to_owned(),
            _ => format!("PDF, page {} of {pages}", reader.page.saturating_add(1)),
        };
        let top = t.navigation_margin + t.navigation_toolbar_height + t.navigation_gap;
        egui::Window::new(title)
            .id(egui::Id::new("pdf_reader"))
            .open(&mut open)
            .collapsible(false)
            // Opens centred below the toolbar, tall enough to read a page and within the screen.
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + Vec2::new(0.0, top))
            .default_width(t.viewer_width)
            .default_height(t.viewer_width.min(ctx.content_rect().height() - top - t.navigation_margin * 4.0).max(t.reader_thumb_width))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let page = i64::from(reader.page);
                    if ui.add_enabled(reader.page > 0, egui::Button::new("Previous")).clicked() {
                        reader.go(page - 1, pages);
                    }
                    if ui.add_enabled(reader.page.saturating_add(1) < pages, egui::Button::new("Next")).clicked() {
                        reader.go(page + 1, pages);
                    }
                    ui.add(egui::Slider::new(&mut reader.zoom, MIN_ZOOM..=MAX_ZOOM).text("Zoom"));
                    ui.separator();
                    let text = if reader.region.is_some() { "New Text node from selection" } else { "New Text node from page" };
                    if ui.button(text).on_hover_text("Drag over the page to select a part of it").clicked() {
                        ask = Some(Ask::Text);
                    }
                    if ui.button("Extract page").on_hover_text("Copy this page into a new PDF node").clicked() {
                        ask = Some(Ask::Page);
                    }
                });
                if let (Some((first, last)), Some(title)) = (range, &target_title) {
                    let here = u64::from(reader.page) + 1;
                    ui.horizontal_wrapped(|ui| {
                        let shown = if last == 0 { format!("{first} to the end") } else { format!("{first} to {last}") };
                        ui.label(format!("{title} reads pages {shown}"));
                        if ui.button("Only this page").clicked() {
                            ask = Some(Ask::Range(here, here));
                        }
                        if ui.button("Start here").clicked() {
                            ask = Some(Ask::Range(here, if last != 0 && last < here { here } else { last }));
                        }
                        if ui.button("End here").clicked() {
                            ask = Some(Ask::Range(first.min(here), here));
                        }
                    });
                }
                ui.separator();
                // The shown page first, so the thumbnails never hold up the page being read.
                if reader.width > 0.0 {
                    previews.request_page(ctx, asset.hash, reader.page, long_edge(reader.width) * ppp, || assets.preview_read(&asset));
                }
                ui.horizontal_top(|ui| {
                    let height = ui.available_height().max(t.reader_thumb_width);
                    // Every thumbnail takes one slot, a portrait page tall plus its number, so only
                    // the rows in view are drawn and asked for.
                    let slot = Vec2::new(t.reader_thumb_width, long_edge(t.reader_thumb_width));
                    let row = slot.y + ui.spacing().item_spacing.y * 2.0 + ui.text_style_height(&egui::TextStyle::Body);
                    let mut thumbs = egui::ScrollArea::vertical().id_salt("pdf_reader_thumbs").max_height(height);
                    if reader.follow {
                        thumbs = thumbs.vertical_scroll_offset((row + ui.spacing().item_spacing.y) * reader.page as f32);
                    }
                    let rows = usize::try_from(pages).unwrap_or(1);
                    thumbs.show_rows(ui, row, rows, |ui, shown| {
                        ui.vertical(|ui| {
                            ui.set_width(t.reader_thumb_width);
                            for index in shown.filter_map(|index| u32::try_from(index).ok()) {
                                let preview = previews.request_page(ui.ctx(), asset.hash, index, slot.y * ppp, || assets.preview_read(&asset));
                                let (cell, response) = ui.allocate_exact_size(slot, Sense::click());
                                let area = Rect::from_center_size(cell.center(), fit(page_size(&preview, slot.x), slot));
                                card::show_image(ui, area, &preview, None, &t);
                                // An edge, so a white page shows on a light window.
                                ui.painter().rect_stroke(area, 0.0, egui::Stroke::new(1.0, t.card_stroke), egui::StrokeKind::Outside);
                                let current = index == reader.page;
                                if current {
                                    ui.painter().rect_stroke(
                                        area,
                                        0.0,
                                        egui::Stroke::new(t.handle_stroke * 2.0, t.marquee_stroke().color),
                                        egui::StrokeKind::Outside,
                                    );
                                }
                                let response = response.on_hover_text(format!("Page {}", index.saturating_add(1)));
                                if response.clicked() {
                                    reader.go(i64::from(index), pages);
                                }
                                ui.label(format!("{}", index.saturating_add(1)));
                            }
                            reader.follow = false;
                        })
                    });
                    egui::ScrollArea::both().id_salt("pdf_reader_page").max_height(height).show(ui, |ui| {
                        let width = (ui.available_width() * reader.zoom).max(1.0);
                        reader.width = width;
                        let preview = previews.request_page(ui.ctx(), asset.hash, reader.page, long_edge(width) * ppp, || assets.preview_read(&asset));
                        let size = page_size(&preview, width);
                        // A selection is a fraction of the drawn page, so it waits for the page's
                        // real proportions.
                        let ready = matches!(preview, Preview::Ready { .. });
                        let (area, response) = ui.allocate_exact_size(size, if ready { Sense::drag() } else { Sense::hover() });
                        reader.area = ready.then_some(area);
                        card::show_image(ui, area, &preview, None, &t);
                        ui.painter().rect_stroke(area, 0.0, egui::Stroke::new(1.0, t.card_stroke), egui::StrokeKind::Inside);
                        let fraction = |pos: Pos2| {
                            let at = (pos - area.min) / area.size().max(Vec2::splat(1.0));
                            Pos2::new(at.x.clamp(0.0, 1.0), at.y.clamp(0.0, 1.0))
                        };
                        if response.drag_started()
                            && let Some(pos) = response.interact_pointer_pos()
                        {
                            reader.drag = Some(fraction(pos));
                            reader.region = None;
                        }
                        if let (Some(start), Some(pos)) = (reader.drag, response.interact_pointer_pos())
                            && response.dragged()
                        {
                            let end = fraction(pos);
                            reader.region = Some([start.x.min(end.x), start.y.min(end.y), start.x.max(end.x), start.y.max(end.y)]);
                        }
                        if response.drag_stopped() {
                            reader.drag = None;
                            // A click without a real drag clears the selection.
                            if reader.region.is_some_and(|[l, top, r, b]| (r - l) * area.width() < 2.0 || (b - top) * area.height() < 2.0) {
                                reader.region = None;
                            }
                        }
                        if let Some([l, top, r, b]) = reader.region {
                            let shown = Rect::from_min_max(area.lerp_inside(Vec2::new(l, top)), area.lerp_inside(Vec2::new(r, b)));
                            ui.painter().rect(shown, 0.0, t.marquee_fill, t.marquee_stroke(), egui::StrokeKind::Inside);
                        }
                    });
                });
            });
        let (node, page, region) = (reader.node, reader.page, reader.region);
        let target_id = reader.target;
        if !open {
            self.reader = None;
        }
        let Some(ask) = ask else {
            return;
        };
        let document = self.session.document();
        let revision = self.session.revision();
        let at = Position { x: origin.x + t.card_width + t.card_padding * 4.0, y: origin.y };
        let result = match ask {
            // The range it already reads: nothing to change, and no empty undo step.
            Ask::Range(first, last) if range == Some((first, last)) => return,
            Ask::Range(first, last) => match target_id {
                Some(target) => {
                    self.session.dispatch("document.use_pages", &json!({"canvas": self.canvas, "node": target, "first": first, "last": last})).map(|_| None)
                }
                None => return,
            },
            ask => {
                let Some(id) = ids::node_id(document, ("pdf_reader", node, revision, page)) else {
                    self.notice = Some("Couldn't allocate a node id; try again.".into());
                    return;
                };
                match ask {
                    Ask::Page => self.session.dispatch(
                        "document.extract_page",
                        &json!({"canvas":self.canvas,"node":node,
                        "page":u64::from(page).saturating_add(1),"id":id,"at":at}),
                    ),
                    _ => self.session.dispatch(
                        "document.extract_text",
                        &json!({"canvas":self.canvas,"node":node,
                        "page":u64::from(page).saturating_add(1),"id":id,"at":at,"region":region.map(|r|r.map(f64::from))}),
                    ),
                }
                .map(|_| Some(id))
            }
        };
        match result {
            Ok(made) => {
                self.notice = None;
                if let Some(made) = made {
                    self.selection.only(made);
                }
            }
            Err(error) => self.notice = Some(error.message),
        }
    }
}

/// The long edge of a portrait page `width` wide: previews are sized by their long edge.
fn long_edge(width: f32) -> f32 {
    width * 1.414
}

/// `size` scaled down, keeping its proportions, to fit inside `slot`.
fn fit(size: Vec2, slot: Vec2) -> Vec2 {
    let scale = (slot.x / size.x.max(1.0)).min(slot.y / size.y.max(1.0)).min(1.0);
    size * scale
}

/// A page's size at `width` points wide, by the drawn page's proportions (portrait A4-ish while
/// it draws).
fn page_size(preview: &Preview, width: f32) -> Vec2 {
    let ratio = match preview {
        Preview::Ready { size, .. } if size[0] > 0 => size[1] as f32 / size[0] as f32,
        _ => 1.414,
    };
    let height = width * ratio;
    Vec2::new(width, if height.is_finite() { height.clamp(1.0, width.max(1.0) * 8.0) } else { width })
}

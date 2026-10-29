//! Reviewing a batch-fanned result (#92): step through its items one at a time, and, when two
//! batches crossed, see every combination at once as a grid, the first batch's items as rows and
//! the second's as columns. Failed and cancelled items show where they are, and so does a run in
//! progress. Which result and item are shown is view state: nothing here changes the document.
use super::*;
use nodecraft_graph::{Generation, ItemTag, Missing};
use nodecraft_types::ItemId;

/// Longest label shown for one item's value.
const MAX_LABEL_CHARS: usize = 40;
/// Longest text shown for the item being reviewed; the rest is on the card.
const MAX_TEXT_CHARS: usize = 4000;

/// Most images of the shown item drawn at once; the card holds the rest.
const MAX_SHOWN_IMAGES: usize = 64;

/// Which card's items are being reviewed, which item is shown, and the review last built.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct BatchReview {
    pub node: NodeId,
    /// Index into the result's items; kept within them as the result changes.
    pub item: usize,
    /// Built once per result and document revision, not every frame.
    cache: Option<Cached>,
}

impl BatchReview {
    pub(super) fn new(node: NodeId) -> Self {
        Self { node, item: 0, cache: None }
    }

    /// The review last built, to tell whether a frame rebuilt it.
    #[cfg(test)]
    pub(super) fn cached(&self) -> Option<std::sync::Arc<Review>> {
        self.cache.as_ref().and_then(|cached| cached.review.clone())
    }
}

/// A review and what it was built from.
#[derive(Clone, Debug, PartialEq)]
struct Cached {
    canvas: CanvasId,
    generation: Option<nodecraft_types::GenerationId>,
    revision: u64,
    review: Option<std::sync::Arc<Review>>,
}

/// One item of the result being reviewed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ItemState {
    /// It has a value on the card's first output: a short label, and its first image if any.
    Done {
        label: String,
        image: Option<nodecraft_types::AssetRef>,
    },
    Failed,
    Cancelled,
}

/// One batch a result was fanned out by, with the items the result names in the batch's order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Axis {
    pub title: String,
    pub keys: Vec<ItemId>,
    pub labels: Vec<String>,
    /// Where each item is in `keys`.
    index: BTreeMap<ItemId, usize>,
    /// Why the batch's current items couldn't be listed, when they couldn't.
    pub problem: Option<String>,
    /// It pairs by Cross.
    cross: bool,
}

/// A fanned result as the review shows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Review {
    /// The batches it was fanned out by, in the order its item tags name them.
    pub axes: Vec<Axis>,
    /// Each item's place on each axis, and its state, in result order.
    pub items: Vec<(Vec<usize>, ItemState)>,
    /// For a Cross of two batches, the item at each row (first axis) and column (second axis).
    pub grid: Option<Vec<Vec<usize>>>,
}

impl Review {
    /// The labels naming item `index` on each axis.
    pub(crate) fn labels(&self, index: usize) -> Vec<&str> {
        let Some((places, _)) = self.items.get(index) else { return Vec::new() };
        self.axes.iter().zip(places).map(|(axis, place)| axis.labels.get(*place).map_or("", String::as_str)).collect()
    }
}

/// What the shown item has to draw, read from the document for it alone.
enum Shown {
    Text(String),
    Images(Vec<nodecraft_types::AssetRef>, usize),
    Missing,
}

/// Whether `node`'s active result was fanned out by batches, so it has items to review.
pub(crate) fn reviewable(node: &Node) -> bool {
    active(node).is_some_and(|generation| !generation.items.is_empty() && nodecraft_exec::fanout::consistent(&generation.items))
}

fn active(node: &Node) -> Option<&Generation> {
    let id = node.active?;
    node.history.iter().find(|generation| generation.id == id)
}

/// A short, one-line label for a value.
pub(crate) fn short_label(value: &Value) -> String {
    match value {
        Value::Text(text) => clip(text.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim(), MAX_LABEL_CHARS),
        Value::Media(asset) => match asset.mime.as_str().split('/').next() {
            Some("image") => "Image".into(),
            Some("video") => "Video".into(),
            Some("audio") => "Audio".into(),
            Some("model") => "3D model".into(),
            _ => "File".into(),
        },
        Value::List(values) => match values.as_slice() {
            [] => "Nothing".into(),
            [one] => short_label(one),
            [first, rest @ ..] => format!("{} +{}", short_label(first), rest.len()),
        },
        Value::Bool(true) => "Yes".into(),
        Value::Bool(false) => "No".into(),
        other => clip(&serde_json::to_string(other).unwrap_or_default(), MAX_LABEL_CHARS),
    }
}

/// `text` cut to at most `max` characters, on a character boundary, marked when cut.
fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", text.get(..end).unwrap_or("")),
        None => text.to_owned(),
    }
}

/// The value item `index` of `node`'s active result has on its first output.
fn item_value(node: &Node, index: usize) -> Option<&Value> {
    match active(node)?.outputs.first()? {
        Value::List(values) => values.get(index),
        _ => None,
    }
}

/// The review of `node`'s active result, labelled with its batches' current items. `None` when
/// the result wasn't fanned out or its items don't match its outputs.
pub(crate) fn review(canvas: &Canvas, kinds: &nodecraft_nodes::Registry, node: &Node) -> Option<Review> {
    let generation = active(node).filter(|_| reviewable(node))?;
    let Some(Value::List(values)) = generation.outputs.first() else { return None };
    if values.len() != generation.items.len() {
        return None;
    }
    let missing: BTreeMap<usize, Missing> =
        generation.incomplete.iter().flat_map(|incomplete| &incomplete.missing).filter_map(|m| Some((usize::try_from(m.item).ok()?, m.why))).collect();
    let shape: Vec<NodeId> = generation.items.first()?.iter().map(|key| key.batch).collect();
    let axes: Vec<Axis> = shape.iter().enumerate().map(|(index, batch)| axis(canvas, kinds, *batch, index, &generation.items)).collect();
    let items = generation
        .items
        .iter()
        .zip(values)
        .enumerate()
        .map(|(index, (tag, value))| {
            let places = tag.iter().zip(&axes).map(|(key, axis)| axis.index.get(&key.item).copied().unwrap_or(0)).collect();
            let state = match missing.get(&index) {
                Some(Missing::Failed) => ItemState::Failed,
                Some(Missing::Cancelled) => ItemState::Cancelled,
                None => ItemState::Done { label: short_label(value), image: media(value).first().map(|asset| (*asset).clone()) },
            };
            (places, state)
        })
        .collect::<Vec<_>>();
    let grid = grid(&axes, &items);
    Some(Review { axes, items, grid })
}

/// One batch's axis: the items the result names, in the batch's current order, then any it no
/// longer holds, in the order the result names them.
fn axis(canvas: &Canvas, kinds: &nodecraft_nodes::Registry, batch: NodeId, index: usize, tags: &[ItemTag]) -> Axis {
    let (current, problem) = match nodecraft_exec::planner::batch_items(canvas, kinds, batch) {
        Ok(current) => (current, None),
        Err(reason) => (Vec::new(), Some(reason.validation_message().to_owned())),
    };
    let named: BTreeSet<ItemId> = tags.iter().filter_map(|tag| tag.get(index)).map(|key| key.item).collect();
    let mut keys = Vec::new();
    let mut labels = Vec::new();
    let mut at: BTreeMap<ItemId, usize> = BTreeMap::new();
    for (item, value) in &current {
        if !named.contains(item) {
            continue;
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = at.entry(*item) {
            entry.insert(keys.len());
            keys.push(*item);
            labels.push(short_label(value));
        }
    }
    let mut gone = 0usize;
    for key in tags.iter().filter_map(|tag| tag.get(index)) {
        if let std::collections::btree_map::Entry::Vacant(entry) = at.entry(key.item) {
            gone = gone.saturating_add(1);
            entry.insert(keys.len());
            keys.push(key.item);
            // When the batch can't be listed, the window says why once, above.
            labels.push(if problem.is_some() { format!("Item {gone}") } else { format!("Item no longer in the batch ({gone})") });
        }
    }
    let node = canvas.nodes().get(&batch);
    // A batch's own text is its items, so only a title the person gave names it.
    let title = node
        .map(|node| node.title.trim())
        .filter(|title| !title.is_empty())
        .map_or_else(|| format!("Batch {}", index.saturating_add(1)), |title| clip(title, MAX_LABEL_CHARS));
    let cross = node.is_some_and(|node| {
        let pairing = if node.kind == nodecraft_nodes::folder::ID { nodecraft_nodes::folder::pairing(node) } else { nodecraft_nodes::batch::pairing(node) };
        matches!(pairing, Ok(nodecraft_nodes::batch::Pairing::Cross))
    });
    Axis { title, keys, labels, index: at, problem, cross }
}

/// For a Cross of two batches, which item is at each row and column: every combination once.
fn grid(axes: &[Axis], items: &[(Vec<usize>, ItemState)]) -> Option<Vec<Vec<usize>>> {
    let [rows, columns] = axes else { return None };
    if !rows.cross || !columns.cross || rows.keys.len().checked_mul(columns.keys.len())? != items.len() {
        return None;
    }
    let mut grid = vec![vec![None; columns.keys.len()]; rows.keys.len()];
    for (index, (places, _)) in items.iter().enumerate() {
        let [row, column] = places.as_slice() else { return None };
        let cell = grid.get_mut(*row)?.get_mut(*column)?;
        if cell.replace(index).is_some() {
            return None;
        }
    }
    grid.into_iter().map(|row| row.into_iter().collect::<Option<Vec<usize>>>()).collect()
}

impl NodeCraftApp {
    /// The batch items review window, while open.
    pub(super) fn batch_review_window(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some(mut open_review) = self.batch_review.take() else { return };
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else { return };
        let Some(node) = canvas.nodes().get(&open_review.node) else { return };
        let title = crate::card::title(node, self.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor()));
        let generation = node.active;
        let revision = self.session.revision();
        let fresh = open_review.cache.as_ref().is_some_and(|c| c.canvas == self.canvas && c.generation == generation && c.revision == revision);
        if !fresh {
            let review = review(canvas, self.session.kinds(), node).map(std::sync::Arc::new);
            open_review.cache = Some(Cached { canvas: self.canvas, generation, revision, review });
        }
        let review = open_review.cache.as_ref().and_then(|cached| cached.review.clone());
        let count = review.as_ref().map_or(0, |review| review.items.len());
        let item = open_review.item.min(count.saturating_sub(1));
        // Only the shown item's value is read, and only what is drawn of it is kept.
        let shown = match review.as_ref().and_then(|review| review.items.get(item)).map(|(_, state)| state) {
            Some(ItemState::Done { .. }) => match item_value(node, item) {
                Some(value) => {
                    let images = media(value);
                    if images.is_empty() {
                        Shown::Text(match value {
                            Value::Text(text) => clip(text, MAX_TEXT_CHARS),
                            other => short_label(other),
                        })
                    } else {
                        Shown::Images(images.iter().take(MAX_SHOWN_IMAGES).map(|asset| (*asset).clone()).collect(), images.len())
                    }
                }
                None => Shown::Missing,
            },
            _ => Shown::Missing,
        };
        let running = self.runs.as_ref().is_some_and(|runs| runs.is_running(self.canvas, open_review.node));
        let mut open = true;
        let mut pick = None;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("Batch items")
            .id(egui::Id::new("nodecraft.batch_review"))
            .open(&mut open)
            .constrain_to(self.viewport)
            .default_width(t.batch_review_width)
            .collapsible(false)
            .frame(frame)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(&title).strong());
                if running {
                    ui.label(egui::RichText::new("Running: these are the results from before; they update when it finishes.").color(t.accent));
                }
                let Some(review) = &review else {
                    ui.label(egui::RichText::new("This card's result wasn't made per batch item. Run it with a batch wired in.").color(t.text_muted));
                    return;
                };
                for axis in &review.axes {
                    if let Some(problem) = &axis.problem {
                        ui.label(egui::RichText::new(format!("{} can't list its items: {problem}", axis.title)).color(t.text_muted));
                    }
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(item > 0, egui::Button::new("◀ Previous")).clicked() {
                        pick = Some(item.saturating_sub(1));
                    }
                    ui.label(format!("Item {} of {count}", item.saturating_add(1)));
                    if ui.add_enabled(item.saturating_add(1) < count, egui::Button::new("Next ▶")).clicked() {
                        pick = Some(item.saturating_add(1));
                    }
                });
                for (axis, label) in review.axes.iter().zip(review.labels(item)) {
                    ui.label(format!("{}: {label}", axis.title));
                }
                if let Some((_, state)) = review.items.get(item) {
                    self.review_item(ui, state, &shown, t);
                }
                if let Some(grid) = &review.grid {
                    ui.separator();
                    self.review_grid(ui, review, grid, item, &mut pick, t);
                } else if review.axes.len() > 2 {
                    ui.label(egui::RichText::new("The grid shows two batches crossed; step through the items to see every one.").color(t.text_muted));
                }
            });
        if open {
            if let Some(item) = pick {
                open_review.item = item;
            }
            self.batch_review = Some(open_review);
        }
    }

    /// The item being reviewed: its text, its images, or why it has none.
    fn review_item(&mut self, ui: &mut egui::Ui, state: &ItemState, shown: &Shown, t: &Tokens) {
        match (state, shown) {
            (ItemState::Failed, _) => {
                ui.label(egui::RichText::new("Failed. Run the card again to retry the items that didn't finish.").color(t.danger));
            }
            (ItemState::Cancelled, _) => {
                ui.label(egui::RichText::new("Cancelled. Run the card again to finish it.").color(t.text_muted));
            }
            (ItemState::Done { .. }, Shown::Text(text)) => {
                egui::ScrollArea::vertical().id_salt("batch_review_text").max_height(t.card_preview_height).show(ui, |ui| {
                    ui.label(text);
                });
            }
            (ItemState::Done { .. }, Shown::Images(images, total)) => {
                egui::ScrollArea::vertical().id_salt("batch_review_images").max_height(t.card_preview_height).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for asset in images {
                            self.review_image(ui, asset, t.card_preview_height, t);
                        }
                    });
                });
                if *total > images.len() {
                    ui.label(egui::RichText::new(format!("{} more images on the card", total.saturating_sub(images.len()))).color(t.text_muted));
                }
            }
            (ItemState::Done { .. }, Shown::Missing) => {}
        }
    }

    /// Every combination of two crossed batches; clicking one shows it above. Only the rows on
    /// screen are laid out, and only the cells on screen are drawn.
    fn review_grid(&mut self, ui: &mut egui::Ui, review: &Review, grid: &[Vec<usize>], shown: usize, pick: &mut Option<usize>, t: &Tokens) {
        let (Some(rows), Some(columns)) = (review.axes.first(), review.axes.get(1)) else { return };
        ui.label(egui::RichText::new(format!("{} × {}", rows.title, columns.title)).color(t.text_muted));
        let images = review.items.iter().any(|(_, state)| matches!(state, ItemState::Done { image: Some(_), .. }));
        let height = if images { t.batch_review_cell } else { t.control_row_height };
        let size = Vec2::new(t.batch_review_cell, height);
        let label = |ui: &mut egui::Ui, text: &str| {
            ui.add_sized(size, egui::Label::new(egui::RichText::new(text).strong()).truncate());
        };
        egui::ScrollArea::both().id_salt("batch_review_grid").max_height(t.batch_review_cell * 4.0).show_rows(
            ui,
            height,
            grid.len().saturating_add(1),
            |ui, range| {
                for row in range {
                    ui.horizontal(|ui| {
                        // The first row names the columns.
                        let Some(cells) = row.checked_sub(1).and_then(|row| grid.get(row)) else {
                            ui.allocate_space(size);
                            for text in &columns.labels {
                                label(ui, text);
                            }
                            return;
                        };
                        label(ui, rows.labels.get(row.saturating_sub(1)).map_or("", String::as_str));
                        for index in cells {
                            let next = Rect::from_min_size(ui.cursor().min, size);
                            match review.items.get(*index) {
                                Some((_, state)) if ui.is_rect_visible(next) => {
                                    if self.review_cell(ui, state, size, *index == shown, t).clicked() {
                                        *pick = Some(*index);
                                    }
                                }
                                _ => {
                                    ui.allocate_space(size);
                                }
                            }
                        }
                    });
                }
            },
        );
    }

    /// One cell of the grid: the item's first image, its text, or its state.
    fn review_cell(&mut self, ui: &mut egui::Ui, state: &ItemState, size: Vec2, selected: bool, t: &Tokens) -> egui::Response {
        let text = match state {
            ItemState::Failed => egui::RichText::new("Failed").color(t.danger),
            ItemState::Cancelled => egui::RichText::new("Cancelled").color(t.text_muted),
            ItemState::Done { label, image: Some(asset) } => {
                let response = ui.allocate_response(size, Sense::click());
                let preview = self.request_review_preview(ui, asset, size.x.min(size.y));
                crate::card::show_image(ui, response.rect, &preview, None, t);
                if selected {
                    ui.painter().rect_stroke(response.rect, t.card_radius, egui::Stroke::new(t.handle_stroke, t.accent), egui::StrokeKind::Inside);
                }
                return response.on_hover_text(label.as_str());
            }
            ItemState::Done { label, image: None } => egui::RichText::new(label.as_str()),
        };
        ui.add_sized(size, egui::Button::selectable(selected, text))
    }

    fn review_image(&mut self, ui: &mut egui::Ui, asset: &nodecraft_types::AssetRef, edge: f32, t: &Tokens) {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(edge), Sense::hover());
        if ui.is_rect_visible(rect) {
            let preview = self.request_review_preview(ui, asset, edge);
            crate::card::show_image(ui, rect, &preview, None, t);
        }
    }

    fn request_review_preview(&mut self, ui: &egui::Ui, asset: &nodecraft_types::AssetRef, edge: f32) -> crate::preview::Preview {
        let assets = self.session.assets();
        self.previews.request_asset(ui.ctx(), asset.hash, edge * ui.ctx().pixels_per_point(), || assets.preview_read(asset))
    }
}

/// The images of one item's value: itself, or those in its list.
fn media(value: &Value) -> Vec<&nodecraft_types::AssetRef> {
    fn image(value: &Value) -> Option<&nodecraft_types::AssetRef> {
        match value {
            Value::Media(asset) if asset.mime.as_str().starts_with("image/") => Some(asset),
            _ => None,
        }
    }
    match value {
        Value::List(values) => values.iter().filter_map(image).collect(),
        value => image(value).into_iter().collect(),
    }
}

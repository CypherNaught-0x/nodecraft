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

/// Which card's items are being reviewed, and which item is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BatchReview {
    pub node: NodeId,
    /// Index into the result's items; kept within them as the result changes.
    pub item: usize,
}

/// One item of the result being reviewed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ItemState {
    /// What the item made on the card's first output.
    Done(Value),
    Failed,
    Cancelled,
}

/// One batch a result was fanned out by, with the items the result names in the batch's order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Axis {
    pub title: String,
    pub keys: Vec<ItemId>,
    pub labels: Vec<String>,
}

/// A fanned result as the review shows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Review {
    /// The batches it was fanned out by, in the order its item tags name them.
    pub axes: Vec<Axis>,
    /// Each item's label on each axis, and its state, in result order.
    pub items: Vec<(Vec<String>, ItemState)>,
    /// For a Cross of two batches, the item at each row (first axis) and column (second axis).
    pub grid: Option<Vec<Vec<usize>>>,
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
    let label = |index: usize, item: ItemId| -> String {
        axes.get(index).and_then(|axis| axis.keys.iter().position(|key| *key == item).and_then(|at| axis.labels.get(at))).cloned().unwrap_or_default()
    };
    let items = generation
        .items
        .iter()
        .zip(values)
        .enumerate()
        .map(|(index, (tag, value))| {
            let labels = tag.iter().enumerate().map(|(axis, key)| label(axis, key.item)).collect();
            let state = match missing.get(&index) {
                Some(Missing::Failed) => ItemState::Failed,
                Some(Missing::Cancelled) => ItemState::Cancelled,
                None => ItemState::Done(value.clone()),
            };
            (labels, state)
        })
        .collect();
    let grid = grid(&axes, &generation.items);
    Some(Review { axes, items, grid })
}

/// One batch's axis: the items the result names, in the batch's current order, then any it no
/// longer holds, in the order the result names them.
fn axis(canvas: &Canvas, kinds: &nodecraft_nodes::Registry, batch: NodeId, index: usize, tags: &[ItemTag]) -> Axis {
    let current = nodecraft_exec::planner::batch_items(canvas, kinds, batch).unwrap_or_default();
    let named: BTreeSet<ItemId> = tags.iter().filter_map(|tag| tag.get(index)).map(|key| key.item).collect();
    let mut keys = Vec::new();
    let mut labels = Vec::new();
    for (item, value) in &current {
        if named.contains(item) && !keys.contains(item) {
            keys.push(*item);
            labels.push(short_label(value));
        }
    }
    let mut gone = 0usize;
    for key in tags.iter().filter_map(|tag| tag.get(index)) {
        if !keys.contains(&key.item) {
            gone = gone.saturating_add(1);
            keys.push(key.item);
            labels.push(format!("Item no longer in the batch ({gone})"));
        }
    }
    // A batch's own text is its items, so only a title the person gave names it.
    let title = canvas
        .nodes()
        .get(&batch)
        .map(|node| node.title.trim())
        .filter(|title| !title.is_empty())
        .map_or_else(|| format!("Batch {}", index.saturating_add(1)), |title| clip(title, MAX_LABEL_CHARS));
    Axis { title, keys, labels }
}

/// For a Cross of two batches, which item is at each row and column: every combination once.
fn grid(axes: &[Axis], tags: &[ItemTag]) -> Option<Vec<Vec<usize>>> {
    let [rows, columns] = axes else { return None };
    if rows.keys.len().checked_mul(columns.keys.len())? != tags.len() {
        return None;
    }
    let mut grid = vec![vec![None; columns.keys.len()]; rows.keys.len()];
    for (index, tag) in tags.iter().enumerate() {
        let [row, column] = tag.as_slice() else { return None };
        let row = rows.keys.iter().position(|key| *key == row.item)?;
        let column = columns.keys.iter().position(|key| *key == column.item)?;
        let cell = grid.get_mut(row)?.get_mut(column)?;
        if cell.replace(index).is_some() {
            return None;
        }
    }
    grid.into_iter().map(|row| row.into_iter().collect::<Option<Vec<usize>>>()).collect()
}

impl NodeCraftApp {
    /// The batch items review window, while open.
    pub(super) fn batch_review_window(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some(open_review) = self.batch_review.clone() else { return };
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else {
            self.batch_review = None;
            return;
        };
        let Some(node) = canvas.nodes().get(&open_review.node) else {
            self.batch_review = None;
            return;
        };
        let title = crate::card::title(node, self.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor()));
        let review = review(canvas, self.session.kinds(), node);
        let running = self.runs.as_ref().is_some_and(|runs| runs.is_running(self.canvas, node.id));
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
                let count = review.items.len();
                let item = open_review.item.min(count.saturating_sub(1));
                ui.horizontal(|ui| {
                    if ui.add_enabled(item > 0, egui::Button::new("◀ Previous")).clicked() {
                        pick = Some(item.saturating_sub(1));
                    }
                    ui.label(format!("Item {} of {count}", item.saturating_add(1)));
                    if ui.add_enabled(item.saturating_add(1) < count, egui::Button::new("Next ▶")).clicked() {
                        pick = Some(item.saturating_add(1));
                    }
                });
                if let Some((labels, state)) = review.items.get(item) {
                    for (axis, label) in review.axes.iter().zip(labels) {
                        ui.label(format!("{}: {label}", axis.title));
                    }
                    self.review_item(ui, state, t);
                }
                if let Some(grid) = &review.grid {
                    ui.separator();
                    self.review_grid(ui, review, grid, item, &mut pick, t);
                } else if review.axes.len() > 2 {
                    ui.label(egui::RichText::new("The grid shows two batches crossed; step through the items to see every one.").color(t.text_muted));
                }
            });
        self.batch_review = match (open, pick) {
            (false, _) => None,
            (true, Some(item)) => Some(BatchReview { item, ..open_review }),
            (true, None) => Some(open_review),
        };
    }

    /// The item being reviewed: its text, its images, or why it has none.
    fn review_item(&mut self, ui: &mut egui::Ui, state: &ItemState, t: &Tokens) {
        match state {
            ItemState::Failed => {
                ui.label(egui::RichText::new("Failed. Run the card again to retry the items that didn't finish.").color(t.danger));
            }
            ItemState::Cancelled => {
                ui.label(egui::RichText::new("Cancelled. Run the card again to finish it.").color(t.text_muted));
            }
            ItemState::Done(value) => {
                let images = media(value);
                if images.is_empty() {
                    let text = match value {
                        Value::Text(text) => clip(text, MAX_TEXT_CHARS),
                        other => short_label(other),
                    };
                    egui::ScrollArea::vertical().id_salt("batch_review_text").max_height(t.card_preview_height).show(ui, |ui| {
                        ui.label(text);
                    });
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for asset in images {
                            self.review_image(ui, asset, t.card_preview_height, t);
                        }
                    });
                }
            }
        }
    }

    /// Every combination of two crossed batches; clicking one shows it above.
    fn review_grid(&mut self, ui: &mut egui::Ui, review: &Review, grid: &[Vec<usize>], shown: usize, pick: &mut Option<usize>, t: &Tokens) {
        let (Some(rows), Some(columns)) = (review.axes.first(), review.axes.get(1)) else { return };
        ui.label(egui::RichText::new(format!("{} × {}", rows.title, columns.title)).color(t.text_muted));
        egui::ScrollArea::both().id_salt("batch_review_grid").max_height(t.batch_review_cell * 4.0).show(ui, |ui| {
            egui::Grid::new("batch_review_cells").spacing([t.control_gap, t.control_gap]).show(ui, |ui| {
                ui.label("");
                for label in &columns.labels {
                    ui.label(egui::RichText::new(label).strong());
                }
                ui.end_row();
                for (label, cells) in rows.labels.iter().zip(grid) {
                    ui.label(egui::RichText::new(label).strong());
                    for index in cells {
                        let Some((_, state)) = review.items.get(*index) else {
                            ui.label("");
                            continue;
                        };
                        if self.review_cell(ui, state, *index == shown, t).clicked() {
                            *pick = Some(*index);
                        }
                    }
                    ui.end_row();
                }
            });
        });
    }

    /// One cell of the grid: the item's first image, its text, or its state.
    fn review_cell(&mut self, ui: &mut egui::Ui, state: &ItemState, selected: bool, t: &Tokens) -> egui::Response {
        let text = match state {
            ItemState::Failed => egui::RichText::new("Failed").color(t.danger),
            ItemState::Cancelled => egui::RichText::new("Cancelled").color(t.text_muted),
            ItemState::Done(value) => {
                if let Some(asset) = media(value).first().copied() {
                    let response = ui.allocate_response(Vec2::splat(t.batch_review_cell), Sense::click());
                    if ui.is_rect_visible(response.rect) {
                        let preview = self.request_review_preview(ui, asset, t.batch_review_cell);
                        crate::card::show_image(ui, response.rect, &preview, None, t);
                        if selected {
                            ui.painter().rect_stroke(response.rect, t.card_radius, egui::Stroke::new(t.handle_stroke, t.accent), egui::StrokeKind::Inside);
                        }
                    }
                    return response.on_hover_text(short_label(value));
                }
                egui::RichText::new(short_label(value))
            }
        };
        ui.add_sized(Vec2::new(t.batch_review_cell, t.control_row_height), egui::Button::selectable(selected, text))
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

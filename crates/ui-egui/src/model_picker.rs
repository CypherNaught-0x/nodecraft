//! Catalog model-choice presentation shared by a card and a bulk selection.
//!
//! This module never edits a node or authorizes a run. The controller retains the immutable
//! catalog snapshot, supplies role-specific compatibility, and previews/commits a choice through
//! the engine's shared model-swap transaction. Catalog rates are metadata, not launch consent.
use crate::theme::Tokens;
use nodecraft_catalog::{
    Catalog, ModelDescriptor, Wiring,
    model::{MAX_TEXT, SELECTION_TIER_HELP},
    pricing::Unit,
};
use nodecraft_types::{CanvasId, Error, ErrorKind, Modality, Money, NodeId};

const QUERY_CHARS: usize = 128;

struct Choice {
    id: String,
    unavailable: Option<String>,
}
#[derive(Clone, Copy)]
enum Row {
    Provider(usize),
    Model(usize),
    /// The ComfyUI server section's heading, with its Refresh button (#970).
    Server,
    /// A target checkpoint the server doesn't list.
    Missing(usize),
    ServerGroup(usize),
    ServerModel(usize, usize),
    Workflows,
    Workflow(usize),
}

/// One model a ComfyUI server lists (#970), as the controller read it from `comfy.models.list`,
/// with the compatibility it evaluated once when opening. The picker hardcodes none of it.
pub struct ServerEntry {
    pub group: String,
    pub name: String,
    /// What choosing it does, e.g. which built-in workflow runs it.
    pub detail: String,
    /// Why it can't be chosen for these cards, shown with it.
    pub unavailable: Option<String>,
}

/// One type of model the server lists.
pub struct ServerGroup {
    pub label: String,
    pub entries: Vec<ServerEntry>,
    /// Entries the server listed past the listing's limits.
    pub more: usize,
}

/// One imported workflow card in the document.
pub struct ServerWorkflow {
    pub canvas: CanvasId,
    pub id: NodeId,
    pub title: String,
    pub unavailable: Option<String>,
}

/// What the default ComfyUI server's last read lists, beside the catalog's choices.
pub struct ServerSection {
    /// The server's name, when it was read through a named server (#864).
    pub server: Option<String>,
    /// Whether the server's models were read at all.
    pub read: bool,
    /// Whether this host can read the server again.
    pub can_refresh: bool,
    /// Why the server's list couldn't be read, shown in the heading row.
    pub error: Option<String>,
    /// The targets' checkpoints the server doesn't list.
    pub missing: Vec<String>,
    pub groups: Vec<ServerGroup>,
    pub workflows: Vec<ServerWorkflow>,
}

impl ServerSection {
    fn text_bytes(&self) -> usize {
        let groups = self.groups.iter().flat_map(|g| g.entries.iter()).map(|e| e.name.len() + e.detail.len() + e.unavailable.as_ref().map_or(0, String::len));
        let workflows = self.workflows.iter().map(|w| w.title.len() + w.unavailable.as_ref().map_or(0, String::len));
        groups.chain(workflows).chain(self.missing.iter().map(String::len)).fold(0usize, usize::saturating_add)
    }
    /// Whether the section shows its heading row (and so its entries).
    fn shown(&self) -> bool {
        self.read || self.can_refresh || self.error.is_some()
    }
    fn heading(&self) -> String {
        match &self.server {
            Some(name) => format!("From ComfyUI server \"{name}\""),
            None => "From your ComfyUI server".into(),
        }
    }
}

/// An open picker retains IDs/reasons and indices, not another model/schema inventory.
/// Its caller must use the same immutable catalog until closing or explicitly rebuilding it.
pub struct ModelPicker {
    choices: Vec<Choice>,
    filtered: Vec<usize>,
    rows: Vec<Row>,
    query: String,
    filtered_query: String,
    highlighted: Option<usize>,
    focus: bool,
    text_bytes: usize,
    server: Option<ServerSection>,
    /// The server entries' and workflows' names in lowercase, computed once for search.
    server_search: Vec<Vec<String>>,
    workflow_search: Vec<String>,
    /// The highlighted server row, by its index in `rows`; never set with `highlighted`.
    server_highlight: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Open,
    Closed,
    /// The caller must obtain a migration preview; this is not permission to change a node.
    Chosen(String),
    /// The controller supplied a different catalog; preserve drafts and explicitly refresh.
    CatalogChanged,
    /// A choice could not be retained; the caller shows this without changing the document.
    Unavailable(&'static str),
    /// A model the ComfyUI server lists (#970): the caller previews it through the engine.
    ServerModel {
        group: String,
        name: String,
    },
    /// An imported workflow card: the caller places a copy beside the targets.
    Workflow {
        canvas: CanvasId,
        id: NodeId,
    },
    /// Read the default ComfyUI server again.
    RefreshServer,
}

fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn allocation() -> Error {
    Error::new(ErrorKind::Internal, "model picker storage could not be reserved")
}
fn copy(text: &str) -> Result<String, Error> {
    let mut result = String::new();
    result.try_reserve_exact(text.len()).map_err(|_| allocation())?;
    result.push_str(text);
    Ok(result)
}

impl ModelPicker {
    /// Evaluate compatibility once, when opening. `additional` supplies the engine's actual
    /// role/mode/port checks after the catalog's coarse modality-count filter succeeds.
    /// A failure stays visible; its public-safe reason must meet the catalog text bound.
    pub fn new(catalog: &Catalog, wiring: &Wiring, mut additional: impl FnMut(&ModelDescriptor) -> Result<(), String>) -> Result<Self, Error> {
        Self::for_targets(catalog, |model| wiring.fit(model).map(|_| ()).map_err(|reason| reason.message()).and_then(|()| additional(model)))
    }

    /// Production target controllers supply the actual role/collection predicate. They must
    /// not count several ordered text wires as several provider prompts when the kind composes
    /// those wires into one prompt. Compatibility is evaluated once, never during idle paint.
    pub fn for_targets(catalog: &Catalog, mut compatibility: impl FnMut(&ModelDescriptor) -> Result<(), String>) -> Result<Self, Error> {
        let mut choices = Vec::new();
        choices.try_reserve_exact(catalog.len()).map_err(|_| allocation())?;
        let mut text_bytes = 0usize;
        for model in catalog.card_models() {
            let unavailable = compatibility(model).err();
            if unavailable.as_ref().is_some_and(|reason| reason.len() > MAX_TEXT) {
                return Err(invalid("model compatibility reason exceeds catalog text limit"));
            }
            text_bytes = text_bytes
                .checked_add(model.id.len())
                .and_then(|n| n.checked_add(unavailable.as_ref().map_or(0, String::len)))
                .ok_or_else(|| invalid("model picker text size overflow"))?;
            choices.push(Choice { id: copy(&model.id)?, unavailable });
        }
        choices.sort_by(|a, b| match (catalog.get(&a.id), catalog.get(&b.id)) {
            (Some(a_model), Some(b_model)) => {
                (a_model.provider.as_str(), a.unavailable.is_some(), std::cmp::Reverse(a_model.quality), a_model.label.as_str(), a.id.as_str()).cmp(&(
                    b_model.provider.as_str(),
                    b.unavailable.is_some(),
                    std::cmp::Reverse(b_model.quality),
                    b_model.label.as_str(),
                    b.id.as_str(),
                ))
            }
            _ => a.id.cmp(&b.id),
        });
        let mut filtered = Vec::new();
        filtered.try_reserve_exact(choices.len()).map_err(|_| allocation())?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(choices.len().checked_mul(2).ok_or_else(|| invalid("model picker row size overflow"))?).map_err(|_| allocation())?;
        let mut picker = Self {
            choices,
            filtered,
            rows,
            query: String::new(),
            filtered_query: String::new(),
            highlighted: None,
            focus: true,
            text_bytes,
            server: None,
            server_search: Vec::new(),
            workflow_search: Vec::new(),
            server_highlight: None,
        };
        picker.refilter(catalog);
        Ok(picker)
    }

    /// Adds the ComfyUI server's own models and the document's imported workflows (#970).
    pub fn with_server(mut self, server: ServerSection, catalog: &Catalog) -> Self {
        self.text_bytes = self.text_bytes.saturating_add(server.text_bytes());
        self.server_search = server.groups.iter().map(|g| g.entries.iter().map(|e| e.name.to_lowercase()).collect()).collect();
        self.workflow_search = server.workflows.iter().map(|w| w.title.to_lowercase()).collect();
        self.server = Some(server);
        self.refilter(catalog);
        self
    }

    /// The current search text, kept when the controller rebuilds the picker.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Retained ID/reason payload lengths only; excludes vector shells/capacity, the borrowed
    /// catalog, bounded query/filter scratch, egui storage and allocator/process overhead.
    pub fn retained_text_bytes(&self) -> usize {
        self.text_bytes
    }

    pub fn set_query(&mut self, query: &str) -> Result<(), Error> {
        if query.chars().take(QUERY_CHARS.saturating_add(1)).count() > QUERY_CHARS {
            return Err(invalid("model search exceeds 128 characters"));
        }
        self.query = copy(query)?;
        Ok(())
    }

    fn refilter(&mut self, catalog: &Catalog) {
        self.filtered.clear();
        self.rows.clear();
        let query = self.query.trim().to_lowercase();
        let mut provider = None;
        for (index, choice) in self.choices.iter().enumerate() {
            let Some(model) = catalog.get(&choice.id) else { continue };
            if !query.is_empty() && !model.id.contains(&query) && !model.provider.contains(&query) && !model.label.to_lowercase().contains(&query) {
                continue;
            }
            if provider != Some(model.provider.as_str()) {
                self.rows.push(Row::Provider(index));
                provider = Some(&model.provider);
            }
            self.filtered.push(index);
            self.rows.push(Row::Model(index));
        }
        self.highlighted = self.filtered.iter().copied().find(|index| self.choices.get(*index).is_some_and(|choice| choice.unavailable.is_none()));
        self.filtered_query.clone_from(&self.query);
        self.server_highlight = None;
        let Some(server) = &self.server else { return };
        let matches = |lower: &str| query.is_empty() || lower.contains(&query);
        let mut rows: Vec<Row> = (0..server.missing.len()).map(Row::Missing).collect();
        for (g, group) in server.groups.iter().enumerate() {
            let names = self.server_search.get(g).map(Vec::as_slice).unwrap_or_default();
            let label = matches(&group.label.to_lowercase());
            let entries: Vec<usize> = (0..group.entries.len()).filter(|i| label || names.get(*i).is_some_and(|name| matches(name))).collect();
            if entries.is_empty() {
                continue;
            }
            rows.push(Row::ServerGroup(g));
            rows.extend(entries.into_iter().map(|e| Row::ServerModel(g, e)));
        }
        let workflows: Vec<usize> =
            (0..server.workflows.len()).filter(|i| matches("workflow") || self.workflow_search.get(*i).is_some_and(|title| matches(title))).collect();
        let mut section = Vec::new();
        if server.shown() {
            section.push(Row::Server);
            section.extend(rows);
        }
        if !workflows.is_empty() {
            section.push(Row::Workflows);
            section.extend(workflows.into_iter().map(Row::Workflow));
        }
        // The server's own models sit right after the catalog's ComfyUI cards, as one group.
        let comfy = |row: &Row| match row {
            Row::Model(index) => {
                self.choices.get(*index).and_then(|choice| catalog.get(&choice.id)).is_some_and(|m| m.provider == nodecraft_nodes::comfy_workflow::PROVIDER)
            }
            _ => false,
        };
        let at = self.rows.iter().rposition(comfy).map_or(self.rows.len(), |last| last.saturating_add(1));
        let tail = self.rows.split_off(at.min(self.rows.len()));
        self.rows.extend(section);
        self.rows.extend(tail);
        if self.highlighted.is_none() {
            self.server_highlight = self.rows.iter().position(|row| self.server_enabled(*row));
        }
    }

    /// Whether `row` is a server entry or workflow that can be chosen.
    fn server_enabled(&self, row: Row) -> bool {
        let Some(server) = &self.server else { return false };
        match row {
            Row::ServerModel(g, e) => server.groups.get(g).and_then(|group| group.entries.get(e)).is_some_and(|entry| entry.unavailable.is_none()),
            Row::Workflow(index) => server.workflows.get(index).is_some_and(|workflow| workflow.unavailable.is_none()),
            _ => false,
        }
    }

    /// What choosing the server row `row` asks for.
    fn server_outcome(&self, row: Row) -> Option<Outcome> {
        let server = self.server.as_ref().filter(|_| self.server_enabled(row))?;
        match row {
            Row::ServerModel(g, e) => {
                let entry = server.groups.get(g)?.entries.get(e)?;
                Some(Outcome::ServerModel { group: entry.group.clone(), name: entry.name.clone() })
            }
            Row::Workflow(index) => server.workflows.get(index).map(|workflow| Outcome::Workflow { canvas: workflow.canvas, id: workflow.id }),
            _ => None,
        }
    }

    /// The highlighted row's index in `rows`, catalog or server.
    fn highlighted_row(&self) -> Option<usize> {
        self.server_highlight.or_else(|| self.highlighted.and_then(|h| self.rows.iter().position(|row| matches!(row, Row::Model(index) if *index == h))))
    }

    /// Moves through every row that can be chosen, catalog and server alike.
    fn move_highlight(&mut self, down: bool) {
        let current = self.highlighted_row();
        let enabled = |index: &usize| match self.rows.get(*index) {
            Some(Row::Model(choice)) => self.choices.get(*choice).is_some_and(|choice| choice.unavailable.is_none()),
            Some(row) => self.server_enabled(*row),
            None => false,
        };
        let next = if down {
            (current.map_or(0, |n| n.saturating_add(1))..self.rows.len()).find(enabled)
        } else {
            (0..current.unwrap_or(self.rows.len())).rev().find(enabled)
        };
        match next.and_then(|index| self.rows.get(index).copied().map(|row| (index, row))) {
            Some((_, Row::Model(choice))) => {
                self.highlighted = Some(choice);
                self.server_highlight = None;
            }
            Some((index, _)) => {
                self.highlighted = None;
                self.server_highlight = Some(index);
            }
            None => {}
        }
    }

    /// Draw only visible rows. Search changes rebuild bounded indices; an idle frame does not
    /// re-evaluate compatibility or clone/flatten the catalog's model schemas.
    pub fn show(&mut self, ctx: &egui::Context, catalog: &Catalog, t: &Tokens) -> Outcome {
        let (up, down, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if escape {
            return Outcome::Closed;
        }
        let mut open = true;
        let mut chosen = None;
        let mut server_chosen = None;
        let mut changed_catalog = false;
        egui::Window::new("Choose a model")
            .id(egui::Id::new("model_picker157"))
            .order(egui::Order::Foreground)
            .open(&mut open)
            .collapsible(false)
            .default_width(t.dialog_width)
            .show(ctx, |ui| {
                let search = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search models or providers…").char_limit(QUERY_CHARS));
                if self.focus {
                    search.request_focus();
                    self.focus = false;
                }
                if self.filtered_query != self.query {
                    self.refilter(catalog);
                }
                if up {
                    self.move_highlight(false);
                }
                if down {
                    self.move_highlight(true);
                }
                ui.label(egui::RichText::new("Model rates only. Run quotes and budgets are checked separately.").color(t.text_muted));
                ui.label(egui::RichText::new(SELECTION_TIER_HELP).color(t.text_muted));
                if self.rows.is_empty() {
                    ui.label("No matching models");
                }
                // One choice control and three bounded text lines, also at larger text sizes.
                let row_height =
                    t.control_row_height.max(ui.spacing().interact_size.y) + 3.0 * (ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y);
                let mut scroll = egui::ScrollArea::vertical().max_height(t.comments_height);
                if (up || down)
                    && let Some(row) = self.highlighted_row()
                {
                    scroll = scroll.vertical_scroll_offset(row as f32 * (row_height + ui.spacing().item_spacing.y));
                }
                scroll.show_rows(ui, row_height, self.rows.len(), |ui, range| {
                    let first = range.start;
                    for (offset, row) in self.rows.get(range).into_iter().flatten().enumerate() {
                        let at = first.saturating_add(offset);
                        let index = match row {
                            Row::Provider(index) | Row::Model(index) => *index,
                            _ => {
                                if let Some(server) = &self.server {
                                    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), row_height), egui::Layout::top_down(egui::Align::Min), |ui| {
                                        if let Some(outcome) = server_row(ui, server, *row, self.server_highlight == Some(at), t) {
                                            server_chosen = Some(outcome);
                                        }
                                    });
                                }
                                continue;
                            }
                        };
                        let Some(choice) = self.choices.get(index) else { continue };
                        let Some(model) = catalog.get(&choice.id) else {
                            changed_catalog = true;
                            continue;
                        };
                        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), row_height), egui::Layout::top_down(egui::Align::Min), |ui| {
                            if matches!(row, Row::Provider(_)) {
                                ui.heading(&model.provider);
                                return;
                            }
                            let response = ui
                                .add_enabled(choice.unavailable.is_none(), egui::Button::selectable(self.highlighted == Some(index), &model.label).truncate());
                            ui.add(egui::Label::new(&model.description).truncate()).on_hover_text(&model.description);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("Selection tier {}/5 · Time hint {} s", model.quality, model.latency_s)).color(t.text_muted),
                                )
                                .truncate(),
                            )
                            .on_hover_text("The time hint is configured catalog metadata. Actual waiting time depends on the request and provider load.");
                            ui.add(egui::Label::new(egui::RichText::new(price_label(model)).color(t.text_muted)).truncate());
                            if let Some(reason) = &choice.unavailable {
                                response.on_disabled_hover_ui(|ui| {
                                    ui.label(reason);
                                    model_details(ui, model);
                                });
                            } else {
                                if response.clicked() {
                                    chosen = Some(index);
                                }
                                response.on_hover_ui(|ui| model_details(ui, model));
                            }
                        });
                    }
                });
            });
        if changed_catalog {
            return Outcome::CatalogChanged;
        }
        if !open {
            return Outcome::Closed;
        }
        if let Some(outcome) = server_chosen.or_else(|| {
            (enter && chosen.is_none())
                .then(|| self.server_highlight.and_then(|at| self.rows.get(at).copied()))
                .flatten()
                .and_then(|row| self.server_outcome(row))
        }) {
            return outcome;
        }
        if let Some(index) = chosen.or(enter.then_some(self.highlighted).flatten())
            && let Some(choice) = self.choices.get(index).filter(|choice| choice.unavailable.is_none())
        {
            if catalog.get(&choice.id).is_none() {
                return Outcome::CatalogChanged;
            }
            return match copy(&choice.id) {
                Ok(id) => Outcome::Chosen(id),
                Err(_) => Outcome::Unavailable("The model choice could not be retained. Try again."),
            };
        }
        Outcome::Open
    }
}

/// Draws one row of the ComfyUI server section; what the person chose there, if anything.
fn server_row(ui: &mut egui::Ui, server: &ServerSection, row: Row, highlighted: bool, t: &Tokens) -> Option<Outcome> {
    let muted = |text: &str| egui::Label::new(egui::RichText::new(text).color(t.text_muted)).truncate();
    match row {
        Row::Server => {
            ui.heading(server.heading());
            if let Some(error) = &server.error {
                ui.add(egui::Label::new(egui::RichText::new(format!("Couldn't list the server's models: {error}")).color(t.danger)).truncate())
                    .on_hover_text(error);
                if server.can_refresh && ui.button("Refresh server models").clicked() {
                    return Some(Outcome::RefreshServer);
                }
                return None;
            }
            let note = if server.read {
                "Listed from the server's last read, not from the catalog. Free: runs on your own server."
            } else {
                "Read the server to list its own checkpoints, LoRAs and other models here."
            };
            ui.add(muted(note));
            if server.can_refresh && ui.button("Refresh server models").clicked() {
                return Some(Outcome::RefreshServer);
            }
        }
        Row::Missing(index) => {
            let name = server.missing.get(index)?;
            ui.add(egui::Label::new(egui::RichText::new(format!("Missing on the server: {name}")).color(t.stale)).truncate()).on_hover_text(name);
            ui.add(muted("The card keeps this checkpoint and won't run it until the server has it. Install it, or choose another."));
        }
        Row::ServerGroup(g) => {
            let group = server.groups.get(g)?;
            ui.strong(&group.label);
            if group.more > 0 {
                ui.add(muted(&format!("{} more not listed", group.more)));
            }
        }
        Row::ServerModel(g, e) => {
            let entry = server.groups.get(g)?.entries.get(e)?;
            let response = ui.add_enabled(entry.unavailable.is_none(), egui::Button::selectable(highlighted, &entry.name).truncate());
            match &entry.unavailable {
                Some(reason) => {
                    ui.add(muted(&format!("Unavailable: {reason}"))).on_hover_text(reason);
                    response.on_disabled_hover_text(reason);
                }
                None => {
                    ui.add(muted(&entry.detail));
                    if response.on_hover_text(&entry.detail).clicked() {
                        return Some(Outcome::ServerModel { group: entry.group.clone(), name: entry.name.clone() });
                    }
                }
            }
        }
        Row::Workflows => {
            ui.heading("Imported ComfyUI workflows");
            ui.add(muted("Choosing one places a copy of that workflow card, with its saved fields, beside this card."));
        }
        Row::Workflow(index) => {
            let workflow = server.workflows.get(index)?;
            let response = ui.add_enabled(workflow.unavailable.is_none(), egui::Button::selectable(highlighted, &workflow.title).truncate());
            match &workflow.unavailable {
                Some(reason) => {
                    ui.add(muted(&format!("Unavailable: {reason}")));
                    response.on_disabled_hover_text(reason);
                }
                None => {
                    ui.add(muted("Imported workflow · its saved mapping"));
                    if response.clicked() {
                        return Some(Outcome::Workflow { canvas: workflow.canvas, id: workflow.id });
                    }
                }
            }
        }
        Row::Provider(_) | Row::Model(_) => {}
    }
    None
}

fn amount(money: Money) -> String {
    let fraction = format!("{:06}", money.micros % 1_000_000);
    let fraction = fraction.trim_end_matches('0');
    let whole = money.micros / 1_000_000;
    if fraction.is_empty() { format!("{whole} {}", String::from(money.currency)) } else { format!("{whole}.{fraction} {}", String::from(money.currency)) }
}
fn price_label(model: &ModelDescriptor) -> String {
    use nodecraft_catalog::Pricing;
    // The picker describes a catalog rate, without a workflow's runtime server evidence.
    // This model's Free rate covers local nodes only, never paid partner nodes (#872).
    if model.id == nodecraft_nodes::comfy_workflow::MODEL_ID {
        return "Workflow dependent; partner cost unknown".into();
    }
    match &model.pricing {
        Pricing::Free => "Free".into(),
        Pricing::ByUsage { currency } => format!("Usage based ({})", String::from(*currency)),
        Pricing::PerCall { price } => format!("{} / call", amount(price.0)),
        Pricing::PerUnit { unit, price, .. } => format!("{} / {}", amount(price.0), unit_label(*unit)),
        Pricing::PerUnitByChoice { unit, prices, input_fee, .. } => {
            let mut label = match prices.values().map(|price| price.0).min_by_key(|money| money.micros) {
                Some(cheapest) if prices.len() > 1 => format!("From {} / {}", amount(cheapest), unit_label(*unit)),
                Some(price) => format!("{} / {}", amount(price), unit_label(*unit)),
                None => format!("Priced per {}", unit_label(*unit)),
            };
            if let Some(fee) = input_fee {
                label.push_str(&format!(" + {} / input image", amount(fee.price.0)));
            }
            label
        }
        Pricing::PerSourceSecond { prices, .. } => match prices.values().map(|price| price.0).min_by_key(|money| money.micros) {
            Some(cheapest) if prices.len() > 1 => format!("From {} / second of source", amount(cheapest)),
            Some(price) => format!("{} / second of source", amount(price)),
            None => "Priced per second of source".into(),
        },
    }
}
fn unit_label(unit: Unit) -> &'static str {
    match unit {
        Unit::Call => "call",
        Unit::Image => "image",
        Unit::Second => "second",
        Unit::Megapixel => "megapixel",
        Unit::ThousandTokens => "1,000 tokens",
        Unit::ThousandCharacters => "1,000 characters",
        Unit::ThousandSteps => "1,000 training steps",
    }
}
fn modality(modality: Modality) -> &'static str {
    match modality {
        Modality::Text => "text",
        Modality::Image => "image",
        Modality::Video => "video",
        Modality::Audio => "audio",
        Modality::Mesh => "3D",
        Modality::Document => "document",
        Modality::Style => "style",
    }
}
fn model_details(ui: &mut egui::Ui, model: &ModelDescriptor) {
    ui.label(&model.description);
    for mode in &model.modes {
        ui.label(format!("{} to {}", mode.id, modality(mode.output)));
        for role in &mode.inputs {
            ui.label(format!(
                "{}: {} {}–{}{}",
                role.role,
                modality(role.modality),
                role.min,
                role.max,
                if role.inline { "; local prompt supported" } else { "" }
            ));
        }
        if let Some(audio) = &mode.audio {
            ui.label(audio.summary());
        }
        if let Some(edit) = &mode.edit {
            ui.label(edit.summary());
        }
        if let Some(document) = &mode.document {
            ui.label(document.summary());
        }
        if let Some(params) = &mode.params {
            ui.label(if params.is_empty() { "Sends no settings".to_owned() } else { format!("Sends only: {}", params.join(", ")) });
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bfl33_tests;
#[cfg(test)]
mod catalog963_tests;
#[cfg(test)]
mod ideogram34_tests;
#[cfg(test)]
mod ltx30_tests;

#[cfg(test)]
mod huggingface48_tests;

#[cfg(test)]
mod gemini44_tests;

#[cfg(test)]
mod deepgram40_tests;

#[cfg(test)]
mod grok31_tests;

#[cfg(test)]
mod hunyuan42_tests;
#[cfg(test)]
mod stability36_tests;

#[cfg(test)]
mod alibaba32_tests;

#[cfg(test)]
mod recraft35_tests;
#[cfg(test)]
mod rodin41_tests;

#[cfg(test)]
mod magnific38_tests;

#[cfg(test)]
mod heygen43_tests;

#[cfg(test)]
mod topaz37_tests;

#[cfg(test)]
mod pika29_tests;

#[cfg(test)]
mod comfy970_tests;

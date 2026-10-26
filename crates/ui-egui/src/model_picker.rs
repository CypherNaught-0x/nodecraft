//! Catalog model-choice presentation shared by a card and a bulk selection.
//!
//! This module never edits a node or authorizes a run. The controller retains the immutable
//! catalog snapshot, supplies role-specific compatibility, and previews/commits a choice through
//! the engine's shared model-swap transaction. Catalog rates are metadata, not launch consent.
use crate::theme::Tokens;
use nodecraft_catalog::{Catalog, ModelDescriptor, Wiring, model::MAX_TEXT, pricing::Unit};
use nodecraft_types::{Error, ErrorKind, Modality, Money};

const QUERY_CHARS: usize = 128;

struct Choice {
    id: String,
    unavailable: Option<String>,
}
#[derive(Clone, Copy)]
enum Row {
    Provider(usize),
    Model(usize),
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
        let mut picker = Self { choices, filtered, rows, query: String::new(), filtered_query: String::new(), highlighted: None, focus: true, text_bytes };
        picker.refilter(catalog);
        Ok(picker)
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
    }

    fn move_highlight(&mut self, down: bool) {
        let current = self.highlighted.and_then(|index| self.filtered.iter().position(|candidate| *candidate == index));
        let enabled = |index: &usize| self.choices.get(*index).is_some_and(|choice| choice.unavailable.is_none());
        let next = if down {
            self.filtered.iter().skip(current.map_or(0, |n| n.saturating_add(1))).copied().find(enabled)
        } else {
            self.filtered.iter().take(current.unwrap_or(self.filtered.len())).rev().copied().find(enabled)
        };
        if next.is_some() {
            self.highlighted = next;
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
                if self.filtered.is_empty() {
                    ui.label("No matching models");
                }
                let row_height = t.control_row_height * 2.0;
                let mut scroll = egui::ScrollArea::vertical().max_height(t.comments_height);
                if (up || down)
                    && let Some(row) = self.rows.iter().position(|row| matches!(row, Row::Model(index) if Some(*index) == self.highlighted))
                {
                    scroll = scroll.vertical_scroll_offset(row as f32 * (row_height + ui.spacing().item_spacing.y));
                }
                scroll.show_rows(ui, row_height, self.rows.len(), |ui, range| {
                    for row in self.rows.get(range).into_iter().flatten() {
                        let index = match row {
                            Row::Provider(index) | Row::Model(index) => *index,
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
                            ui.label(
                                egui::RichText::new(format!("Quality {}/5 · typically {} s · {}", model.quality, model.latency_s, price_label(model)))
                                    .color(t.text_muted),
                            );
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

fn amount(money: Money) -> String {
    let fraction = format!("{:06}", money.micros % 1_000_000);
    let fraction = fraction.trim_end_matches('0');
    let whole = money.micros / 1_000_000;
    if fraction.is_empty() { format!("{whole} {}", String::from(money.currency)) } else { format!("{whole}.{fraction} {}", String::from(money.currency)) }
}
fn price_label(model: &ModelDescriptor) -> String {
    use nodecraft_catalog::Pricing;
    match &model.pricing {
        Pricing::Free => "Free".into(),
        Pricing::ByUsage { currency } => format!("Usage based ({})", String::from(*currency)),
        Pricing::PerCall { price } => format!("{} / call", amount(price.0)),
        Pricing::PerUnit { unit, price, .. } => format!("{} / {}", amount(price.0), unit_label(*unit)),
        Pricing::PerUnitByChoice { unit, prices, .. } => match prices.values().map(|price| price.0).min_by_key(|money| money.micros) {
            Some(cheapest) if prices.len() > 1 => format!("From {} / {}", amount(cheapest), unit_label(*unit)),
            Some(price) => format!("{} / {}", amount(price), unit_label(*unit)),
            None => format!("Priced per {}", unit_label(*unit)),
        },
        Pricing::PerSourceSecond { prices, .. } => match prices.values().map(|price| price.0).min_by_key(|money| money.micros) {
            Some(cheapest) if prices.len() > 1 => format!("From {} / second of source", amount(cheapest)),
            Some(price) => format!("{} / second of source", amount(price)),
            None => "Priced per second of source".into(),
        },
    }
}
fn unit_label(unit: Unit) -> &'static str {
    match unit {
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
mod huggingface48_tests;

#[cfg(test)]
mod gemini44_tests;

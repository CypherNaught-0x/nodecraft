//! The API keys panel's endpoint rows (#962): for each provider that takes a custom base URL
//! (#960), the address its requests and key go to, which the person can set, change or reset
//! next to the key. Saving is the host's: the panel checks an address and hands it over.
use std::collections::BTreeMap;

use nodecraft_engine::custom_endpoints::{self, CustomEndpoint};
use nodecraft_engine::settings::ProviderSettings;

use crate::theme::Tokens;

/// The longest address a field takes.
const MAX_URL_CHARS: usize = 2048;

/// A base URL to save for `provider`: `None` resets it to the provider's own API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointChange {
    pub provider: &'static str,
    pub url: Option<String>,
}

/// What a provider's saved entry says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InUse {
    Default,
    Set(String),
    /// Set, but not text NodeCraft can use: its runs are refused until it is fixed or reset.
    Unusable,
}

#[derive(Default)]
pub(super) struct Endpoints {
    /// What settings say, by provider; `None` until the host has said (no settings here).
    in_use: Option<BTreeMap<&'static str, InUse>>,
    /// The fields being edited, by provider.
    edits: BTreeMap<&'static str, String>,
    changes: Vec<EndpointChange>,
    pub(super) notice: Option<String>,
}

impl Endpoints {
    /// Takes what settings say now. A field the person hasn't touched follows it.
    pub(super) fn sync(&mut self, settings: &ProviderSettings) {
        let in_use: BTreeMap<&'static str, InUse> = custom_endpoints::available()
            .map(|entry| {
                // As runs read it: a misspelled or unreadable entry refuses them, so it shows.
                let state = match custom_endpoints::base_url(settings, entry.provider) {
                    Ok(None) => InUse::Default,
                    Ok(Some(url)) => InUse::Set(url.as_str().to_owned()),
                    Err(_) => InUse::Unusable,
                };
                (entry.provider, state)
            })
            .collect();
        if self.in_use.as_ref() == Some(&in_use) {
            return;
        }
        for (provider, state) in &in_use {
            let before = self.in_use.as_ref().and_then(|seen| seen.get(provider));
            let field = self.edits.entry(provider).or_default();
            if before.is_none_or(|before| *field == shown(before)) {
                *field = shown(state);
            }
        }
        self.in_use = Some(in_use);
    }

    /// Forgets what was typed, when the panel closes.
    pub(super) fn forget(&mut self) {
        self.in_use = None;
        self.edits.clear();
        self.notice = None;
    }

    pub(super) fn take_changes(&mut self) -> Vec<EndpointChange> {
        std::mem::take(&mut self.changes)
    }

    /// Draws the rows; nothing when the host has given no settings.
    pub(super) fn show(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let Some(in_use) = self.in_use.clone() else { return };
        ui.separator();
        ui.label(egui::RichText::new("Endpoints").color(t.text));
        ui.label(
            egui::RichText::new(
                "Send a provider's requests and its key to another server that speaks its API, such as a LiteLLM proxy or vLLM. \
                 Leave blank for the provider's own API. Test checks the key at the address in use.",
            )
            .color(t.text_muted),
        );
        egui::Grid::new("nodecraft.keys.endpoints").num_columns(3).spacing([t.control_gap * 2.0, t.control_gap]).show(ui, |ui| {
            for entry in custom_endpoints::available() {
                let state = in_use.get(entry.provider).cloned().unwrap_or(InUse::Default);
                self.row(ui, entry, &state, t);
                ui.end_row();
            }
        });
        if let Some(notice) = &self.notice {
            ui.label(egui::RichText::new(notice).color(t.danger));
        }
    }

    fn row(&mut self, ui: &mut egui::Ui, entry: &'static CustomEndpoint, state: &InUse, t: &Tokens) {
        let account = nodecraft_engine::provider_setup::key_account(entry.provider).unwrap_or(entry.provider);
        let label = ui.label(egui::RichText::new(entry.label).color(t.text)).on_hover_text(format!("Uses the {account} key"));
        let (text, color) = match state {
            InUse::Default => (format!("{} (default)", entry.default), t.text_muted),
            InUse::Set(url) => (url.clone(), t.text),
            InUse::Unusable => ("Saved address can't be used".to_owned(), t.danger),
        };
        let shown = ui.label(egui::RichText::new(text).color(color).monospace());
        if *state == InUse::Unusable {
            shown.on_hover_text("Its runs are refused until you set or reset it here, or fix [providers.base_urls] in config.toml");
        }
        ui.horizontal(|ui| {
            let field = self.edits.entry(entry.provider).or_default();
            ui.add(
                egui::TextEdit::singleline(field)
                    .id(egui::Id::new(("nodecraft.keys.endpoint", entry.provider)))
                    .char_limit(MAX_URL_CHARS)
                    .hint_text(entry.default)
                    .desired_width(ui.spacing().text_edit_width),
            )
            .labelled_by(label.id);
            let typed = field.clone();
            if ui.button("Save address").on_hover_text(format!("Save the address for {}", entry.label)).clicked() {
                // Checked before anything is saved; the message never quotes the address.
                match custom_endpoints::check(Some(&typed)) {
                    Ok(url) => {
                        self.notice = None;
                        if let Some(url) = &url {
                            self.edits.insert(entry.provider, url.clone());
                        }
                        self.changes.push(EndpointChange { provider: entry.provider, url });
                    }
                    Err(error) => self.notice = Some(format!("{}: {}", entry.label, error.message)),
                }
            }
            if ui
                .add_enabled(*state != InUse::Default, egui::Button::new("Reset"))
                .on_hover_text(format!("Use {}'s own API again", entry.label))
                .on_disabled_hover_text(format!("{} already uses its own API", entry.label))
                .clicked()
            {
                self.notice = None;
                self.edits.insert(entry.provider, String::new());
                self.changes.push(EndpointChange { provider: entry.provider, url: None });
            }
        });
    }
}

/// What a field shows for a saved state: the address, or blank for the default.
fn shown(state: &InUse) -> String {
    match state {
        InUse::Set(url) => url.clone(),
        InUse::Default | InUse::Unusable => String::new(),
    }
}

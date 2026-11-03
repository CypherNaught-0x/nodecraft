//! Browser server settings, saved by engine commands. No provider token enters these fields.
use super::NodeCraftApp;
use crate::theme::Tokens;
use nodecraft_engine::browser_providers::OriginProviders;
use nodecraft_engine::settings::ProviderSettings;
use nodecraft_types::Error;

pub(super) struct BrowserProviders {
    pub store: OriginProviders,
    pub open: bool,
    was_open: bool,
    comfyui: String,
    litellm: String,
    /// Custom base URLs being edited, by provider (#960); one field per
    /// [`nodecraft_engine::custom_endpoints::PROVIDERS`] entry.
    base_urls: std::collections::BTreeMap<&'static str, String>,
    applied: ProviderSettings,
    notice: Option<String>,
}

impl BrowserProviders {
    #[cfg(any(test, target_arch = "wasm32"))]
    pub(super) fn new(store: OriginProviders, notice: Option<String>) -> Self {
        let settings = store.settings().unwrap_or_default();
        Self {
            store,
            open: false,
            was_open: false,
            comfyui: nodecraft_engine::browser_providers::comfyui_url(&settings).unwrap_or_default(),
            litellm: settings.litellm_url.clone().unwrap_or_default(),
            base_urls: base_urls(|provider| settings.base_urls.get(provider).cloned()),
            applied: settings,
            notice,
        }
    }

    fn changed(&mut self) -> Result<Option<ProviderSettings>, Error> {
        let settings = self.store.settings()?;
        if settings == self.applied {
            return Ok(None);
        }
        self.applied = settings.clone();
        Ok(Some(settings))
    }

    fn show(&mut self, ctx: &egui::Context, session: &mut nodecraft_engine::Session, t: &Tokens) {
        if self.open && !self.was_open {
            match session.dispatch("providers.settings_get", &serde_json::json!({})) {
                Ok(settings) => {
                    self.comfyui = settings.get("comfyui_url").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
                    self.litellm = settings.get("litellm_url").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
                    let saved = settings.get("base_urls");
                    self.base_urls = base_urls(|provider| saved.and_then(|urls| urls.get(provider)).and_then(serde_json::Value::as_str).map(str::to_owned));
                }
                Err(error) => self.notice = Some(error.message),
            }
        }
        self.was_open = self.open;
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("Provider servers").id(egui::Id::new("nodecraft.browser_providers")).open(&mut open).default_width(t.dialog_width).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
            ui.label("Server addresses are kept in this browser for this site. Put tokens and virtual keys in API keys.");
            ui.label("Calls go directly from this page to the server. Allow this page's origin in ComfyUI and LiteLLM's CORS settings. HTTPS pages may block HTTP servers.");
            let label = ui.label("ComfyUI server");
            ui.add(egui::TextEdit::singleline(&mut self.comfyui).id(egui::Id::new("browser_comfyui_url")).char_limit(2048).hint_text("http://127.0.0.1:8188")).labelled_by(label.id);
            let label = ui.label("LiteLLM server");
            ui.add(egui::TextEdit::singleline(&mut self.litellm).id(egui::Id::new("browser_litellm_url")).char_limit(2048).hint_text("http://127.0.0.1:4000")).labelled_by(label.id);
            ui.label("Use the LiteLLM proxy's root address, without /v1. Its optional virtual key uses the litellm account in API keys.");
            ui.label("LiteLLM models are not available yet. You can save this address for future use.");
            ui.separator();
            ui.label("Custom endpoints");
            ui.label(
                "Send a provider's requests and key to another server that speaks its API, such as a LiteLLM proxy or vLLM. \
                 Leave blank to use the provider's own API. Runs keep the catalog price; a step that would be free needs your consent to an unknown price.",
            );
            for entry in nodecraft_engine::custom_endpoints::PROVIDERS {
                let label = ui.label(entry.label);
                let text = self.base_urls.entry(entry.provider).or_default();
                ui.add(egui::TextEdit::singleline(text).id(egui::Id::new(("browser_base_url", entry.provider))).char_limit(2048).hint_text(entry.default))
                    .labelled_by(label.id);
            }
            if ui.button("Save servers").clicked() {
                let base_urls: serde_json::Map<String, serde_json::Value> =
                    self.base_urls.iter().map(|(provider, url)| ((*provider).to_owned(), serde_json::Value::from(url.as_str()))).collect();
                match session.dispatch("providers.settings_set", &serde_json::json!({"comfyui_url": self.comfyui, "litellm_url": self.litellm, "base_urls": base_urls})) {
                    Ok(_) => self.notice = Some("Provider servers saved for this site.".into()),
                    Err(error) => self.notice = Some(error.message),
                }
            }
            if let Some(notice) = &self.notice {
                ui.label(notice);
            }
        });
        self.open = open;
    }
}

/// One editable text per provider that takes a custom base URL, from what `saved` holds.
fn base_urls(saved: impl Fn(&str) -> Option<String>) -> std::collections::BTreeMap<&'static str, String> {
    nodecraft_engine::custom_endpoints::PROVIDERS.iter().map(|entry| (entry.provider, saved(entry.provider).unwrap_or_default())).collect()
}

impl NodeCraftApp {
    pub(super) fn browser_providers_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        if let Some(providers) = &mut self.browser_providers {
            providers.show(ctx, &mut self.session, t);
        }
    }

    /// Also observes command-driven saves, so a remote settings change invalidates cached
    /// adapters and an old endpoint's pending list before the next frame accepts input.
    pub(super) fn tick_browser_providers(&mut self) {
        let Some(providers) = &mut self.browser_providers else { return };
        let settings = match providers.changed() {
            Ok(Some(settings)) => settings,
            Ok(None) => return,
            Err(error) => {
                self.notice = Some(error.message);
                return;
            }
        };
        self.run_confirm = None;
        nodecraft_engine::option_fetch::invalidate(&mut self.session);
        if let Some(fetcher) = &mut self.option_fetch
            && let Some(notice) = fetcher.invalidate(&mut self.session)
        {
            self.notice = Some(notice);
        }
        self.session.swap_resource::<nodecraft_types::OptionLists>(None);
        self.schemas.forget_listed();
        if let Some(runs) = &mut self.runs
            && let Err(error) = runs.settings_changed(&mut self.session, &settings)
        {
            self.notice = Some(error.message);
        }
    }
}

#[cfg(test)]
mod tests;

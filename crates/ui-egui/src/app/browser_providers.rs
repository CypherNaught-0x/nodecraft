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
            if ui.button("Save servers").clicked() {
                match session.dispatch("providers.settings_set", &serde_json::json!({"comfyui_url": self.comfyui, "litellm_url": self.litellm})) {
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

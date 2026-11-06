//! The LiteLLM proxy's address, next to its key in the API keys panel (desktop). The browser build
//! sets it in its Provider servers panel instead.
//!
//! The address is not a secret: it is kept in settings (`[providers] litellm_url`), checked
//! before it is saved, and shown as typed. Its virtual key stays in the `litellm` keychain account.
use crate::theme::Tokens;

/// What the panel's LiteLLM section holds between frames.
#[derive(Default)]
pub struct LitellmAddress {
    /// The address in settings, as the host last said.
    saved: Option<String>,
    /// What is typed; `None` until the field is first shown, then it starts from `saved`.
    draft: Option<String>,
    /// A change to save: `Some(None)` clears the address.
    change: Option<Option<String>>,
    /// Why the last address wasn't saved.
    pub error: Option<String>,
}

/// The address as NodeCraft keeps it: the proxy root, without a trailing `/` or `/v1` (the chat
/// client adds `/v1`). An empty field clears it. Invalid addresses are refused before saving.
pub fn normalized(text: &str) -> Result<Option<String>, String> {
    let trimmed = text.trim().trim_end_matches('/');
    let root = trimmed.strip_suffix("/v1").unwrap_or(trimmed).trim_end_matches('/');
    nodecraft_engine::browser_providers::server_url(Some(root))
        .map(|url| url.map(|url| url.trim_end_matches('/').to_owned()))
        .map_err(|_| "Enter the proxy's address as http(s)://host[:port], without a user name, password, query or fragment.".to_owned())
}

impl LitellmAddress {
    /// The host tells the section what settings hold, every frame it may be drawn.
    pub fn set_saved(&mut self, saved: Option<&str>) {
        if self.saved.as_deref() != saved {
            self.saved = saved.map(str::to_owned);
            // A change saved elsewhere replaces an untouched draft.
            self.draft = None;
        }
    }

    /// The change to save, once: `Some(None)` clears the address.
    pub fn take_change(&mut self) -> Option<Option<String>> {
        self.change.take()
    }

    /// Draws the section.
    pub fn show(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        ui.label(egui::RichText::new("LiteLLM server").color(t.text));
        ui.label(
            egui::RichText::new("The proxy's address, such as https://litellm.example.com. Its models are listed in model pickers once it is saved.")
                .color(t.text_muted),
        );
        let saved = self.saved.clone().unwrap_or_default();
        let draft = self.draft.get_or_insert_with(|| saved.clone());
        let mut save = false;
        ui.horizontal(|ui| {
            let label = ui.label(egui::RichText::new("Address").color(t.text_muted));
            let field = ui.add(egui::TextEdit::singleline(draft).hint_text("https://litellm.example.com").desired_width(t.card_width)).labelled_by(label.id);
            save |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let changed = draft.trim() != saved;
            save |= ui.add_enabled(changed, egui::Button::new("Save address")).clicked();
            if !saved.is_empty() && ui.button("Clear address").clicked() {
                draft.clear();
                save = true;
            }
        });
        if save {
            match normalized(draft) {
                Ok(url) => {
                    self.error = None;
                    if let Some(url) = &url {
                        draft.clone_from(url);
                    }
                    self.change = Some(url);
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(error) = &self.error {
            ui.label(egui::RichText::new(error).color(t.danger));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_kept_as_the_proxy_root() {
        assert_eq!(normalized("https://llms.example.com").unwrap().as_deref(), Some("https://llms.example.com"));
        assert_eq!(normalized(" https://llms.example.com/v1/ ").unwrap().as_deref(), Some("https://llms.example.com"));
        assert_eq!(normalized("http://127.0.0.1:4000/").unwrap().as_deref(), Some("http://127.0.0.1:4000"));
        assert_eq!(normalized("https://example.com/litellm/v1").unwrap().as_deref(), Some("https://example.com/litellm"));
        assert_eq!(normalized("").unwrap(), None, "an empty field clears it");
    }

    #[test]
    fn bad_addresses_are_refused_before_saving() {
        for bad in ["llms.example.com", "ftp://x.example", "https://user:pw@x.example", "https://x.example/?key=1", "https://x.example/#f", "https://"] {
            assert!(normalized(bad).is_err(), "{bad}");
        }
    }
}

//! The palette lends its query focus only to its own live navigation selection.
use super::*;

impl NodeCraftApp {
    pub(super) fn live_palette_entries(&mut self, ctx: &egui::Context) -> Vec<palette::Entry> {
        let mut entries = self.palette_entries();
        let navigation: Vec<_> = self.session.registry().specs().filter(|spec| Self::is_navigation_command(spec.id)).copied().collect();
        for spec in navigation {
            if self.palette_navigation_enabled(ctx, spec.id, &self.palette_command_params(spec.id)).is_ok() {
                entries.push(palette::Entry {
                    target: palette::Target::Command(spec.id),
                    label: spec.label.to_owned(),
                    shortcut: spec.shortcut.map(str::to_owned),
                });
            }
        }
        entries
    }

    pub(super) fn dispatch_live_palette_command(&mut self, ctx: &egui::Context, id: &str) {
        if Self::is_navigation_command(id) {
            // The chosen palette is already closed. Relinquish only its query; another editor
            // or an active gesture still refuses the ordinary live navigation dispatch.
            Palette::release_text_focus(ctx);
            self.text_focused = canvas::text_input_focused(ctx);
            if let Err(error) = self.dispatch_navigation(ctx, id, &self.palette_command_params(id)) {
                self.notice = Some(error.message);
            }
        } else {
            self.dispatch_palette_command(id);
        }
    }
}

#[cfg(test)]
mod tests;

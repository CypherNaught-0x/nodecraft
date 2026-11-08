//! The palette lends its query focus only to its own live navigation selection.
use super::*;

impl NodeCraftApp {
    pub(super) fn live_palette_entries(&mut self, ctx: &egui::Context) -> Vec<palette::Entry> {
        let mut entries = self.palette_entries();
        let [all, selected, toggle, _] = self.palette_navigation_contexts(ctx);
        let navigation: Vec<_> =
            self.session.registry().specs().filter(|spec| Self::is_navigation_command(spec.id) && spec.id != "canvas.pan_to").copied().collect();
        for spec in navigation {
            let context = match spec.id {
                "canvas.fit_all" => &all,
                "canvas.fit_selection" => &selected,
                _ => &toggle,
            };
            let params = self.palette_command_params(spec.id);
            if !palette::takes(&spec, &params) {
                continue;
            }
            match self.navigation_enabled_with_context(context.clone(), spec.id, &params) {
                Ok(()) => entries.push(palette::Entry::command(&spec, None)),
                Err(error) if palette::explains(&error) => entries.push(palette::Entry::command(&spec, Some(&error))),
                Err(_) => {}
            }
        }
        palette::sort(&mut entries);
        entries
    }

    /// Runs a command chosen in the palette; whether it ran.
    pub(super) fn dispatch_live_palette_command(&mut self, ctx: &egui::Context, id: &str) -> bool {
        if Self::is_navigation_command(id) {
            // The chosen palette is already closed. Relinquish only its query; another editor
            // or an active gesture still refuses the ordinary live navigation dispatch.
            Palette::release_text_focus(ctx);
            self.text_focused = canvas::text_input_focused(ctx);
            match self.dispatch_navigation(ctx, id, &self.palette_command_params(id)) {
                Ok(_) => true,
                Err(error) => {
                    self.notice = Some(error.message);
                    false
                }
            }
        } else {
            self.dispatch_palette_command(id)
        }
    }
}

#[cfg(test)]
mod tests;

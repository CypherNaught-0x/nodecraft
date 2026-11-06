//! The keys panel's custom endpoints (#962): the panel shows each provider's address and checks
//! a new one; the app saves it in settings (`[providers.base_urls]`, #960) and drops the adapters
//! and quotes that used the old one, so runs and `keys.test` follow at once.
use super::NodeCraftApp;
use crate::theme::Tokens;

impl NodeCraftApp {
    /// The API keys panel, with the endpoint rows where settings can be saved.
    pub(super) fn keys_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        #[cfg(not(target_arch = "wasm32"))]
        if self.keys.open {
            let settings = self.store.is_some().then(|| self.provider_settings());
            self.keys.sync_endpoints(settings.as_ref());
        }
        let changes = self.keys.show(ctx, &mut self.session, &self.key_accounts, t);
        self.save_accounts(changes);
        #[cfg(not(target_arch = "wasm32"))]
        for change in self.keys.take_endpoint_changes() {
            let notice = self.save_endpoint(&change).err().map(|error| error.message);
            self.keys.endpoint_notice(notice);
        }
    }

    /// Saves `change` in settings and writes the file now, so a check reads what was shown.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn save_endpoint(&mut self, change: &crate::keys::EndpointChange) -> Result<(), nodecraft_types::Error> {
        use nodecraft_types::{Error, ErrorKind};
        let Some(store) = &mut self.store else {
            return Err(Error::new(ErrorKind::Unsupported, "settings can't be saved here, so the address can't be changed"));
        };
        if store.is_read_only() {
            return Err(Error::new(ErrorKind::Unsupported, "the settings file is read-only, so the address can't be changed"));
        }
        // Checked again here: the panel's check is the one the person sees, this one is the gate.
        let url = nodecraft_engine::custom_endpoints::check(change.url.as_deref())?;
        // A reset writes a blank entry, which means the provider's own API: settings keep keys
        // they don't know as written, so removing it could leave the old address in the file.
        store.update(|saved| {
            saved.providers.base_urls.insert(change.provider.to_owned(), url.clone().unwrap_or_default().into());
        });
        let written = store
            .flush()
            .map_err(|error| Error::new(ErrorKind::Internal, format!("the address is in use but couldn't be written to the settings file: {error}")));
        let settings = store.get().providers.clone();
        nodecraft_engine::option_fetch::invalidate(&mut self.session);
        let changed = match &mut self.runs {
            Some(runs) => runs.settings_changed(&mut self.session, &settings),
            None => Ok(()),
        };
        // The file's failure says more: it is why a check now answers "unverifiable".
        written.and(changed)?;
        // An entry spelled differently (`OpenAI`, `openai_images`) or a `[providers.base_urls]`
        // that isn't a table still refuses the provider's runs, and settings keep keys they
        // don't model, so only the file can be fixed.
        match nodecraft_engine::custom_endpoints::base_url(&settings, change.provider) {
            Ok(_) => Ok(()),
            Err(error) => Err(Error::new(
                ErrorKind::InvalidInput,
                format!("saved, but config.toml still has another entry for this provider: {} (under [providers.base_urls])", error.message),
            )),
        }
    }
}

#[cfg(test)]
#[path = "key_endpoints_tests.rs"]
mod tests;

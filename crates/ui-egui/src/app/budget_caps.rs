//! Spending caps (#201): the Budgets panel's caps, kept in settings and handed to the run host.
use super::*;

impl NodeCraftApp {
    /// The caps runs are checked against now.
    pub fn budget_caps(&self) -> &nodecraft_engine::settings::BudgetSettings {
        &self.budget_caps
    }

    /// Checks later quotes and jobs against `caps`; saving them is the caller's.
    pub(super) fn set_budget_caps(&mut self, caps: nodecraft_engine::settings::BudgetSettings) {
        if let Some(runs) = &self.runs {
            runs.set_budgets(&mut self.session, caps.budgets());
        }
        self.budget_caps = caps;
    }

    /// Draws the Budgets panel and keeps what it saves; the browser keeps it for the page only.
    pub(super) fn budgets_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        let spent = self.runs.as_ref().map(|runs| runs.spent(&self.session));
        let Some(caps) = self.budgets.show(ctx, &self.budget_caps, spent.as_ref(), t) else { return };
        #[cfg(not(target_arch = "wasm32"))]
        let kept = if let Some(store) = &mut self.store {
            store.update(|settings| settings.budgets = caps.clone());
            true
        } else {
            false
        };
        #[cfg(target_arch = "wasm32")]
        let kept = false;
        if !kept {
            self.budgets.status = Some("Runs are checked against these caps until this window closes; there is no settings file to keep them in.".into());
        }
        self.set_budget_caps(caps);
    }
}

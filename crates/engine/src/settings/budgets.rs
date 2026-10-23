//! Spending caps for runs (#201): per call, per session and per project, in US cents. Only the
//! person changes them, in the app's Budgets section; no command raises them, so an agent can't
//! lift its own limit. Read the hostile fields through [`BudgetSettings::budgets`].
use nodecraft_exec::cost::Budgets;
use nodecraft_types::{Currency, Money};
use serde::{Deserialize, Serialize};

/// No cap unless one is set; an unset cap is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetSettings {
    /// Most one job may cost.
    pub per_call_usd_cents: Option<u64>,
    /// Most this session may spend.
    pub session_usd_cents: Option<u64>,
    /// Most the project open in this window may spend.
    pub project_usd_cents: Option<u64>,
}

impl BudgetSettings {
    /// The largest cap read from the file: 100,000 USD.
    pub const MAX_USD_CENTS: u64 = 10_000_000;

    /// The caps the run host checks, each capped at [`Self::MAX_USD_CENTS`].
    pub fn budgets(&self) -> Budgets {
        let usd = |cents: Option<u64>| cents.map(|c| Money::new(Currency::USD, c.min(Self::MAX_USD_CENTS).saturating_mul(10_000)));
        Budgets { per_call: usd(self.per_call_usd_cents), session: usd(self.session_usd_cents), project: usd(self.project_usd_cents) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_from_the_file_are_bounded_and_in_dollars() {
        let settings = BudgetSettings { per_call_usd_cents: Some(25), session_usd_cents: Some(u64::MAX), project_usd_cents: None };
        let budgets = settings.budgets();
        assert_eq!(budgets.per_call, Some(Money::new(Currency::USD, 250_000)));
        assert_eq!(budgets.session, Some(Money::new(Currency::USD, 100_000_000_000)));
        assert_eq!(budgets.project, None);
        assert_eq!(BudgetSettings::default().budgets(), Budgets::default(), "nothing is capped until the person sets a cap");
        let parsed: BudgetSettings = serde_json::from_value(serde_json::json!({"session_usd_cents": 500})).unwrap();
        assert_eq!(parsed.budgets().session, Some(Money::new(Currency::USD, 5_000_000)));
    }
}

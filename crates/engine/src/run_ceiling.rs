//! The cumulative per-run ceiling for accepted quotes (#366).
//!
//! `run.start` checks the quote against what the caller accepted, per currency. A quote is only
//! an estimate when part of it is deferred (a step whose job count is known at run time is
//! priced as one job) or retried under a new key, so the accepted amounts are also kept as a
//! ceiling on what the run may reserve while it runs:
//!
//! - Every new priced reservation of the run is debited at its estimate, under the usage log's
//!   lock, and refused before the provider is invoked when it would take the run's reserved
//!   total in that currency past the accepted amount. A currency the caller accepted nothing in
//!   has a ceiling of zero: an absent amount never authorizes spending.
//! - A reservation released on evidence that nothing reached the provider is credited back; one
//!   that may have been billed (unresolved) or was completed keeps its debit. A cheaper actual
//!   cost never frees room, and a dearer one is reported in the usage log as it is; the ceiling
//!   bounds new admission, not what a provider eventually bills.
//! - A job adopted after it already ran is debited even past the ceiling (it isn't refused:
//!   the work happened).
//! - Jobs whose price is unknown are never debited as if free. They exist only for steps the
//!   quote listed as unpriced, which `run.start` admits only with explicit partial consent, and
//!   any budget cap still refuses them.
//!
//! The ceiling lives only in the authorized run built from the prepared run's own acceptance;
//! nothing outside this crate can create or widen one, and it isn't saved: a reloaded session
//! needs a new quote and acceptance.
use nodecraft_types::{Currency, Money, ProviderTarget};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

/// Most debits one run's ceiling records (a run has at most 100,000 jobs). Debits that settle stay
/// recorded, so this bounds the run's whole life, not just its open reservations.
pub const MAX_CEILING_RESERVATIONS: usize = 100_000;

#[derive(Debug, Default)]
struct Ledger {
    limits: BTreeMap<Currency, u64>,
    reserved: BTreeMap<Currency, u64>,
    /// Debits by reservation, so a credit applies once and only to this run's own debit.
    debits: BTreeMap<(ProviderTarget, String), Money>,
}

/// A run's accepted amounts and what it has reserved against them. Clones share one ledger.
#[derive(Clone)]
pub struct RunCeiling(Arc<Mutex<Ledger>>);

impl std::fmt::Debug for RunCeiling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunCeiling")
    }
}

impl PartialEq for RunCeiling {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for RunCeiling {}

impl RunCeiling {
    pub(crate) fn new(limits: BTreeMap<Currency, u64>) -> Self {
        Self(Arc::new(Mutex::new(Ledger { limits, ..Ledger::default() })))
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The accepted amount and what is reserved against it, in micros.
    pub fn status(&self, currency: Currency) -> (u64, u64) {
        let ledger = self.ledger();
        (ledger.limits.get(&currency).copied().unwrap_or(0), ledger.reserved.get(&currency).copied().unwrap_or(0))
    }

    /// Debits a new reservation; `Ok(false)` when this run already holds a debit for it. `adopted`
    /// work already ran, so it is debited even past the ceiling. `Err` is the refusal message;
    /// nothing changes on an error.
    pub(crate) fn debit(&self, provider: &ProviderTarget, key: &str, estimate: Money, adopted: bool) -> Result<bool, String> {
        let mut ledger = self.ledger();
        let id = (provider.clone(), key.to_owned());
        if ledger.debits.contains_key(&id) {
            return Ok(false);
        }
        if !adopted && ledger.debits.len() >= MAX_CEILING_RESERVATIONS {
            return Err("the run has reserved too many jobs to track; quote and start a new run".into());
        }
        let currency = estimate.currency;
        let limit = ledger.limits.get(&currency).copied().unwrap_or(0);
        let reserved = ledger.reserved.get(&currency).copied().unwrap_or(0);
        let after = reserved.checked_add(estimate.micros);
        if !adopted && after.is_none_or(|after| after > limit) {
            return Err(if limit == 0 {
                format!("the run's accepted amounts include nothing in {currency}, so this job can't be charged")
            } else {
                format!("the run would reserve more than the {currency} amount that was accepted; quote and accept again to continue")
            });
        }
        ledger.reserved.insert(currency, after.unwrap_or(u64::MAX));
        ledger.debits.insert(id, estimate);
        Ok(true)
    }

    /// Credits a reservation released without reaching the provider. A reservation this run
    /// didn't debit, or one already credited, changes nothing.
    pub(crate) fn credit(&self, provider: &ProviderTarget, key: &str) {
        let mut ledger = self.ledger();
        if let Some(money) = ledger.debits.remove(&(provider.clone(), key.to_owned())) {
            let reserved = ledger.reserved.entry(money.currency).or_default();
            *reserved = reserved.saturating_sub(money.micros);
        }
    }
}

#[cfg(test)]
mod tests;

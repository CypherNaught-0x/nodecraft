//! The cumulative per-run ceiling for accepted quotes (#366).
//!
//! `run.start` checks the quote against what the caller accepted. A quote is only an estimate
//! when part of it is deferred (a step whose job count is known at run time is priced as one job)
//! or retried under a new key, so what was accepted is also kept as a ceiling on what the run may
//! admit while it runs:
//!
//! - **Priced jobs.** Every new priced reservation of the run is debited at its estimate, under
//!   the usage log's lock, and refused before the provider is invoked when it would take the
//!   run's reserved total in that currency past the accepted amount. A currency the caller
//!   accepted nothing in has a ceiling of zero: an absent amount never authorizes spending.
//! - **Unknown-price jobs.** They are never debited as if free. The caller accepts a count of
//!   them (`unpriced_jobs`), and every new unknown-price admission (a provider and key the run
//!   hasn't admitted) uses one; past the count it is refused before the provider. A missing
//!   count is zero, never unlimited. The count is not a price: budgets still apply on their own.
//! - **Releases.** A priced reservation released on evidence that nothing reached the provider
//!   is credited back, by the reservation that took the debit only. One that may have been
//!   billed (unresolved) or was completed keeps its debit; a cheaper actual cost never frees
//!   room, and a dearer one is reported in the usage log as it is. Unknown-price admissions are
//!   never given back: without a price, a release proves nothing worth refunding.
//! - **Adopted work.** A job adopted after it was already submitted is debited (or counted) even
//!   past the ceiling, and its debit is never refunded: the work happened.
//!
//! The ceiling lives only in the authorized run built from the prepared run's own acceptance;
//! nothing outside this crate can create or widen one, and it isn't saved: a reloaded session
//! needs a new quote and acceptance.
use nodecraft_types::{Currency, Money, ProviderTarget};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

/// Most priced debits, and separately most unknown-price admissions, one run records by provider
/// and key (a run has at most 100,000 jobs). Settled debits stay recorded, so this bounds the
/// run's whole life. Adopted work past it is still accounted, in an aggregate that is never
/// refunded.
pub const MAX_CEILING_RESERVATIONS: usize = 100_000;

type Reservation = (ProviderTarget, String);

#[derive(Debug, Default)]
struct Ledger {
    limits: BTreeMap<Currency, u64>,
    reserved: BTreeMap<Currency, u64>,
    /// Debits by reservation, so a credit applies once and only to this run's own debit.
    debits: BTreeMap<Reservation, Money>,
    unpriced_limit: u64,
    unpriced: BTreeSet<Reservation>,
    /// Adopted unknown-price work past [`MAX_CEILING_RESERVATIONS`], counted but not keyed.
    unpriced_overflow: u64,
}

impl Ledger {
    fn unpriced_used(&self) -> u64 {
        u64::try_from(self.unpriced.len()).unwrap_or(u64::MAX).saturating_add(self.unpriced_overflow)
    }
}

/// A run's accepted amounts and unknown-price job count, and what it has admitted against them.
/// Clones share one ledger.
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
    pub(crate) fn new(limits: BTreeMap<Currency, u64>, unpriced_limit: u64) -> Self {
        Self(Arc::new(Mutex::new(Ledger { limits, unpriced_limit, ..Ledger::default() })))
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The accepted amount and what is reserved against it, in micros.
    pub fn status(&self, currency: Currency) -> (u64, u64) {
        let ledger = self.ledger();
        (ledger.limits.get(&currency).copied().unwrap_or(0), ledger.reserved.get(&currency).copied().unwrap_or(0))
    }

    /// The accepted count of unknown-price jobs and how many the run has admitted.
    pub fn unpriced_status(&self) -> (u64, u64) {
        let ledger = self.ledger();
        (ledger.unpriced_limit, ledger.unpriced_used())
    }

    /// Debits a new priced reservation. `Ok(true)` when this call took a debit the reservation
    /// may later credit back; `Ok(false)` when the run already holds one for it, or when adopted
    /// work past the record bound went into the unrefundable aggregate. `adopted` work was
    /// already submitted, so it is debited even past the ceiling. `Err` is the refusal message;
    /// nothing changes on an error.
    pub(crate) fn debit(&self, provider: &ProviderTarget, key: &str, estimate: Money, adopted: bool) -> Result<bool, String> {
        let mut ledger = self.ledger();
        let id = (provider.clone(), key.to_owned());
        if ledger.debits.contains_key(&id) {
            return Ok(false);
        }
        let currency = estimate.currency;
        let limit = ledger.limits.get(&currency).copied().unwrap_or(0);
        let reserved = ledger.reserved.get(&currency).copied().unwrap_or(0);
        let after = reserved.checked_add(estimate.micros);
        let full = ledger.debits.len() >= MAX_CEILING_RESERVATIONS;
        if !adopted {
            if after.is_none_or(|after| after > limit) {
                return Err(if limit == 0 {
                    format!("the run's accepted amounts include nothing in {currency}, so this job can't be charged")
                } else {
                    format!("the run would reserve more than the {currency} amount that was accepted; quote and accept again to continue")
                });
            }
            if full {
                return Err("the run has reserved too many jobs to track; quote and start a new run".into());
            }
        }
        ledger.reserved.insert(currency, after.unwrap_or(u64::MAX));
        if full {
            return Ok(false);
        }
        ledger.debits.insert(id, estimate);
        Ok(true)
    }

    /// Credits a priced reservation released without reaching the provider. A reservation this
    /// run didn't debit, or one already credited, changes nothing.
    pub(crate) fn credit(&self, provider: &ProviderTarget, key: &str) {
        let mut ledger = self.ledger();
        if let Some(money) = ledger.debits.remove(&(provider.clone(), key.to_owned())) {
            let reserved = ledger.reserved.entry(money.currency).or_default();
            *reserved = reserved.saturating_sub(money.micros);
        }
    }

    /// Admits one unknown-price job, using one of the accepted count unless the run already
    /// admitted this provider and key (a retry of the same submission). Adopted work is counted
    /// even past the count. Never given back.
    pub(crate) fn admit_unpriced(&self, provider: &ProviderTarget, key: &str, adopted: bool) -> Result<(), String> {
        let mut ledger = self.ledger();
        let id = (provider.clone(), key.to_owned());
        if ledger.unpriced.contains(&id) {
            return Ok(());
        }
        let full = ledger.unpriced.len() >= MAX_CEILING_RESERVATIONS;
        if !adopted {
            let limit = ledger.unpriced_limit;
            if ledger.unpriced_used() >= limit {
                return Err(if limit == 0 {
                    "the run's acceptance allows no jobs of unknown price".into()
                } else {
                    format!("the run's accepted {limit} jobs of unknown price are used up; quote and accept again to continue")
                });
            }
            if full {
                return Err("the run has admitted too many jobs to track; quote and start a new run".into());
            }
        }
        if full {
            ledger.unpriced_overflow = ledger.unpriced_overflow.saturating_add(1);
        } else {
            ledger.unpriced.insert(id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

//! Budgets enforced while jobs run (#207). [`cost::check`](crate::cost::check) prices a whole run
//! before it starts; the [`Ledger`] holds each job to the caps when it is about to start:
//!
//! - **Reservations:** a job reserves its estimate before it is submitted, so jobs running in
//!   parallel (or runs started back to back) can't together pass a cap. A job that would break a
//!   cap is refused before it reaches the provider.
//! - **Settling:** when a job ends, the provider's reported cost (or the estimate when it reported
//!   none) is charged and recorded in the [`UsageLog`], once per provider and idempotency key, so
//!   a retry or resume is one charge.
//! - **Resumed jobs** (a stored handle after a restart) are adopted without a cap check: the job
//!   already runs on the provider, so the money is committed either way.
//! - **Releasing:** a job that certainly never reached the provider (refused, bad input or key)
//!   gives its reservation back without a charge.
//!
//! Caps are in their own currency; an estimate in another currency can't be compared and is
//! refused (as `cost::check` does). Spend is read from the usage log, which keeps evicted spend.

use crate::cost::{Budgets, Charge, MAX_CHARGE_MICROS, MAX_TEXT, UsageEntry, UsageLog, format_money};
use crate::scheduler::JobId;
use nodecraft_types::{Currency, Error, ErrorKind, Money, NodeId};
use std::collections::BTreeMap;
use std::fmt;

/// Error messages of jobs the budget refused start with this, so frontends can show "over
/// budget" and the scheduler treats them as final.
pub const REFUSAL_PREFIX: &str = "budget: ";

/// Whether a job error came from a budget refusal.
pub fn is_refusal(error: &str) -> bool {
    error.starts_with(REFUSAL_PREFIX)
}

/// Why the ledger refused to start a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    PerCall {
        estimate: Money,
        cap: Money,
    },
    Session {
        would_spend: Money,
        cap: Money,
    },
    Project {
        would_spend: Money,
        cap: Money,
    },
    /// A cap is set and the job's price isn't known.
    Unpriced,
    /// The estimate is in a different currency from a cap.
    Currency {
        cap: Currency,
        estimate: Currency,
    },
    /// The job already holds a reservation.
    AlreadyReserved(JobId),
    /// The job has no idempotency key, so its charge couldn't be told from a retry's.
    MissingKey,
    /// The job's provider, model or key is too long for the usage log, so it couldn't be charged.
    Unrecordable,
    /// The totals don't fit in the money type.
    Overflow,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PerCall { estimate, cap } => {
                write!(f, "this job costs about {}, more than the per-call cap of {}", format_money(*estimate), format_money(*cap))
            }
            Self::Session { would_spend, cap } => {
                write!(f, "this would bring the session's spend to {}, over its cap of {}", format_money(*would_spend), format_money(*cap))
            }
            Self::Project { would_spend, cap } => {
                write!(f, "this would bring the project's spend to {}, over its cap of {}", format_money(*would_spend), format_money(*cap))
            }
            Self::Unpriced => f.write_str("the price of this job is unknown and a spending cap is set"),
            Self::Currency { cap, estimate } => write!(f, "the cap is in {}, but the price is in {}", String::from(*cap), String::from(*estimate)),
            Self::AlreadyReserved(job) => write!(f, "{job} already holds a budget reservation"),
            Self::MissingKey => f.write_str("the job has no idempotency key, so its charge can't be tracked"),
            Self::Unrecordable => f.write_str("the job's provider, model or key is too long to record its charge"),
            Self::Overflow => f.write_str("the amounts are too large"),
        }
    }
}

impl Refusal {
    /// The job error for a refused job (non-transient, with [`REFUSAL_PREFIX`]).
    pub fn job_error(&self) -> String {
        format!("{REFUSAL_PREFIX}not started: {self}")
    }
}

/// What a job reserved and what it is, for the usage log.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reservation {
    estimate: Option<Money>,
}

/// See the module docs.
#[derive(Clone, Debug)]
pub struct Ledger {
    budgets: Budgets,
    allow_unpriced: bool,
    session: String,
    usage: UsageLog,
    reserved: BTreeMap<JobId, Reservation>,
    reserved_totals: BTreeMap<Currency, Money>,
    /// Charges the usage log refused: they no longer hold a reservation but still count against
    /// the caps for the rest of the session.
    unrecorded: BTreeMap<Currency, Money>,
}

/// Room left in a usage key for the `#n` suffix of resubmissions.
pub const KEY_SUFFIX_ROOM: usize = 12;

fn add_to(map: &mut BTreeMap<Currency, Money>, amount: Money) -> Result<(), Refusal> {
    let total = map.entry(amount.currency).or_insert_with(|| Money::zero(amount.currency));
    *total = total.checked_add(amount).map_err(|_| Refusal::Overflow)?;
    Ok(())
}

impl Ledger {
    /// A ledger for `session` with the project's usage so far (`usage`, restored from the
    /// project) and the user's caps. `allow_unpriced` lets jobs with no known price run while a
    /// cap is set (they could spend anything).
    pub fn new(budgets: Budgets, allow_unpriced: bool, session: impl Into<String>, usage: UsageLog) -> Self {
        Self {
            budgets,
            allow_unpriced,
            session: session.into(),
            usage,
            reserved: BTreeMap::new(),
            reserved_totals: BTreeMap::new(),
            unrecorded: BTreeMap::new(),
        }
    }

    /// Changes the caps (the user edited them). Reservations already made stand.
    pub fn set_budgets(&mut self, budgets: Budgets, allow_unpriced: bool) {
        self.budgets = budgets;
        self.allow_unpriced = allow_unpriced;
    }

    pub fn usage(&self) -> &UsageLog {
        &self.usage
    }

    pub fn is_reserved(&self, job: JobId) -> bool {
        self.reserved.contains_key(&job)
    }

    /// What running jobs have reserved, per currency.
    pub fn reserved(&self) -> &BTreeMap<Currency, Money> {
        &self.reserved_totals
    }

    /// Charges the usage log refused this session, per currency (they still count).
    pub fn unrecorded(&self) -> &BTreeMap<Currency, Money> {
        &self.unrecorded
    }

    /// Whether a job's charge could be recorded: its provider, model and key (with room for a
    /// resubmission suffix) and this ledger's session fit the usage log. Check before submitting.
    pub fn recordable(&self, provider: &str, model: &str, idempotency_key: &str) -> Result<(), Refusal> {
        if idempotency_key.is_empty() {
            return Err(Refusal::MissingKey);
        }
        let too_long = [provider.len(), model.len(), self.session.len(), idempotency_key.len().saturating_add(KEY_SUFFIX_ROOM)].iter().any(|n| *n > MAX_TEXT);
        if too_long { Err(Refusal::Unrecordable) } else { Ok(()) }
    }

    fn any_cap(&self) -> bool {
        self.budgets.per_call.is_some() || self.budgets.session.is_some() || self.budgets.project.is_some()
    }

    /// Whether `estimate` fits the caps on top of what is spent and reserved.
    fn check(&self, estimate: Option<Money>) -> Result<(), Refusal> {
        let Some(estimate) = estimate else {
            return if self.any_cap() && !self.allow_unpriced { Err(Refusal::Unpriced) } else { Ok(()) };
        };
        // A free job adds nothing, even once spend is over a cap (as `cost::check`).
        if estimate.micros == 0 {
            return Ok(());
        }
        // No job costs this much; the usage log would refuse the charge.
        if estimate.micros > MAX_CHARGE_MICROS {
            return Err(Refusal::Overflow);
        }
        if let Some(cap) = self.budgets.per_call {
            if cap.currency != estimate.currency {
                return Err(Refusal::Currency { cap: cap.currency, estimate: estimate.currency });
            }
            if estimate.micros > cap.micros {
                return Err(Refusal::PerCall { estimate, cap });
            }
        }
        let spent = self.usage.spent(&self.session);
        let zero = Money::zero(estimate.currency);
        let reserved = self
            .reserved_totals
            .get(&estimate.currency)
            .copied()
            .unwrap_or(zero)
            .checked_add(self.unrecorded.get(&estimate.currency).copied().unwrap_or(zero))
            .map_err(|_| Refusal::Overflow)?;
        for (cap, already, project) in [(self.budgets.session, &spent.session, false), (self.budgets.project, &spent.project, true)] {
            let Some(cap) = cap else { continue };
            if cap.currency != estimate.currency {
                return Err(Refusal::Currency { cap: cap.currency, estimate: estimate.currency });
            }
            let before = already.get(&cap.currency).copied().unwrap_or(Money::zero(cap.currency));
            let would_spend = before.checked_add(reserved).and_then(|m| m.checked_add(estimate)).map_err(|_| Refusal::Overflow)?;
            if would_spend.micros > cap.micros {
                return Err(if project { Refusal::Project { would_spend, cap } } else { Refusal::Session { would_spend, cap } });
            }
        }
        Ok(())
    }

    /// Reserves `estimate` for `job` before it starts, or refuses with the cap it would break.
    /// An unpriced job reserves nothing (and is refused while a cap is set, unless allowed).
    pub fn reserve(&mut self, job: JobId, estimate: Option<Money>) -> Result<(), Refusal> {
        if self.reserved.contains_key(&job) {
            return Err(Refusal::AlreadyReserved(job));
        }
        self.check(estimate)?;
        if let Some(amount) = estimate {
            add_to(&mut self.reserved_totals, amount)?;
        }
        self.reserved.insert(job, Reservation { estimate });
        Ok(())
    }

    /// Takes on a job already running on the provider (resumed after a restart): its estimate is
    /// held without a cap check. Adopting a job that is already reserved changes nothing.
    pub fn adopt(&mut self, job: JobId, estimate: Option<Money>) -> Result<(), Refusal> {
        if self.reserved.contains_key(&job) {
            return Ok(());
        }
        if let Some(amount) = estimate {
            add_to(&mut self.reserved_totals, amount)?;
        }
        self.reserved.insert(job, Reservation { estimate });
        Ok(())
    }

    /// Settles a job that ran or may have run: charges the provider's reported `actual` cost, or
    /// the estimate when it reported none, records it, and frees the reservation. Returns what
    /// was charged (`None` when nothing was known to charge). A retry or resume under the same
    /// provider and idempotency key is charged once; a reported cost for a key charged as
    /// unresolved before settles that charge.
    ///
    /// A reported cost is hostile input: one in another currency than the estimate, or larger
    /// than any job could cost, is ignored and the estimate is charged as unresolved. If the
    /// charge can't be recorded (`Err`), it still counts against the caps for the session
    /// ([`Ledger::unrecorded`]).
    pub fn commit(&mut self, job: JobId, charge: Settlement<'_>) -> Result<Option<Money>, Error> {
        let reservation = self.reserved.get(&job).cloned().ok_or_else(|| Error::new(ErrorKind::NotFound, format!("{job} has no budget reservation")))?;
        let actual = charge.actual.filter(|actual| actual.micros <= MAX_CHARGE_MICROS && reservation.estimate.is_none_or(|e| e.currency == actual.currency));
        let (cost, status) = match (actual, reservation.estimate) {
            (Some(actual), _) => (actual, Charge::Actual),
            (None, Some(estimate)) if charge.actual.is_some() => (estimate, Charge::Unresolved),
            (None, Some(estimate)) => (estimate, if charge.unresolved { Charge::Unresolved } else { Charge::Actual }),
            (None, None) => {
                self.unreserve(job);
                // An unpriced job whose reported cost can't be trusted: nothing sound to charge.
                return match charge.actual {
                    Some(_) => Err(Error::new(ErrorKind::InvalidInput, "the provider reported an implausible cost for an unpriced job")),
                    None => Ok(None),
                };
            }
        };
        let entry = UsageEntry {
            at_ms: charge.at_ms,
            session: self.session.clone(),
            node: charge.node,
            provider: charge.provider.to_owned(),
            model: charge.model.to_owned(),
            cost,
            idempotency_key: charge.idempotency_key.to_owned(),
            status,
        };
        self.unreserve(job);
        match self.usage.record(entry) {
            Ok(true) => Ok(Some(cost)),
            // Already charged under this key: a reported cost settles an unresolved charge.
            Ok(false) if status == Charge::Actual && actual.is_some() => {
                Ok(self.usage.settle(charge.provider, charge.idempotency_key, cost, Charge::Actual).is_ok().then_some(cost))
            }
            Ok(false) => Ok(None),
            Err(error) => {
                let _ = add_to(&mut self.unrecorded, cost);
                Err(error)
            }
        }
    }

    /// Gives a job's reservation back without charging: it certainly never reached the provider.
    pub fn release(&mut self, job: JobId) -> bool {
        self.unreserve(job)
    }

    fn unreserve(&mut self, job: JobId) -> bool {
        let Some(reservation) = self.reserved.remove(&job) else { return false };
        if let Some(amount) = reservation.estimate
            && let Some(total) = self.reserved_totals.get_mut(&amount.currency)
        {
            *total = total.checked_sub(amount).unwrap_or(Money::zero(amount.currency));
        }
        true
    }
}

/// What to record when a job is settled.
#[derive(Clone, Copy, Debug)]
pub struct Settlement<'a> {
    /// The provider's reported cost, if any.
    pub actual: Option<Money>,
    /// Without a reported cost, whether the outcome is unknown (the job may or may not have been
    /// billed: [`Charge::Unresolved`]) rather than finished ([`Charge::Actual`] at the estimate).
    pub unresolved: bool,
    pub provider: &'a str,
    pub model: &'a str,
    pub idempotency_key: &'a str,
    pub node: NodeId,
    /// Wall-clock milliseconds since the Unix epoch.
    pub at_ms: u64,
}

#[cfg(test)]
mod tests;

//! Cost quotes and budgets: runs cost money, so every paid job is priced before it starts and
//! held against the user's caps.
//!
//! The [`Ledger`] tracks, in one currency:
//! - **reservations**: a job's estimate is reserved before it starts, so jobs running in parallel
//!   can't together overshoot a cap;
//! - **spend**: when a job ends its actual cost (or, when the provider doesn't report one, its
//!   estimate) is committed;
//! - **caps**: a hard per-call cap, a per-session cap, and a per-project cap (the project's spend
//!   so far comes from the project file).
//!
//! A [`Quote`] prices a whole run up front (the cost on the Generate button and in run
//! confirmations) and says which cap it would break. The usage log exports as CSV.

use std::collections::BTreeMap;
use std::fmt;

use nodecraft_types::{Currency, Money};

use crate::scheduler::JobId;

/// Cap on usage-log rows kept in memory (older rows are dropped; export regularly).
pub const MAX_LOG_ROWS: usize = 100_000;

/// The user's spending caps. `None` means no cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BudgetLimits {
    /// Most one job may cost.
    pub per_call: Option<Money>,
    /// Most this app session may spend.
    pub per_session: Option<Money>,
    /// Most this project may spend in total (including earlier sessions).
    pub per_project: Option<Money>,
    /// Allow jobs whose price isn't known. When any cap is set this defaults to `false`: an
    /// unpriced job could spend anything.
    pub allow_unpriced: bool,
}

/// Which cap a job or run would break.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BudgetError {
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
    /// A cap is set and the price of this job isn't known.
    Unpriced,
    /// The amount is in a different currency than the budget.
    Currency {
        expected: Currency,
        got: Currency,
    },
    /// The total doesn't fit in the money type.
    Overflow,
    /// No reservation for this job (it wasn't reserved, or was already settled).
    NotReserved(JobId),
}

impl fmt::Display for BudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PerCall { estimate, cap } => write!(f, "this job costs about {estimate}, more than the per-call cap of {cap}"),
            Self::Session { would_spend, cap } => write!(f, "this would bring the session's spend to {would_spend}, over its cap of {cap}"),
            Self::Project { would_spend, cap } => write!(f, "this would bring the project's spend to {would_spend}, over its cap of {cap}"),
            Self::Unpriced => f.write_str("the price of this job is unknown and a spending cap is set; allow unpriced jobs to run it"),
            Self::Currency { expected, got } => write!(f, "the budget is in {expected}, but the price is in {got}"),
            Self::Overflow => f.write_str("the amount is too large"),
            Self::NotReserved(job) => write!(f, "{job} has no budget reservation"),
        }
    }
}

impl std::error::Error for BudgetError {}

/// One priced job in a quote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Priced {
    pub job: JobId,
    /// `None` when the price isn't known (e.g. pay-per-second video with an unknown length).
    pub estimate: Option<Money>,
}

/// The price of a run before it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    /// Sum of the known estimates.
    pub total: Money,
    /// Jobs whose price isn't known (shown as "+ n unpriced").
    pub unpriced: usize,
    pub jobs: usize,
    /// The first cap the run would break, if any. The run can still be confirmed partly: the
    /// ledger refuses each job that would break a cap when it is about to start.
    pub over: Option<BudgetError>,
}

/// One row of the usage log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    /// When the job ended, in milliseconds since the Unix epoch (from the caller's wall clock).
    pub at_ms: u64,
    pub job: JobId,
    pub provider: String,
    pub model: String,
    pub estimate: Option<Money>,
    /// What the provider reported; `None` when it didn't (the estimate was charged instead).
    pub actual: Option<Money>,
    pub charged: Money,
}

/// See the module docs.
#[derive(Debug, Clone)]
pub struct Ledger {
    currency: Currency,
    limits: BudgetLimits,
    project_before: Money,
    session_spent: Money,
    reserved: BTreeMap<JobId, Money>,
    log: Vec<UsageRecord>,
}

impl Ledger {
    /// A ledger in `currency`; `project_spent` is what the project spent in earlier sessions.
    pub fn new(currency: Currency, limits: BudgetLimits, project_spent: Money) -> Result<Self, BudgetError> {
        let ledger =
            Self { currency, limits, project_before: Money::zero(currency), session_spent: Money::zero(currency), reserved: BTreeMap::new(), log: Vec::new() };
        ledger.same_currency(project_spent)?;
        for cap in [limits.per_call, limits.per_session, limits.per_project].into_iter().flatten() {
            ledger.same_currency(cap)?;
        }
        Ok(Self { project_before: project_spent, ..ledger })
    }

    fn same_currency(&self, m: Money) -> Result<Money, BudgetError> {
        if m.currency == self.currency { Ok(m) } else { Err(BudgetError::Currency { expected: self.currency, got: m.currency }) }
    }

    fn add(a: Money, b: Money) -> Result<Money, BudgetError> {
        a.checked_add(b).map_err(|_| BudgetError::Overflow)
    }

    pub fn limits(&self) -> BudgetLimits {
        self.limits
    }

    /// Changes the caps (the user edited them). Reservations already made stand.
    pub fn set_limits(&mut self, limits: BudgetLimits) -> Result<(), BudgetError> {
        for cap in [limits.per_call, limits.per_session, limits.per_project].into_iter().flatten() {
            self.same_currency(cap)?;
        }
        self.limits = limits;
        Ok(())
    }

    fn reserved_total(&self) -> Result<Money, BudgetError> {
        Money::checked_sum(self.currency, self.reserved.values().copied()).map_err(|_| BudgetError::Overflow)
    }

    /// What the session has spent plus what running jobs have reserved.
    pub fn session_committed(&self) -> Result<Money, BudgetError> {
        Self::add(self.session_spent, self.reserved_total()?)
    }

    pub fn session_spent(&self) -> Money {
        self.session_spent
    }

    /// The project's spend including this session (to save in the project file).
    pub fn project_spent(&self) -> Result<Money, BudgetError> {
        Self::add(self.project_before, self.session_spent)
    }

    /// Checks that `extra` on top of what is spent and reserved stays within the caps.
    fn check(&self, estimate: Option<Money>, extra: Money) -> Result<(), BudgetError> {
        let any_cap = self.limits.per_call.is_some() || self.limits.per_session.is_some() || self.limits.per_project.is_some();
        let Some(estimate) = estimate else {
            return if any_cap && !self.limits.allow_unpriced { Err(BudgetError::Unpriced) } else { Ok(()) };
        };
        self.same_currency(estimate)?;
        if !any_cap {
            return Ok(());
        }
        if let Some(cap) = self.limits.per_call
            && exceeds(estimate, cap)
        {
            return Err(BudgetError::PerCall { estimate, cap });
        }
        let session = Self::add(self.session_committed()?, extra)?;
        if let Some(cap) = self.limits.per_session
            && exceeds(session, cap)
        {
            return Err(BudgetError::Session { would_spend: session, cap });
        }
        let project = Self::add(Self::add(self.project_before, self.reserved_total()?)?, Self::add(self.session_spent, extra)?)?;
        if let Some(cap) = self.limits.per_project
            && exceeds(project, cap)
        {
            return Err(BudgetError::Project { would_spend: project, cap });
        }
        Ok(())
    }

    /// Prices a run before it starts.
    pub fn quote(&self, jobs: &[Priced]) -> Result<Quote, BudgetError> {
        let mut total = Money::zero(self.currency);
        let mut unpriced = 0usize;
        let mut over = None;
        for p in jobs {
            match p.estimate {
                Some(e) => {
                    self.same_currency(e)?;
                    if over.is_none()
                        && let Some(cap) = self.limits.per_call
                        && exceeds(e, cap)
                    {
                        over = Some(BudgetError::PerCall { estimate: e, cap });
                    }
                    total = Self::add(total, e)?;
                }
                None => unpriced += 1,
            }
        }
        if over.is_none() && unpriced > 0 {
            over = self.check(None, Money::zero(self.currency)).err();
        }
        if over.is_none() {
            // The whole run against the session and project caps (not the per-call one, done above).
            over = match self.check(Some(Money::zero(self.currency)), total) {
                Err(e @ (BudgetError::Session { .. } | BudgetError::Project { .. })) => Some(e),
                Err(e) => return Err(e),
                Ok(()) => None,
            };
        }
        Ok(Quote { total, unpriced, jobs: jobs.len(), over })
    }

    /// Reserves `estimate` for `job` before it starts, or refuses with the cap it would break.
    /// Unpriced jobs reserve nothing (and are refused when a cap is set, unless allowed).
    pub fn reserve(&mut self, job: JobId, estimate: Option<Money>) -> Result<(), BudgetError> {
        let amount = estimate.unwrap_or(Money::zero(self.currency));
        self.check(estimate, amount)?;
        self.reserved.insert(job, amount);
        Ok(())
    }

    /// Settles a job that ran: charges what the provider reported, or the estimate when it
    /// reported nothing, and logs it.
    #[allow(clippy::too_many_arguments)]
    pub fn commit(&mut self, job: JobId, actual: Option<Money>, provider: &str, model: &str, at_ms: u64) -> Result<Money, BudgetError> {
        let estimate = self.reserved.get(&job).copied().ok_or(BudgetError::NotReserved(job))?;
        if let Some(a) = actual {
            self.same_currency(a)?;
        }
        let charged = actual.unwrap_or(estimate);
        self.session_spent = Self::add(self.session_spent, charged)?;
        self.reserved.remove(&job);
        if self.log.len() >= MAX_LOG_ROWS {
            self.log.remove(0);
        }
        let estimate = (estimate.micros > 0).then_some(estimate);
        self.log.push(UsageRecord { at_ms, job, provider: provider.to_owned(), model: model.to_owned(), estimate, actual, charged });
        Ok(charged)
    }

    /// Releases a job's reservation without charging (it never reached the provider, or was
    /// cancelled before it could be billed).
    pub fn release(&mut self, job: JobId) -> bool {
        self.reserved.remove(&job).is_some()
    }

    pub fn log(&self) -> &[UsageRecord] {
        &self.log
    }

    /// The usage log as CSV (RFC 4180), safe to open in a spreadsheet: cells that a spreadsheet
    /// would run as a formula are prefixed with `'`.
    pub fn to_csv(&self) -> String {
        let mut out = String::from("time_ms,job,provider,model,currency,estimate,actual,charged\r\n");
        let money = |m: Option<Money>| m.map_or_else(String::new, |m| decimal(m.micros));
        for r in &self.log {
            let row = [
                r.at_ms.to_string(),
                r.job.0.to_string(),
                cell(&r.provider),
                cell(&r.model),
                self.currency.to_string(),
                money(r.estimate),
                money(r.actual),
                decimal(r.charged.micros),
            ];
            out.push_str(&row.join(","));
            out.push_str("\r\n");
        }
        out
    }
}

/// Micro-units as a decimal amount (`1234567` → `1.234567`).
/// `a > b` for amounts already known to share the ledger's currency.
fn exceeds(a: Money, b: Money) -> bool {
    a.checked_cmp(b).is_ok_and(|o| o == std::cmp::Ordering::Greater)
}

fn decimal(micros: u64) -> String {
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

/// A CSV cell: quoted when needed, formula-injection safe.
fn cell(text: &str) -> String {
    let mut t: String = text.chars().filter(|c| !c.is_control()).take(256).collect();
    if t.starts_with(['=', '+', '-', '@']) {
        t.insert(0, '\'');
    }
    if t.contains([',', '"']) || t.starts_with(' ') || t.ends_with(' ') { format!("\"{}\"", t.replace('"', "\"\"")) } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(micros: u64) -> Money {
        Money::new(Currency::USD, micros)
    }

    fn caps(call: Option<u64>, session: Option<u64>, project: Option<u64>) -> BudgetLimits {
        BudgetLimits { per_call: call.map(usd), per_session: session.map(usd), per_project: project.map(usd), allow_unpriced: false }
    }

    #[test]
    fn reservations_hold_parallel_jobs_within_the_session_cap() {
        let mut l = Ledger::new(Currency::USD, caps(None, Some(1_000_000), None), usd(0)).unwrap();
        l.reserve(JobId(1), Some(usd(400_000))).unwrap();
        l.reserve(JobId(2), Some(usd(400_000))).unwrap();
        // A third would overshoot while the first two still run.
        assert_eq!(l.reserve(JobId(3), Some(usd(400_000))), Err(BudgetError::Session { would_spend: usd(1_200_000), cap: usd(1_000_000) }));
        // The first job turned out cheaper than estimated: room for the third.
        assert_eq!(l.commit(JobId(1), Some(usd(100_000)), "fal", "flux", 1).unwrap(), usd(100_000));
        l.reserve(JobId(3), Some(usd(400_000))).unwrap();
        assert_eq!(l.session_committed().unwrap(), usd(900_000));
    }

    #[test]
    fn per_call_and_project_caps() {
        let mut l = Ledger::new(Currency::USD, caps(Some(500_000), None, Some(2_000_000)), usd(1_800_000)).unwrap();
        assert!(matches!(l.reserve(JobId(1), Some(usd(600_000))), Err(BudgetError::PerCall { .. })));
        assert!(matches!(l.reserve(JobId(1), Some(usd(300_000))), Err(BudgetError::Project { .. })), "1.8 already spent in earlier sessions");
        l.reserve(JobId(1), Some(usd(150_000))).unwrap();
        l.commit(JobId(1), None, "fal", "flux", 1).unwrap();
        assert_eq!(l.project_spent().unwrap(), usd(1_950_000), "no reported cost: the estimate is charged");
    }

    #[test]
    fn unpriced_jobs_need_permission_when_capped() {
        let mut l = Ledger::new(Currency::USD, caps(None, Some(1_000_000), None), usd(0)).unwrap();
        assert_eq!(l.reserve(JobId(1), None), Err(BudgetError::Unpriced));
        let mut allow = caps(None, Some(1_000_000), None);
        allow.allow_unpriced = true;
        l.set_limits(allow).unwrap();
        l.reserve(JobId(1), None).unwrap();
        l.commit(JobId(1), Some(usd(250_000)), "veo", "veo-3", 1).unwrap();
        assert_eq!(l.session_spent(), usd(250_000), "the reported cost still counts");
        let mut free = Ledger::new(Currency::USD, BudgetLimits::default(), usd(0)).unwrap();
        free.reserve(JobId(2), None).unwrap();
    }

    #[test]
    fn quotes_price_runs_and_name_the_broken_cap() {
        let l = Ledger::new(Currency::USD, caps(Some(500_000), Some(1_000_000), None), usd(0)).unwrap();
        let q = l.quote(&[Priced { job: JobId(1), estimate: Some(usd(300_000)) }, Priced { job: JobId(2), estimate: Some(usd(300_000)) }]).unwrap();
        assert_eq!((q.total, q.unpriced, q.jobs, q.over), (usd(600_000), 0, 2, None));
        let q = l.quote(&(0..4).map(|i| Priced { job: JobId(i), estimate: Some(usd(300_000)) }).collect::<Vec<_>>()).unwrap();
        assert_eq!(q.over, Some(BudgetError::Session { would_spend: usd(1_200_000), cap: usd(1_000_000) }));
        let q = l.quote(&[Priced { job: JobId(1), estimate: Some(usd(700_000)) }]).unwrap();
        assert!(matches!(q.over, Some(BudgetError::PerCall { .. })));
        let q = l.quote(&[Priced { job: JobId(1), estimate: None }]).unwrap();
        assert_eq!((q.unpriced, q.over), (1, Some(BudgetError::Unpriced)));
    }

    #[test]
    fn currencies_settling_and_overflow() {
        let eur = Money::new(Currency::EUR, 1);
        assert!(Ledger::new(Currency::USD, BudgetLimits { per_session: Some(eur), ..BudgetLimits::default() }, usd(0)).is_err());
        let mut l = Ledger::new(Currency::USD, BudgetLimits::default(), usd(0)).unwrap();
        assert!(matches!(l.reserve(JobId(1), Some(eur)), Err(BudgetError::Currency { .. })));
        assert_eq!(l.commit(JobId(9), None, "p", "m", 0), Err(BudgetError::NotReserved(JobId(9))));
        l.reserve(JobId(1), Some(usd(5))).unwrap();
        assert!(l.release(JobId(1)));
        assert!(!l.release(JobId(1)));
        assert_eq!(l.session_committed().unwrap(), usd(0), "released, not charged");
        l.reserve(JobId(2), Some(usd(u64::MAX))).unwrap();
        assert_eq!(l.reserve(JobId(3), Some(usd(1))), Ok(()), "no caps: nothing to check against");
        assert_eq!(l.session_committed(), Err(BudgetError::Overflow));
    }

    #[test]
    fn csv_export_is_spreadsheet_safe() {
        let mut l = Ledger::new(Currency::USD, BudgetLimits::default(), usd(0)).unwrap();
        l.reserve(JobId(1), Some(usd(1_234_567))).unwrap();
        l.commit(JobId(1), Some(usd(1_000_000)), "=HYPERLINK(\"evil\")", "flux, dev", 1700).unwrap();
        l.reserve(JobId(2), None).unwrap();
        l.commit(JobId(2), None, "ollama", "llama", 1800).unwrap();
        let csv = l.to_csv();
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(lines[0], "time_ms,job,provider,model,currency,estimate,actual,charged");
        assert_eq!(lines[1], "1700,1,\"'=HYPERLINK(\"\"evil\"\")\",\"flux, dev\",USD,1.234567,1.000000,1.000000");
        assert_eq!(lines[2], "1800,2,ollama,llama,USD,,,0.000000");
    }
}

//! Cost quotes and budgets: runs cost money, so every paid job is priced before it starts and
//! held against the user's caps.
//!
//! The [`Ledger`] tracks, in one currency:
//! - **reservations**: a job's estimate is reserved before it starts, so jobs running in parallel
//!   can't together overshoot a cap;
//! - **spend**: when a job ends its actual cost (or, when the provider doesn't report one, its
//!   estimate) is committed, with how it ended;
//! - **caps**: a hard per-call cap, a per-session cap, and a per-project cap (the project's spend
//!   so far comes from the project file).
//!
//! Money that may have been billed is never dropped: a job that reached the provider, or whose
//! submit had an uncertain outcome, is charged its estimate if it ends without a reported cost.
//! A [`Quote`] prices a whole run up front and lists every cap it would break. The usage log
//! exports as CSV.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use nodecraft_types::{Currency, Money};

use crate::scheduler::JobId;

/// Cap on usage-log rows kept in memory; older rows are dropped (and counted).
pub const MAX_LOG_ROWS: usize = 100_000;

/// Error messages of jobs the budget refused start with this, so frontends can show "over budget".
pub const REFUSAL_PREFIX: &str = "budget: ";

/// True for a job error produced by a budget refusal.
pub fn is_budget_refusal(error: &str) -> bool {
    error.starts_with(REFUSAL_PREFIX)
}

/// The user's spending caps. `None` means no cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BudgetLimits {
    /// Most one job may cost.
    pub per_call: Option<Money>,
    /// Most this app session may spend.
    pub per_session: Option<Money>,
    /// Most this project may spend in total (including earlier sessions).
    pub per_project: Option<Money>,
    /// Allow jobs whose price isn't known while a cap is set (they could spend anything).
    pub allow_unpriced: bool,
}

impl BudgetLimits {
    fn any_cap(&self) -> bool {
        self.per_call.is_some() || self.per_session.is_some() || self.per_project.is_some()
    }
}

/// Which cap a job or run would break, or why the ledger can't account for it.
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
    /// The job already holds a reservation; settle it before reserving again.
    AlreadyReserved(JobId),
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
            Self::AlreadyReserved(job) => write!(f, "{job} already holds a budget reservation"),
        }
    }
}

impl std::error::Error for BudgetError {}

/// The price of a run before it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    /// Sum of the known estimates in the budget's currency.
    pub total: Money,
    /// Jobs whose price isn't known, or is in another currency or in provider credits.
    pub unpriced: usize,
    pub jobs: usize,
    /// Every cap the run would break (empty when it fits). The run can still be confirmed partly:
    /// the ledger refuses each job that would break a cap when it is about to start.
    pub over: Vec<BudgetError>,
}

/// How a job ended, for the usage log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    Done,
    Failed,
    Cancelled,
    /// It may or may not have run on the provider (a lost reply, a cancel during submit).
    Unresolved,
}

impl Settled {
    fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unresolved => "unresolved",
        }
    }
}

/// One row of the usage log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    /// When the job ended, in milliseconds since the Unix epoch (from the caller's wall clock).
    pub at_ms: u64,
    pub job: JobId,
    pub provider: String,
    pub model: String,
    pub outcome: Settled,
    /// The quoted price; `None` when it wasn't known.
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
    reserved: BTreeMap<JobId, Option<Money>>,
    reserved_total: Money,
    log: VecDeque<UsageRecord>,
    dropped_rows: u64,
}

impl Ledger {
    /// A ledger in `currency`; `project_spent` is what the project spent in earlier sessions.
    pub fn new(currency: Currency, limits: BudgetLimits, project_spent: Money) -> Result<Self, BudgetError> {
        let zero = Money::zero(currency);
        let mut ledger = Self {
            currency,
            limits: BudgetLimits::default(),
            project_before: zero,
            session_spent: zero,
            reserved: BTreeMap::new(),
            reserved_total: zero,
            log: VecDeque::new(),
            dropped_rows: 0,
        };
        ledger.project_before = ledger.same_currency(project_spent)?;
        ledger.set_limits(limits)?;
        Ok(ledger)
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

    /// What the session has spent plus what running jobs have reserved.
    pub fn session_committed(&self) -> Result<Money, BudgetError> {
        Self::add(self.session_spent, self.reserved_total)
    }

    pub fn session_spent(&self) -> Money {
        self.session_spent
    }

    /// The project's spend including this session (to save in the project file).
    pub fn project_spent(&self) -> Result<Money, BudgetError> {
        Self::add(self.project_before, self.session_spent)
    }

    pub fn is_reserved(&self, job: JobId) -> bool {
        self.reserved.contains_key(&job)
    }

    /// Checks that `extra` on top of what is spent and reserved stays within the caps. The sums
    /// are always checked, so a hostile estimate can't overflow the ledger even without caps.
    fn check(&self, estimate: Option<Money>, extra: Money) -> Result<(), BudgetError> {
        let Some(estimate) = estimate else {
            return if self.limits.any_cap() && !self.limits.allow_unpriced { Err(BudgetError::Unpriced) } else { Ok(()) };
        };
        self.same_currency(estimate)?;
        let session = Self::add(self.session_committed()?, extra)?;
        let project = Self::add(self.project_before, session)?;
        if let Some(cap) = self.limits.per_call
            && exceeds(estimate, cap)
        {
            return Err(BudgetError::PerCall { estimate, cap });
        }
        if let Some(cap) = self.limits.per_session
            && exceeds(session, cap)
        {
            return Err(BudgetError::Session { would_spend: session, cap });
        }
        if let Some(cap) = self.limits.per_project
            && exceeds(project, cap)
        {
            return Err(BudgetError::Project { would_spend: project, cap });
        }
        Ok(())
    }

    /// Prices a run before it starts. Estimates in another currency (or credits) count as
    /// unpriced rather than failing the quote.
    pub fn quote(&self, estimates: &[Option<Money>]) -> Result<Quote, BudgetError> {
        let mut total = Money::zero(self.currency);
        let mut unpriced = 0usize;
        let mut over = Vec::new();
        for e in estimates {
            match e {
                Some(e) if e.currency == self.currency => {
                    if let Some(cap) = self.limits.per_call
                        && exceeds(*e, cap)
                        && !over.iter().any(|o| matches!(o, BudgetError::PerCall { .. }))
                    {
                        over.push(BudgetError::PerCall { estimate: *e, cap });
                    }
                    total = Self::add(total, *e)?;
                }
                _ => unpriced += 1,
            }
        }
        if unpriced > 0 && self.limits.any_cap() && !self.limits.allow_unpriced {
            over.push(BudgetError::Unpriced);
        }
        let session = Self::add(self.session_committed()?, total)?;
        if let Some(cap) = self.limits.per_session
            && exceeds(session, cap)
        {
            over.push(BudgetError::Session { would_spend: session, cap });
        }
        let project = Self::add(self.project_before, session)?;
        if let Some(cap) = self.limits.per_project
            && exceeds(project, cap)
        {
            over.push(BudgetError::Project { would_spend: project, cap });
        }
        Ok(Quote { total, unpriced, jobs: estimates.len(), over })
    }

    /// Reserves `estimate` for `job` before it starts, or refuses with the cap it would break.
    /// Unpriced jobs reserve nothing (and are refused when a cap is set, unless allowed).
    pub fn reserve(&mut self, job: JobId, estimate: Option<Money>) -> Result<(), BudgetError> {
        if self.reserved.contains_key(&job) {
            return Err(BudgetError::AlreadyReserved(job));
        }
        let amount = estimate.unwrap_or(Money::zero(self.currency));
        self.check(estimate, amount)?;
        self.reserved_total = Self::add(self.reserved_total, amount)?;
        self.reserved.insert(job, estimate);
        Ok(())
    }

    /// Takes on a job that is already running on the provider (resumed after a restart): its
    /// estimate is held without a cap check, since the money is committed either way.
    pub fn adopt(&mut self, job: JobId, estimate: Option<Money>) -> Result<(), BudgetError> {
        if let Some(e) = estimate {
            self.same_currency(e)?;
        }
        if self.reserved.contains_key(&job) {
            return Ok(());
        }
        self.reserved_total = Self::add(self.reserved_total, estimate.unwrap_or(Money::zero(self.currency)))?;
        self.reserved.insert(job, estimate);
        Ok(())
    }

    /// Settles a job that may have been billed: charges what the provider reported, or the
    /// estimate when it reported nothing, and logs it with how it ended.
    pub fn commit(&mut self, job: JobId, actual: Option<Money>, outcome: Settled, provider: &str, model: &str, at_ms: u64) -> Result<Money, BudgetError> {
        let estimate = *self.reserved.get(&job).ok_or(BudgetError::NotReserved(job))?;
        if let Some(a) = actual {
            self.same_currency(a)?;
        }
        let charged = actual.or(estimate).unwrap_or(Money::zero(self.currency));
        self.session_spent = Self::add(self.session_spent, charged)?;
        self.unreserve(job);
        if self.log.len() >= MAX_LOG_ROWS {
            self.log.pop_front();
            self.dropped_rows = self.dropped_rows.saturating_add(1);
        }
        self.log.push_back(UsageRecord { at_ms, job, provider: provider.to_owned(), model: model.to_owned(), outcome, estimate, actual, charged });
        Ok(charged)
    }

    /// Releases a job's reservation without charging: it certainly never reached the provider.
    pub fn release(&mut self, job: JobId) -> bool {
        self.unreserve(job)
    }

    fn unreserve(&mut self, job: JobId) -> bool {
        match self.reserved.remove(&job) {
            Some(estimate) => {
                let amount = estimate.unwrap_or(Money::zero(self.currency));
                self.reserved_total = self.reserved_total.checked_sub(amount).unwrap_or(Money::zero(self.currency));
                true
            }
            None => false,
        }
    }

    pub fn log(&self) -> impl Iterator<Item = &UsageRecord> {
        self.log.iter()
    }

    /// Rows dropped from the in-memory log because it was full.
    pub fn dropped_rows(&self) -> u64 {
        self.dropped_rows
    }

    /// The usage log as CSV (RFC 4180), safe to open in a spreadsheet: cells a spreadsheet would
    /// run as a formula are prefixed with `'`.
    pub fn to_csv(&self) -> String {
        let mut out = String::from("time_ms,job,provider,model,outcome,currency,estimate,actual,charged\r\n");
        let money = |m: Option<Money>| m.map_or_else(String::new, |m| decimal(m.micros));
        for r in &self.log {
            let row = [
                r.at_ms.to_string(),
                r.job.0.to_string(),
                cell(&r.provider),
                cell(&r.model),
                r.outcome.as_str().to_owned(),
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

/// `a > b` for amounts already known to share the ledger's currency.
fn exceeds(a: Money, b: Money) -> bool {
    a.checked_cmp(b).is_ok_and(|o| o == std::cmp::Ordering::Greater)
}

/// Micro-units as a decimal amount (`1234567` → `1.234567`).
fn decimal(micros: u64) -> String {
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

/// A CSV cell: quoted when needed, formula-injection safe.
fn cell(text: &str) -> String {
    let mut t: String = text.chars().filter(|c| !c.is_control()).take(256).collect();
    // Spreadsheets evaluate after leading blanks, and accept fullwidth signs too.
    let first = t.trim_start().chars().next();
    if first.is_some_and(|c| "=+-@＝＋－＠".contains(c)) {
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

    fn commit(l: &mut Ledger, job: u64, actual: Option<Money>) -> Money {
        l.commit(JobId(job), actual, Settled::Done, "fal", "flux", 1).unwrap()
    }

    #[test]
    fn reservations_hold_parallel_jobs_within_the_session_cap() {
        let mut l = Ledger::new(Currency::USD, caps(None, Some(1_000_000), None), usd(0)).unwrap();
        l.reserve(JobId(1), Some(usd(400_000))).unwrap();
        l.reserve(JobId(2), Some(usd(400_000))).unwrap();
        assert_eq!(l.reserve(JobId(3), Some(usd(400_000))), Err(BudgetError::Session { would_spend: usd(1_200_000), cap: usd(1_000_000) }));
        assert_eq!(commit(&mut l, 1, Some(usd(100_000))), usd(100_000));
        l.reserve(JobId(3), Some(usd(400_000))).unwrap();
        assert_eq!(l.session_committed().unwrap(), usd(900_000));
        assert_eq!(l.reserve(JobId(3), Some(usd(1))), Err(BudgetError::AlreadyReserved(JobId(3))), "never overwritten silently");
    }

    #[test]
    fn per_call_and_project_caps() {
        let mut l = Ledger::new(Currency::USD, caps(Some(500_000), None, Some(2_000_000)), usd(1_800_000)).unwrap();
        assert!(matches!(l.reserve(JobId(1), Some(usd(600_000))), Err(BudgetError::PerCall { .. })));
        assert!(matches!(l.reserve(JobId(1), Some(usd(300_000))), Err(BudgetError::Project { .. })), "1.8 already spent in earlier sessions");
        l.reserve(JobId(1), Some(usd(150_000))).unwrap();
        commit(&mut l, 1, None);
        assert_eq!(l.project_spent().unwrap(), usd(1_950_000), "no reported cost: the estimate is charged");
    }

    #[test]
    fn adopted_jobs_are_charged_without_a_cap_check() {
        // After a restart a running job is resumed: it is charged even though it's over the cap now.
        let mut l = Ledger::new(Currency::USD, caps(None, Some(100_000), None), usd(0)).unwrap();
        l.adopt(JobId(7), Some(usd(900_000))).unwrap();
        assert!(l.is_reserved(JobId(7)));
        l.commit(JobId(7), None, Settled::Done, "veo", "veo-3", 5).unwrap();
        assert_eq!(l.session_spent(), usd(900_000));
        assert_eq!(l.log().count(), 1);
    }

    #[test]
    fn unpriced_jobs_need_permission_when_capped() {
        let mut l = Ledger::new(Currency::USD, caps(None, Some(1_000_000), None), usd(0)).unwrap();
        assert_eq!(l.reserve(JobId(1), None), Err(BudgetError::Unpriced));
        let mut allow = caps(None, Some(1_000_000), None);
        allow.allow_unpriced = true;
        l.set_limits(allow).unwrap();
        l.reserve(JobId(1), None).unwrap();
        commit(&mut l, 1, Some(usd(250_000)));
        assert_eq!(l.session_spent(), usd(250_000), "the reported cost still counts");
        let mut free = Ledger::new(Currency::USD, BudgetLimits::default(), usd(0)).unwrap();
        free.reserve(JobId(2), None).unwrap();
    }

    #[test]
    fn quotes_price_runs_and_list_every_broken_cap() {
        let l = Ledger::new(Currency::USD, caps(Some(500_000), Some(1_000_000), Some(1_100_000)), usd(0)).unwrap();
        let q = l.quote(&[Some(usd(300_000)), Some(usd(300_000))]).unwrap();
        assert_eq!((q.total, q.unpriced, q.jobs, q.over), (usd(600_000), 0, 2, vec![]));
        let q = l.quote(&[Some(usd(700_000)), Some(usd(300_000)), Some(usd(300_000))]).unwrap();
        assert_eq!(q.over.len(), 3, "per-call, session and project: {:?}", q.over);
        let q = l.quote(&[None, Some(Money::new(Currency::EUR, 5))]).unwrap();
        assert_eq!((q.unpriced, q.over.as_slice()), (2, &[BudgetError::Unpriced][..]), "other currencies count as unpriced");
    }

    #[test]
    fn currencies_settling_and_overflow() {
        let eur = Money::new(Currency::EUR, 1);
        assert!(Ledger::new(Currency::USD, BudgetLimits { per_session: Some(eur), ..BudgetLimits::default() }, usd(0)).is_err());
        let mut l = Ledger::new(Currency::USD, BudgetLimits::default(), usd(0)).unwrap();
        assert!(matches!(l.reserve(JobId(1), Some(eur)), Err(BudgetError::Currency { .. })));
        assert_eq!(l.commit(JobId(9), None, Settled::Done, "p", "m", 0), Err(BudgetError::NotReserved(JobId(9))));
        l.reserve(JobId(1), Some(usd(5))).unwrap();
        assert!(l.release(JobId(1)));
        assert!(!l.release(JobId(1)));
        assert_eq!(l.session_committed().unwrap(), usd(0), "released, not charged");
        l.reserve(JobId(2), Some(usd(u64::MAX))).unwrap();
        assert_eq!(l.reserve(JobId(3), Some(usd(1))), Err(BudgetError::Overflow), "a hostile estimate can't overflow the ledger");
        assert!(l.release(JobId(2)));
        l.reserve(JobId(3), Some(usd(1))).unwrap();
    }

    #[test]
    fn csv_export_is_spreadsheet_safe_and_records_outcomes() {
        let mut l = Ledger::new(Currency::USD, BudgetLimits::default(), usd(0)).unwrap();
        l.reserve(JobId(1), Some(usd(1_234_567))).unwrap();
        l.commit(JobId(1), Some(usd(1_000_000)), Settled::Done, "=HYPERLINK(\"evil\")", "flux, dev", 1700).unwrap();
        l.reserve(JobId(2), None).unwrap();
        l.commit(JobId(2), None, Settled::Unresolved, "  +cmd", "\u{ff1d}1+1", 1800).unwrap();
        l.reserve(JobId(3), Some(usd(0))).unwrap();
        l.commit(JobId(3), None, Settled::Failed, "ollama", "llama", 1900).unwrap();
        let csv = l.to_csv();
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(lines[0], "time_ms,job,provider,model,outcome,currency,estimate,actual,charged");
        assert_eq!(lines[1], "1700,1,\"'=HYPERLINK(\"\"evil\"\")\",\"flux, dev\",done,USD,1.234567,1.000000,1.000000");
        assert_eq!(lines[2], "1800,2,'  +cmd,'\u{ff1d}1+1,unresolved,USD,,,0.000000");
        assert_eq!(lines[3], "1900,3,ollama,llama,failed,USD,0.000000,,0.000000", "a free job shows 0, not blank");
    }
}

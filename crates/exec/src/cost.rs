//! Cost quotes, budgets and the usage log: what a run will cost before it starts, whether the
//! caps allow it, and what has been spent.
//!
//! Everything here is pure. A node is paid when its kind has a model parameter
//! ([`ParamControl::Model`]); its price is the chosen catalog model's price per job times the jobs
//! the step plans. Nodes without a model parameter (sources, local actions) are free. Totals are
//! kept per currency (never converted), and a step whose price isn't known blocks any capped run,
//! so a budget can't be passed by a quote that was silently incomplete.

use crate::planner::RunPlan;
use nodecraft_catalog::{Catalog, ModelDescriptor, Pricing};
use nodecraft_graph::{Canvas, Node};
use nodecraft_nodes::Registry;
use nodecraft_types::{Currency, Error, ErrorKind, Money, NodeId, ParamControl, ParamValues, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Why a step has no price yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unpriced {
    /// The node's model parameter is empty.
    NoModel,
    /// The model isn't in the catalog.
    UnknownModel(String),
    /// The model is priced by usage (tokens, characters) measured during the run.
    ByUsage(String),
    /// The node, its kind, the model or its parameters couldn't be priced.
    Invalid(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepQuote {
    pub node: NodeId,
    pub model: Option<String>,
    /// The price of one job, `None` for free steps.
    pub per_job: Result<Option<Money>, Unpriced>,
    /// Jobs the step runs (its planned jobs, or one when they're planned later).
    pub jobs: u64,
}

impl StepQuote {
    /// The step's price: per job times jobs.
    pub fn total(&self) -> Result<Option<Money>, Unpriced> {
        match &self.per_job {
            Ok(Some(amount)) => amount.checked_mul(self.jobs).map(Some).map_err(|error| Unpriced::Invalid(error.message)),
            other => other.clone(),
        }
    }
}

/// A run's price: per step, and totals per currency of the priced steps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Quote {
    pub steps: Vec<StepQuote>,
    pub totals: BTreeMap<Currency, Money>,
}

impl Quote {
    /// Steps without a known price; while any remain the total is partial.
    pub fn unpriced(&self) -> impl Iterator<Item = (NodeId, Unpriced)> + '_ {
        self.steps.iter().filter_map(|step| step.total().err().map(|why| (step.node, why)))
    }
    pub fn is_complete(&self) -> bool {
        self.unpriced().next().is_none()
    }
}

/// The values a model is quoted and run with: the node's parameters that belong to the model's
/// schema. The provider request (#74) must be built from the same values, so the quote matches
/// the charge.
pub fn model_params(node: &Node, model: &ModelDescriptor) -> ParamValues {
    node.params.iter().filter(|(name, _)| model.params.params.iter().any(|spec| spec.name == **name)).map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// Price every step of `plan`.
pub fn quote(plan: &RunPlan, canvas: &Canvas, kinds: &Registry, catalog: &Catalog) -> Result<Quote, Error> {
    let mut quote = Quote::default();
    for step in &plan.steps {
        let node = canvas.nodes().get(&step.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "a planned node is not on the canvas"))?;
        let jobs = step.jobs.as_ref().map_or(1, |jobs| u64::try_from(jobs.len()).unwrap_or(u64::MAX).max(1));
        let (model, per_job) = price_step(node, kinds, catalog);
        let mut step = StepQuote { node: step.node, model, per_job, jobs };
        if let Ok(Some(amount)) = step.total() {
            let total = quote.totals.entry(amount.currency).or_insert_with(|| Money::zero(amount.currency));
            match total.checked_add(amount) {
                Ok(sum) => *total = sum,
                // A total too large to add is reported on the step; the rest still shows.
                Err(error) => step.per_job = Err(Unpriced::Invalid(error.message)),
            }
        }
        quote.steps.push(step);
    }
    Ok(quote)
}

fn price_step(node: &Node, kinds: &Registry, catalog: &Catalog) -> (Option<String>, Result<Option<Money>, Unpriced>) {
    let kind = match kinds.get(&node.kind) {
        Ok(kind) => kind,
        Err(_) => return (None, Err(Unpriced::Invalid(format!("the {} node kind isn't available", node.kind)))),
    };
    let descriptor = match kind.resolve_instance(node) {
        Ok(descriptor) => descriptor,
        Err(error) => return (None, Err(Unpriced::Invalid(error.message))),
    };
    // A kind without a model parameter runs locally and is free.
    let Some(spec) = descriptor.params.params.iter().find(|spec| spec.control == ParamControl::Model) else {
        return (None, Ok(None));
    };
    let id = match node.params.get(&spec.name) {
        Some(Value::Text(id)) if !id.is_empty() => id.clone(),
        _ => return (None, Err(Unpriced::NoModel)),
    };
    let Some(model) = catalog.get(&id) else {
        return (Some(id.clone()), Err(Unpriced::UnknownModel(id)));
    };
    // Free models cost nothing in any currency, so they never trip a currency check.
    if model.pricing == Pricing::Free {
        return (Some(id), Ok(None));
    }
    let price = match model.quote(&model_params(node, model)) {
        Ok(Some(money)) => Ok(Some(money)),
        Ok(None) => Err(Unpriced::ByUsage(id.clone())),
        Err(error) => Err(Unpriced::Invalid(error.message)),
    };
    (Some(id), price)
}

/// Spending caps. A cap applies to amounts in its own currency; anything else can't be compared
/// and is blocked, and spend recorded in other currencies doesn't count towards it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Budgets {
    /// Most one job may cost.
    pub per_call: Option<Money>,
    /// Most this session may spend in total.
    pub session: Option<Money>,
    /// Most this project may spend in total.
    pub project: Option<Money>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    PerCall,
    Session,
    Project,
}

/// Why a run can't start under the caps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// The run (or one job, for `PerCall`) would go past the cap.
    Over { scope: Scope, node: Option<NodeId>, limit: Money, would_be: Money },
    /// A capped run has a step whose price isn't known.
    Unknown { scope: Scope, node: NodeId },
    /// An amount is in a currency the cap isn't.
    Currency { scope: Scope, limit: Money, amount: Money },
}

impl Block {
    pub fn message(&self) -> String {
        let name = |scope: &Scope| match scope {
            Scope::PerCall => "per-call",
            Scope::Session => "session",
            Scope::Project => "project",
        };
        match self {
            Self::Over { scope, limit, would_be, .. } => {
                format!("This would exceed the {} budget: {} of {}.", name(scope), format_money(*would_be), format_money(*limit))
            }
            Self::Unknown { scope, .. } => format!("A step's price isn't known yet, so the {} budget can't be checked.", name(scope)),
            Self::Currency { scope, limit, amount } => {
                format!("The {} budget is in {}, but a price is in {}.", name(scope), String::from(limit.currency), String::from(amount.currency))
            }
        }
    }
}

/// A money amount as text, e.g. `0.25 USD` (trailing zeros trimmed, at most six decimals).
pub fn format_money(money: Money) -> String {
    let whole = money.micros / 1_000_000;
    let fraction = money.micros % 1_000_000;
    let currency = String::from(money.currency);
    if fraction == 0 {
        format!("{whole} {currency}")
    } else {
        let digits = format!("{fraction:06}");
        format!("{whole}.{} {currency}", digits.trim_end_matches('0'))
    }
}

/// Check a quote against the caps given what has been spent. Empty means the run may start.
/// Amounts equal to a cap are allowed.
pub fn check(quote: &Quote, budgets: &Budgets, spent: &Spent) -> Result<Vec<Block>, Error> {
    let mut blocks = Vec::new();
    if let Some(limit) = budgets.per_call {
        for step in &quote.steps {
            match &step.per_job {
                Err(_) => blocks.push(Block::Unknown { scope: Scope::PerCall, node: step.node }),
                Ok(None) => {}
                Ok(Some(amount)) if amount.currency != limit.currency => blocks.push(Block::Currency { scope: Scope::PerCall, limit, amount: *amount }),
                Ok(Some(amount)) if amount.micros > limit.micros => {
                    blocks.push(Block::Over { scope: Scope::PerCall, node: Some(step.node), limit, would_be: *amount });
                }
                Ok(Some(_)) => {}
            }
        }
    }
    for (scope, limit, already) in [(Scope::Session, budgets.session, &spent.session), (Scope::Project, budgets.project, &spent.project)] {
        let Some(limit) = limit else { continue };
        if let Some((node, _)) = quote.unpriced().next() {
            blocks.push(Block::Unknown { scope, node });
        }
        for amount in quote.totals.values() {
            if amount.currency != limit.currency && amount.micros > 0 {
                blocks.push(Block::Currency { scope, limit, amount: *amount });
            }
        }
        let run = quote.totals.get(&limit.currency).copied().unwrap_or(Money::zero(limit.currency));
        // A run that adds nothing in the cap's currency is never "over", even past the cap.
        if run.micros == 0 {
            continue;
        }
        let before = already.get(&limit.currency).copied().unwrap_or(Money::zero(limit.currency));
        let would_be = before.checked_add(run)?;
        if would_be.micros > limit.micros {
            blocks.push(Block::Over { scope, node: None, limit, would_be });
        }
    }
    Ok(blocks)
}

/// What has been spent, per currency, in a session and in the project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spent {
    pub session: BTreeMap<Currency, Money>,
    pub project: BTreeMap<Currency, Money>,
}

/// One charged job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageEntry {
    /// Milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// The session it happened in (an id the host assigns per launch).
    pub session: String,
    pub node: NodeId,
    pub provider: String,
    pub model: String,
    pub cost: Money,
    /// The idempotency key the provider was charged under, so a retry shows as one charge.
    pub idempotency_key: String,
}

/// Most entries kept; older ones leave the list (export them first) but stay in the totals.
pub const MAX_USAGE_ENTRIES: usize = 100_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageLog {
    entries: VecDeque<UsageEntry>,
    /// Idempotency keys of `entries`, for constant-time duplicate checks.
    keys: BTreeSet<String>,
    /// Spend of entries that left the list: per session and in total, so caps never loosen.
    evicted_sessions: BTreeMap<String, BTreeMap<Currency, Money>>,
    evicted_total: BTreeMap<Currency, Money>,
}

fn add_to(map: &mut BTreeMap<Currency, Money>, amount: Money) -> Result<(), Error> {
    let total = map.entry(amount.currency).or_insert_with(|| Money::zero(amount.currency));
    *total = total.checked_add(amount)?;
    Ok(())
}

impl UsageLog {
    /// Record a charge. Returns `false` when its idempotency key was already recorded (a retry is
    /// the same charge). An empty key is refused, since it couldn't tell retries from new charges.
    /// (A key whose entry already left the list counts again; export before that.)
    pub fn record(&mut self, entry: UsageEntry) -> Result<bool, Error> {
        if entry.idempotency_key.is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, "a usage entry needs an idempotency key"));
        }
        if self.keys.contains(&entry.idempotency_key) {
            return Ok(false);
        }
        if self.entries.len() >= MAX_USAGE_ENTRIES
            && let Some(oldest) = self.entries.pop_front()
        {
            self.keys.remove(&oldest.idempotency_key);
            add_to(&mut self.evicted_total, oldest.cost)?;
            add_to(self.evicted_sessions.entry(oldest.session).or_default(), oldest.cost)?;
        }
        self.keys.insert(entry.idempotency_key.clone());
        self.entries.push_back(entry);
        Ok(true)
    }

    pub fn entries(&self) -> impl Iterator<Item = &UsageEntry> {
        self.entries.iter()
    }

    /// Totals per currency for `session` and for the whole project, including entries that have
    /// left the list.
    pub fn spent(&self, session: &str) -> Result<Spent, Error> {
        let mut spent = Spent { session: self.evicted_sessions.get(session).cloned().unwrap_or_default(), project: self.evicted_total.clone() };
        for entry in &self.entries {
            add_to(&mut spent.project, entry.cost)?;
            if entry.session == session {
                add_to(&mut spent.session, entry.cost)?;
            }
        }
        Ok(spent)
    }

    /// The listed entries as CSV (RFC 4180), safe to open in a spreadsheet: text cells that could
    /// be read as formulas are prefixed with an apostrophe.
    pub fn to_csv(&self) -> String {
        let mut out = String::from("time_ms,session,node,provider,model,amount,currency,idempotency_key\r\n");
        for e in &self.entries {
            let amount = format_money(e.cost);
            let amount = amount.split_once(' ').map_or(amount.as_str(), |(n, _)| n).to_owned();
            let row = [
                e.at_ms.to_string(),
                cell(&e.session),
                e.node.to_string(),
                cell(&e.provider),
                cell(&e.model),
                amount,
                String::from(e.cost.currency),
                cell(&e.idempotency_key),
            ];
            out.push_str(&row.join(","));
            out.push_str("\r\n");
        }
        out
    }
}

/// One CSV text cell. Text whose first non-blank character could start a formula (including the
/// full-width forms some locales accept) gets an apostrophe; quotes, commas and line breaks are
/// quoted.
fn cell(text: &str) -> String {
    let first = text.trim_start().chars().next();
    let formula =
        first.is_some_and(|c| matches!(c, '=' | '+' | '-' | '@' | '\u{FF1D}' | '\u{FF0B}' | '\u{FF0D}' | '\u{FF20}')) || text.starts_with(['\t', '\r']);
    let text = if formula { format!("'{text}") } else { text.to_owned() };
    if text.contains([',', '"', '\n', '\r']) { format!("\"{}\"", text.replace('"', "\"\"")) } else { text }
}

#[cfg(test)]
mod tests;

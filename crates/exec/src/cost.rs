//! Cost quotes, budgets and the usage log: what a run will cost before it starts, whether the
//! caps allow it, and what has been spent.
//!
//! Everything here is pure. A node is paid when its kind has a model parameter
//! ([`ParamControl::Model`]); its price is the chosen catalog model's price per job times the jobs
//! the step plans. Nodes without a model parameter (sources, local actions) are free, except
//! generation kinds, which can't be priced without one. Totals are
//! kept per currency (never converted), and a step whose price isn't known blocks any capped run,
//! so a budget can't be passed by a quote that was silently incomplete.

use crate::planner::RunPlan;
use nodecraft_catalog::{Catalog, ModelDescriptor, Pricing};
use nodecraft_graph::{Canvas, Node};
use nodecraft_nodes::{Category, Registry};
use nodecraft_types::{Currency, Error, ErrorKind, Money, NodeId, ParamControl, ParamValues, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

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
    /// The catalog models its model parameters name, in schema order.
    pub models: Vec<String>,
    /// The price of one job (all its models together), `None` for free steps.
    pub per_job: Result<Option<Money>, Unpriced>,
    /// Jobs the step runs: its planned jobs, or one when they're planned later.
    pub jobs: u64,
    /// Whether `jobs` is known. A paid step whose jobs are planned later (it waits on another
    /// step of the run) is an estimate: the driver checks budgets again before each job (#207).
    pub jobs_known: bool,
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
    /// Paid steps quoted before their jobs are known (one job assumed).
    pub fn estimated(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.steps.iter().filter(|step| !step.jobs_known && matches!(step.per_job, Ok(Some(_)))).map(|step| step.node)
    }
    /// Every step priced and every paid step's job count known: the total is exact.
    pub fn is_complete(&self) -> bool {
        self.unpriced().next().is_none() && self.estimated().next().is_none()
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
        let jobs_known = step.jobs.is_some();
        let (models, per_job) = price_step(node, kinds, catalog);
        let mut step = StepQuote { node: step.node, models, per_job, jobs, jobs_known };
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

/// One job's price for `node`: the sum of the models its model parameters name. Free when the
/// kind has no model parameter (local work) or every model is free.
fn price_step(node: &Node, kinds: &Registry, catalog: &Catalog) -> (Vec<String>, Result<Option<Money>, Unpriced>) {
    let kind = match kinds.get(&node.kind) {
        Ok(kind) => kind,
        Err(_) => return (Vec::new(), Err(Unpriced::Invalid(format!("the {} node kind isn't available", node.kind)))),
    };
    let descriptor = match kind.resolve_instance(node) {
        Ok(descriptor) => descriptor,
        Err(error) => return (Vec::new(), Err(Unpriced::Invalid(error.message))),
    };
    let mut models = Vec::new();
    let mut total: Option<Money> = None;
    // A generation kind that names no model calls something we can't price: never "free".
    if descriptor.category == Category::Generation && !descriptor.params.params.iter().any(|spec| spec.control == ParamControl::Model) {
        return (models, Err(Unpriced::Invalid(format!("the {} node kind has no model to price", node.kind))));
    }
    for spec in descriptor.params.params.iter().filter(|spec| spec.control == ParamControl::Model) {
        let id = match node.params.get(&spec.name) {
            Some(Value::Text(id)) if !id.is_empty() => id.clone(),
            _ => return (models, Err(Unpriced::NoModel)),
        };
        models.push(id.clone());
        let Some(model) = catalog.get(&id) else {
            return (models, Err(Unpriced::UnknownModel(id)));
        };
        // Free models cost nothing in any currency, so they never trip a currency check.
        if model.pricing == Pricing::Free {
            continue;
        }
        let price = match model.quote(&model_params(node, model)) {
            Ok(Some(money)) => money,
            Ok(None) => return (models, Err(Unpriced::ByUsage(id))),
            Err(error) => return (models, Err(Unpriced::Invalid(error.message))),
        };
        total = match total {
            None => Some(price),
            Some(sum) => match sum.checked_add(price) {
                Ok(sum) => Some(sum),
                Err(error) => return (models, Err(Unpriced::Invalid(error.message))),
            },
        };
    }
    (models, Ok(total))
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

/// How settled a charge is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Charge {
    /// What the provider reported, or the estimate for a job that finished without a report.
    #[default]
    Actual,
    /// The quoted price, held while the job runs; settled later.
    Estimate,
    /// The job may have been billed but its outcome is unknown (a lost submit, a run given up
    /// on): charged at the estimate so caps hold.
    Unresolved,
}

impl Charge {
    fn as_str(self) -> &'static str {
        match self {
            Self::Actual => "actual",
            Self::Estimate => "estimate",
            Self::Unresolved => "unresolved",
        }
    }
}

/// One charged job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    #[serde(default)]
    pub status: Charge,
}

/// Most entries kept; older ones leave the list (export them first) but stay in the totals.
pub const MAX_USAGE_ENTRIES: usize = 100_000;
/// Largest single charge accepted, in micro-units (a million units of any currency). Anything
/// larger is a provider or file error, and accepting it would push the totals towards overflow.
pub const MAX_CHARGE_MICROS: u64 = 1_000_000 * 1_000_000;
/// Largest total a saved log may claim: room for a million maximal charges, far from overflow.
pub const MAX_TOTAL_MICROS: u64 = MAX_CHARGE_MICROS * 1_000_000;
/// Longest session id, provider, model or key text accepted.
const MAX_TEXT: usize = 512;

type Totals = BTreeMap<Currency, Money>;

/// The usage log and the totals it implies. Totals cover every charge ever recorded, including
/// entries that have left the list, so caps never loosen; they are saved with the log.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(try_from = "UsageLogFile", into = "UsageLogFile")]
pub struct UsageLog {
    entries: VecDeque<UsageEntry>,
    /// `(provider, idempotency key)` → sequence number of its entry; keys are only unique per
    /// provider. An entry's index is its sequence number minus `first_seq`.
    keys: BTreeMap<(String, String), u64>,
    first_seq: u64,
    project: Totals,
    sessions: BTreeMap<String, Totals>,
}

/// Logs are equal when they list the same charges with the same totals (the key index is
/// derived from the entries).
impl PartialEq for UsageLog {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries && self.project == other.project && self.sessions == other.sessions
    }
}
impl Eq for UsageLog {}

/// How a usage log is saved: the listed entries and the totals.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageLogFile {
    pub entries: Vec<UsageEntry>,
    pub project: Totals,
    #[serde(default)]
    pub sessions: BTreeMap<String, Totals>,
}

fn add_to(map: &mut Totals, amount: Money) -> Result<(), Error> {
    let total = map.entry(amount.currency).or_insert_with(|| Money::zero(amount.currency));
    *total = total.checked_add(amount)?;
    Ok(())
}

fn sub_from(map: &mut Totals, amount: Money) -> Result<(), Error> {
    let total = map.get_mut(&amount.currency).ok_or_else(|| Error::new(ErrorKind::Internal, "usage totals are missing a currency"))?;
    *total = total.checked_sub(amount)?;
    Ok(())
}

fn check_entry(entry: &UsageEntry) -> Result<(), Error> {
    let bad = |m: &str| Err(Error::new(ErrorKind::InvalidInput, m.to_owned()));
    if entry.idempotency_key.is_empty() {
        return bad("a usage entry needs an idempotency key");
    }
    if [&entry.session, &entry.provider, &entry.model, &entry.idempotency_key].iter().any(|t| t.len() > MAX_TEXT) {
        return bad("a usage entry's text is too long");
    }
    if entry.cost.micros > MAX_CHARGE_MICROS {
        return bad("a charge is larger than any job could cost");
    }
    Ok(())
}

impl UsageLog {
    /// Record a charge. Returns `false` when its idempotency key was already recorded (a retry is
    /// the same charge; use [`UsageLog::settle`] to change it). An empty key is refused, since it
    /// couldn't tell retries from new charges, and so is an implausibly large cost. Nothing changes
    /// on an error. (A key whose entry already left the list counts again; export before that.)
    pub fn record(&mut self, entry: UsageEntry) -> Result<bool, Error> {
        check_entry(&entry)?;
        let key = (entry.provider.clone(), entry.idempotency_key.clone());
        if self.keys.contains_key(&key) {
            return Ok(false);
        }
        // New totals first, so an overflow leaves the log as it was.
        let mut project = self.project.clone();
        add_to(&mut project, entry.cost)?;
        let mut session = self.sessions.get(&entry.session).cloned().unwrap_or_default();
        add_to(&mut session, entry.cost)?;
        // The same ceiling a saved log is held to, so every log that records can be reloaded.
        if project.values().any(|m| m.micros > MAX_TOTAL_MICROS) {
            return Err(Error::new(ErrorKind::Overflow, "the project's recorded spend is at its limit"));
        }
        let seq = self.first_seq.checked_add(self.entries.len() as u64).ok_or_else(|| Error::new(ErrorKind::Overflow, "usage log sequence overflow"))?;
        if self.entries.len() >= MAX_USAGE_ENTRIES
            && let Some(oldest) = self.entries.pop_front()
        {
            // The totals already include it.
            self.keys.remove(&(oldest.provider, oldest.idempotency_key));
            self.first_seq = self.first_seq.saturating_add(1);
        }
        self.project = project;
        self.sessions.insert(entry.session.clone(), session);
        self.keys.insert(key, seq);
        self.entries.push_back(entry);
        Ok(true)
    }

    /// The listed, unsettled entry for a key and its index.
    fn open_entry(&self, provider: &str, idempotency_key: &str) -> Result<(usize, &UsageEntry), Error> {
        let seq = *self
            .keys
            .get(&(provider.to_owned(), idempotency_key.to_owned()))
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "no listed charge has that key (it may have been exported and dropped)"))?;
        let index = usize::try_from(seq.saturating_sub(self.first_seq)).map_err(|_| Error::new(ErrorKind::Internal, "usage log index out of range"))?;
        let entry = self.entries.get(index).ok_or_else(|| Error::new(ErrorKind::Internal, "usage log index out of range"))?;
        if entry.status == Charge::Actual {
            return Err(Error::new(ErrorKind::Conflict, "that charge is already settled"));
        }
        Ok((index, entry))
    }

    /// Changes a recorded estimate's cost and status: it becomes the actual cost, or stays
    /// charged as unresolved. A charge already settled as actual is final, and the currency can't
    /// change. Totals move by the difference; nothing changes on an error.
    ///
    /// `NotFound` means the entry already left the list: its estimate stays in the totals (the
    /// conservative outcome). Don't `record` it again, which would count it twice.
    pub fn settle(&mut self, provider: &str, idempotency_key: &str, cost: Money, status: Charge) -> Result<(), Error> {
        if cost.micros > MAX_CHARGE_MICROS {
            return Err(Error::new(ErrorKind::InvalidInput, "a charge is larger than any job could cost"));
        }
        if status == Charge::Estimate {
            return Err(Error::new(ErrorKind::InvalidInput, "settle to an actual or unresolved charge"));
        }
        let (index, entry) = self.open_entry(provider, idempotency_key)?;
        if cost.currency != entry.cost.currency {
            return Err(Error::new(ErrorKind::CurrencyMismatch, "a charge can't change currency when it settles"));
        }
        let mut project = self.project.clone();
        sub_from(&mut project, entry.cost)?;
        add_to(&mut project, cost)?;
        let mut session = self.sessions.get(&entry.session).cloned().unwrap_or_default();
        sub_from(&mut session, entry.cost)?;
        add_to(&mut session, cost)?;
        let name = entry.session.clone();
        self.project = project;
        self.sessions.insert(name, session);
        if let Some(entry) = self.entries.get_mut(index) {
            entry.cost = cost;
            entry.status = status;
        }
        Ok(())
    }

    /// Removes an unsettled charge for a job the provider certainly never billed (a rejected key,
    /// invalid input), taking it out of the totals and freeing its key for a later attempt.
    /// Only an estimate can be released: an unresolved charge may have been billed.
    pub fn release(&mut self, provider: &str, idempotency_key: &str) -> Result<(), Error> {
        let (index, entry) = self.open_entry(provider, idempotency_key)?;
        if entry.status != Charge::Estimate {
            return Err(Error::new(ErrorKind::Conflict, "only an estimate can be released; this charge may have been billed"));
        }
        let mut project = self.project.clone();
        sub_from(&mut project, entry.cost)?;
        let mut session = self.sessions.get(&entry.session).cloned().unwrap_or_default();
        sub_from(&mut session, entry.cost)?;
        let name = entry.session.clone();
        self.project = project;
        self.sessions.insert(name, session);
        self.entries.remove(index);
        // Later entries moved up one: renumber the index (rare, so a rebuild is fine).
        self.first_seq = 0;
        self.keys = self.entries.iter().enumerate().map(|(i, e)| ((e.provider.clone(), e.idempotency_key.clone()), i as u64)).collect();
        Ok(())
    }

    /// The listed charge for `(provider, idempotency_key)`, if any.
    pub fn charge(&self, provider: &str, idempotency_key: &str) -> Option<&UsageEntry> {
        let seq = *self.keys.get(&(provider.to_owned(), idempotency_key.to_owned()))?;
        self.entries.get(usize::try_from(seq.checked_sub(self.first_seq)?).ok()?)
    }

    pub fn entries(&self) -> impl Iterator<Item = &UsageEntry> {
        self.entries.iter()
    }

    /// Totals per currency for `session` and for the whole project, including entries that have
    /// left the list.
    pub fn spent(&self, session: &str) -> Spent {
        Spent { session: self.sessions.get(session).cloned().unwrap_or_default(), project: self.project.clone() }
    }

    /// The listed entries as CSV (RFC 4180), safe to open in a spreadsheet: text cells that could
    /// be read as formulas are prefixed with an apostrophe.
    pub fn to_csv(&self) -> String {
        let mut out = String::from("time_ms,session,node,provider,model,amount,currency,status,idempotency_key\r\n");
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
                e.status.as_str().to_owned(),
                cell(&e.idempotency_key),
            ];
            out.push_str(&row.join(","));
            out.push_str("\r\n");
        }
        out
    }
}

impl From<UsageLog> for UsageLogFile {
    fn from(log: UsageLog) -> Self {
        Self { entries: log.entries.into(), project: log.project, sessions: log.sessions }
    }
}

/// A saved log is checked like new input: bounded, keyed, and with totals that cover its
/// entries (a file can't claim less was spent than it lists).
impl TryFrom<UsageLogFile> for UsageLog {
    type Error = Error;

    fn try_from(file: UsageLogFile) -> Result<Self, Error> {
        let bad = |m: &str| Error::new(ErrorKind::InvalidInput, format!("usage log: {m}"));
        if file.entries.len() > MAX_USAGE_ENTRIES {
            return Err(bad("too many entries"));
        }
        let mut listed = Totals::new();
        let mut listed_sessions: BTreeMap<&str, Totals> = BTreeMap::new();
        let mut keys = BTreeMap::new();
        for (seq, entry) in file.entries.iter().enumerate() {
            check_entry(entry)?;
            if keys.insert((entry.provider.clone(), entry.idempotency_key.clone()), seq as u64).is_some() {
                return Err(bad("an idempotency key appears twice"));
            }
            add_to(&mut listed, entry.cost)?;
            add_to(listed_sessions.entry(entry.session.as_str()).or_default(), entry.cost)?;
        }
        let covers = |totals: Option<&Totals>, sums: &Totals| {
            sums.iter().all(|(currency, sum)| sum.micros == 0 || totals.and_then(|t| t.get(currency)).is_some_and(|t| t.micros >= sum.micros))
        };
        let consistent = |totals: &Totals| totals.iter().all(|(currency, money)| money.currency == *currency);
        if !covers(Some(&file.project), &listed) || !consistent(&file.project) || !file.sessions.values().all(consistent) {
            return Err(bad("the totals don't cover the listed charges"));
        }
        if !listed_sessions.iter().all(|(name, sums)| covers(file.sessions.get(*name), sums)) {
            return Err(bad("a session's totals don't cover its charges"));
        }
        if file.sessions.keys().any(|name| name.len() > MAX_TEXT) {
            return Err(bad("a session id is too long"));
        }
        // Every charge counts in its session and the project, so the project is exactly the sum
        // of the sessions; totals far below overflow keep later charges recordable.
        let mut summed = Totals::new();
        for totals in file.sessions.values() {
            for money in totals.values() {
                add_to(&mut summed, *money)?;
            }
        }
        summed.retain(|_, m| m.micros > 0);
        let nonzero: Totals = file.project.iter().filter(|(_, m)| m.micros > 0).map(|(c, m)| (*c, *m)).collect();
        if summed != nonzero {
            return Err(bad("the project total isn't the sum of its sessions"));
        }
        if file.project.values().any(|m| m.micros > MAX_TOTAL_MICROS) {
            return Err(bad("a total is larger than any project could spend"));
        }
        Ok(Self { entries: file.entries.into(), keys, first_seq: 0, project: file.project, sessions: file.sessions })
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

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
    /// The catalog models its model parameters name, in schema order.
    pub models: Vec<String>,
    /// The price of one job (all its models together), `None` for free steps.
    pub per_job: Result<Option<Money>, Unpriced>,
    /// Jobs the step runs: its planned jobs (possibly none), or one when they're planned later.
    pub jobs: u64,
    /// Whether `jobs` is known. A paid step whose jobs are planned later (it waits on another
    /// step of the run) is an estimate: the driver checks budgets again before each job (#207).
    pub jobs_known: bool,
}

impl StepQuote {
    /// The step is known to run no jobs.
    pub fn runs_nothing(&self) -> bool {
        self.jobs_known && self.jobs == 0
    }

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
    /// Steps without a known price; while any remain the total is partial. A step known to run
    /// no jobs costs nothing, priced or not.
    pub fn unpriced(&self) -> impl Iterator<Item = (NodeId, Unpriced)> + '_ {
        self.steps.iter().filter(|step| !step.runs_nothing()).filter_map(|step| step.total().err().map(|why| (step.node, why)))
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
        // A known empty job list costs nothing; one job is assumed only while the count is unknown.
        let jobs = step.jobs.as_ref().map_or(1, |jobs| u64::try_from(jobs.len()).unwrap_or(u64::MAX));
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

/// The result of checking a quote against the caps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Admission {
    /// Why the run can't start; empty when it may.
    pub blocks: Vec<Block>,
    /// Paid steps quoted at one job before their job count is known, while a session or project
    /// cap is set: the run may start, but only **provisionally**. Their real jobs must be held
    /// to the caps as they start (`budget::Ledger`), since the total checked here is a guess.
    pub provisional: Vec<NodeId>,
}

impl Admission {
    /// The run may start (possibly provisionally).
    pub fn allowed(&self) -> bool {
        self.blocks.is_empty()
    }
    /// The run may start and nothing was admitted provisionally (every capped step's job count
    /// was known, or no session or project cap applies). Unpriced steps under no cap still pass.
    pub fn is_final(&self) -> bool {
        self.blocks.is_empty() && self.provisional.is_empty()
    }
}

/// Check a quote against the caps given what has been spent. Amounts equal to a cap are allowed.
/// A quote with deferred paid steps is admitted only provisionally ([`Admission::provisional`]).
pub fn check(quote: &Quote, budgets: &Budgets, spent: &Spent) -> Result<Admission, Error> {
    let mut blocks = Vec::new();
    if let Some(limit) = budgets.per_call {
        for step in quote.steps.iter().filter(|step| !step.runs_nothing()) {
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
    let provisional = if budgets.session.is_some() || budgets.project.is_some() { quote.estimated().collect() } else { Vec::new() };
    Ok(Admission { blocks, provisional })
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
pub const MAX_TEXT: usize = 512;
/// Most text (session ids, providers, models, keys) the listed entries may hold together; past
/// it the oldest entries leave the list, as past [`MAX_USAGE_ENTRIES`].
pub const MAX_LISTED_TEXT: usize = 32 * 1024 * 1024;
/// Identities of charges that left the list, kept so a late retry of one isn't charged again.
/// Past this many, the oldest identity is forgotten: a retry of a charge more than
/// `MAX_USAGE_ENTRIES + MAX_RETIRED_KEYS` charges old counts again (the documented boundary).
pub const MAX_RETIRED_KEYS: usize = 250_000;
/// Open charges (estimates, unresolved) kept after they leave the list, so they can still be
/// settled or released. Past this many, an open charge leaving the list stays charged as it is
/// (the conservative outcome) and only its identity is kept. Their text is outside
/// [`MAX_LISTED_TEXT`] and bounded by this count times four fields of [`MAX_TEXT`] (20 MiB).
/// Unresolved charges stay pending until settled as actual; hosts settle or export them, since
/// a full pending list only stops later charges from being settleable, never from counting.
pub const MAX_PENDING: usize = 10_000;
/// Most sessions with their own totals. Past it, the least recently charged session folds into
/// one "earlier sessions" total, saved under the empty name, which still counts for the project.
/// Never folded: the session charging, and any session with an open charge (an estimate or
/// unresolved), whose later settle or release must come out of its own total. A session id that
/// charges again after being folded starts a fresh session total: sessions are per launch, and one
/// 1,024 sessions dormant is over. Sessions kept for open charges can exceed the limit, by at most
/// the number of open charges.
pub const MAX_SESSIONS: usize = 1024;
/// The session name the folded totals of earlier sessions are kept under.
pub const EARLIER_SESSIONS: &str = "";

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
    /// Identities of charges that left the list, oldest first (see [`MAX_RETIRED_KEYS`]).
    retired: VecDeque<Retired>,
    retired_set: BTreeSet<Retired>,
    /// Open charges that left the list, by `(provider, key)` (see [`MAX_PENDING`]).
    pending: BTreeMap<(String, String), UsageEntry>,
    /// Text bytes of the listed entries (see [`MAX_LISTED_TEXT`]).
    text_bytes: usize,
    /// When each session last charged (a counter, not saved: rebuilt from the listed entries), so
    /// folding picks the least recently used.
    session_clock: BTreeMap<String, u64>,
    clock: u64,
}

/// A retired charge's identity: 16 bytes of a hash of its provider and key.
type Retired = [u8; 16];

fn retired_id(provider: &str, idempotency_key: &str) -> Retired {
    let mut hasher = blake3::Hasher::new();
    for text in [provider, idempotency_key] {
        hasher.update(&u64::try_from(text.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(text.as_bytes());
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(hasher.finalize().as_bytes().get(..16).unwrap_or(&[0; 16]));
    id
}

/// Takes `amount` out of session `name`'s totals; what its own total no longer holds (it was
/// folded into the earlier sessions') comes out of [`EARLIER_SESSIONS`].
fn take_from_session(sessions: &mut BTreeMap<String, Totals>, name: &str, amount: Money) -> Result<(), Error> {
    let own = sessions.get(name).and_then(|t| t.get(&amount.currency)).map_or(0, |m| m.micros);
    let from_own = own.min(amount.micros);
    if from_own > 0
        && let Some(totals) = sessions.get_mut(name)
    {
        sub_from(totals, Money::new(amount.currency, from_own))?;
    }
    let rest = amount.micros.saturating_sub(from_own);
    if rest > 0 {
        let earlier = sessions.get_mut(EARLIER_SESSIONS).ok_or_else(|| Error::new(ErrorKind::Internal, "usage totals are missing a session"))?;
        sub_from(earlier, Money::new(amount.currency, rest))?;
    }
    Ok(())
}

/// Adds `amount` to session `name`'s totals, or to [`EARLIER_SESSIONS`] when it was folded.
fn give_to_session(sessions: &mut BTreeMap<String, Totals>, name: &str, amount: Money) -> Result<(), Error> {
    let target = if sessions.contains_key(name) { name } else { EARLIER_SESSIONS };
    add_to(sessions.entry(target.to_owned()).or_default(), amount)
}

/// The two sessions a change to an entry of `name` can touch, copied so the change is atomic.
fn touched_sessions(sessions: &BTreeMap<String, Totals>, name: &str) -> BTreeMap<String, Totals> {
    [name, EARLIER_SESSIONS].into_iter().filter_map(|n| sessions.get(n).map(|t| (n.to_owned(), t.clone()))).collect()
}

fn entry_text(entry: &UsageEntry) -> usize {
    [&entry.session, &entry.provider, &entry.model, &entry.idempotency_key].iter().fold(0usize, |sum, t| sum.saturating_add(t.len()))
}

fn hex(id: &Retired) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Retired> {
    if text.len() != 32 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let mut id = [0u8; 16];
    for (i, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(id)
}

/// Logs are equal when they list the same charges with the same totals (the key index is
/// derived from the entries).
impl PartialEq for UsageLog {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
            && self.project == other.project
            && self.sessions == other.sessions
            && self.retired == other.retired
            && self.pending == other.pending
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
    /// Identities (hex) of charges that left the list, oldest first.
    #[serde(default)]
    pub retired: Vec<String>,
    /// Open charges that left the list.
    #[serde(default)]
    pub pending: Vec<UsageEntry>,
}

/// Where an open charge is.
enum Location {
    Listed(usize),
    Pending,
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
    if entry.session == EARLIER_SESSIONS {
        return bad("a usage entry needs a session id");
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
    /// the same charge; use [`UsageLog::settle`] to change it), including a charge that already
    /// left the list (within [`MAX_RETIRED_KEYS`]). An empty key is refused, since it couldn't
    /// tell retries from new charges, and so is an implausibly large cost. Nothing changes on an
    /// error.
    pub fn record(&mut self, entry: UsageEntry) -> Result<bool, Error> {
        check_entry(&entry)?;
        let key = (entry.provider.clone(), entry.idempotency_key.clone());
        if self.keys.contains_key(&key) || self.pending.contains_key(&key) || self.retired_set.contains(&retired_id(&key.0, &key.1)) {
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
        // A new session past the limit folds the least recently used one (into a copy, so an error
        // leaves the log as it was).
        let folded = if self.sessions.contains_key(&entry.session) { None } else { self.folded_sessions(&entry.session)? };
        // Nothing below can fail: the log changes from here on.
        let text = entry_text(&entry);
        while self.entries.len() >= MAX_USAGE_ENTRIES || (!self.entries.is_empty() && self.text_bytes.saturating_add(text) > MAX_LISTED_TEXT) {
            self.retire_oldest();
        }
        let seq = self.first_seq.saturating_add(self.entries.len() as u64);
        self.project = project;
        if let Some(sessions) = folded {
            // Folded sessions are forgotten by the clock too, so it stays as small as the map.
            self.session_clock.retain(|name, _| sessions.contains_key(name));
            self.sessions = sessions;
            // Folded sessions need no recency any more; the clock stays as small as the totals.
            let sessions = &self.sessions;
            self.session_clock.retain(|name, _| sessions.contains_key(name));
        }
        self.sessions.insert(entry.session.clone(), session);
        self.clock = self.clock.saturating_add(1);
        self.session_clock.insert(entry.session.clone(), self.clock);
        self.keys.insert(key, seq);
        self.text_bytes = self.text_bytes.saturating_add(text);
        self.entries.push_back(entry);
        Ok(true)
    }

    /// The oldest entry leaves the list; the totals already include it. An open charge is kept
    /// pending (room permitting) so it can still settle; otherwise its identity is kept so a late
    /// retry isn't charged again.
    fn retire_oldest(&mut self) {
        let Some(oldest) = self.entries.pop_front() else { return };
        self.text_bytes = self.text_bytes.saturating_sub(entry_text(&oldest));
        self.first_seq = self.first_seq.saturating_add(1);
        let key = (oldest.provider.clone(), oldest.idempotency_key.clone());
        self.keys.remove(&key);
        if oldest.status != Charge::Actual && self.pending.len() < MAX_PENDING {
            self.pending.insert(key, oldest);
            return;
        }
        self.retire_identity(&key.0, &key.1);
    }

    fn retire_identity(&mut self, provider: &str, idempotency_key: &str) {
        let id = retired_id(provider, idempotency_key);
        if self.retired_set.insert(id) {
            self.retired.push_back(id);
        }
        while self.retired.len() > MAX_RETIRED_KEYS {
            if let Some(forgotten) = self.retired.pop_front() {
                self.retired_set.remove(&forgotten);
            }
        }
    }

    /// The session totals with room for `current`'s: `None` when there is room already; past
    /// [`MAX_SESSIONS`], the least recently charged sessions other than `current` move into
    /// [`EARLIER_SESSIONS`] (the project total is unchanged).
    fn folded_sessions(&self, current: &str) -> Result<Option<BTreeMap<String, Totals>>, Error> {
        if self.sessions.len() < MAX_SESSIONS {
            return Ok(None);
        }
        let open: BTreeSet<&str> =
            self.entries.iter().filter(|e| e.status != Charge::Actual).chain(self.pending.values()).map(|e| e.session.as_str()).collect();
        let mut sessions = self.sessions.clone();
        // The earlier-sessions total exists before counting, so the first fold frees one slot.
        sessions.entry(EARLIER_SESSIONS.to_owned()).or_default();
        // Least recently charged first; sorted once, so catching up after the limit was exceeded
        // (sessions kept for open charges that have since settled) stays linear.
        let mut candidates: Vec<(u64, String)> = sessions
            .keys()
            .filter(|name| name.as_str() != current && name.as_str() != EARLIER_SESSIONS && !open.contains(name.as_str()))
            .map(|name| (self.session_clock.get(name.as_str()).copied().unwrap_or(0), name.clone()))
            .collect();
        candidates.sort_unstable();
        let mut candidates = candidates.into_iter();
        while sessions.len() >= MAX_SESSIONS {
            let Some((_, name)) = candidates.next() else { break };
            let totals = sessions.remove(&name).unwrap_or_default();
            let earlier = sessions.entry(EARLIER_SESSIONS.to_owned()).or_default();
            for money in totals.into_values() {
                add_to(earlier, money)?;
            }
        }
        Ok(Some(sessions))
    }

    /// The listed, unsettled entry for a key and its index.
    fn open_entry(&self, provider: &str, idempotency_key: &str) -> Result<(Location, &UsageEntry), Error> {
        let key = (provider.to_owned(), idempotency_key.to_owned());
        if let Some(entry) = self.pending.get(&key) {
            return Ok((Location::Pending, entry));
        }
        let seq = *self.keys.get(&key).ok_or_else(|| Error::new(ErrorKind::NotFound, "no open charge has that key (it may have been exported and dropped)"))?;
        let index = usize::try_from(seq.saturating_sub(self.first_seq)).map_err(|_| Error::new(ErrorKind::Internal, "usage log index out of range"))?;
        let entry = self.entries.get(index).ok_or_else(|| Error::new(ErrorKind::Internal, "usage log index out of range"))?;
        if entry.status == Charge::Actual {
            return Err(Error::new(ErrorKind::Conflict, "that charge is already settled"));
        }
        Ok((Location::Listed(index), entry))
    }

    /// Changes a recorded estimate's cost and status: it becomes the actual cost, or stays
    /// charged as unresolved. A charge already settled as actual is final, and the currency can't
    /// change. Totals move by the difference; nothing changes on an error.
    ///
    /// Open charges that left the list stay settleable while pending (see [`MAX_PENDING`]).
    /// `NotFound` means the charge was settled already, or left the list while pending was full:
    /// its last amount stays in the totals (the conservative outcome). Don't `record` it again,
    /// which would count it twice.
    pub fn settle(&mut self, provider: &str, idempotency_key: &str, cost: Money, status: Charge) -> Result<(), Error> {
        if cost.micros > MAX_CHARGE_MICROS {
            return Err(Error::new(ErrorKind::InvalidInput, "a charge is larger than any job could cost"));
        }
        if status == Charge::Estimate {
            return Err(Error::new(ErrorKind::InvalidInput, "settle to an actual or unresolved charge"));
        }
        let (location, entry) = self.open_entry(provider, idempotency_key)?;
        if cost.currency != entry.cost.currency {
            return Err(Error::new(ErrorKind::CurrencyMismatch, "a charge can't change currency when it settles"));
        }
        let mut project = self.project.clone();
        sub_from(&mut project, entry.cost)?;
        add_to(&mut project, cost)?;
        // The same ceiling `record` and a saved log are held to.
        if project.values().any(|m| m.micros > MAX_TOTAL_MICROS) {
            return Err(Error::new(ErrorKind::Overflow, "the project's recorded spend is at its limit"));
        }
        let mut sessions = touched_sessions(&self.sessions, &entry.session);
        take_from_session(&mut sessions, &entry.session, entry.cost)?;
        give_to_session(&mut sessions, &entry.session, cost)?;
        self.project = project;
        self.sessions.extend(sessions);
        match location {
            Location::Listed(index) => {
                if let Some(entry) = self.entries.get_mut(index) {
                    entry.cost = cost;
                    entry.status = status;
                }
            }
            Location::Pending => {
                let key = (provider.to_owned(), idempotency_key.to_owned());
                if status == Charge::Actual {
                    // Settled for good: off the list, so only its identity is kept.
                    self.pending.remove(&key);
                    self.retire_identity(provider, idempotency_key);
                } else if let Some(entry) = self.pending.get_mut(&key) {
                    entry.cost = cost;
                    entry.status = status;
                }
            }
        }
        Ok(())
    }

    /// Removes an unsettled charge for a job the provider certainly never billed (a rejected key,
    /// invalid input), taking it out of the totals and freeing its key for a later attempt.
    /// Only an estimate can be released: an unresolved charge may have been billed.
    pub fn release(&mut self, provider: &str, idempotency_key: &str) -> Result<(), Error> {
        let (location, entry) = self.open_entry(provider, idempotency_key)?;
        if entry.status != Charge::Estimate {
            return Err(Error::new(ErrorKind::Conflict, "only an estimate can be released; this charge may have been billed"));
        }
        let mut project = self.project.clone();
        sub_from(&mut project, entry.cost)?;
        let mut sessions = touched_sessions(&self.sessions, &entry.session);
        take_from_session(&mut sessions, &entry.session, entry.cost)?;
        self.project = project;
        self.sessions.extend(sessions);
        let index = match location {
            Location::Listed(index) => index,
            Location::Pending => {
                self.pending.remove(&(provider.to_owned(), idempotency_key.to_owned()));
                return Ok(());
            }
        };
        if let Some(removed) = self.entries.remove(index) {
            self.text_bytes = self.text_bytes.saturating_sub(entry_text(&removed));
        }
        // Later entries moved up one: renumber the index (rare, so a rebuild is fine).
        self.first_seq = 0;
        self.keys = self.entries.iter().enumerate().map(|(i, e)| ((e.provider.clone(), e.idempotency_key.clone()), i as u64)).collect();
        Ok(())
    }

    /// The listed (or pending) charge for `(provider, idempotency_key)`, if any.
    pub fn charge(&self, provider: &str, idempotency_key: &str) -> Option<&UsageEntry> {
        let key = (provider.to_owned(), idempotency_key.to_owned());
        if let Some(entry) = self.pending.get(&key) {
            return Some(entry);
        }
        let seq = *self.keys.get(&key)?;
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
    /// be read as formulas are prefixed with an apostrophe. Its size is bounded by the list's
    /// limits; [`UsageLog::write_csv`] streams it instead.
    pub fn to_csv(&self) -> String {
        let mut out = Vec::new();
        // Writing to memory can't fail, and every cell is UTF-8.
        let _ = self.write_csv(&mut out);
        String::from_utf8(out).unwrap_or_default()
    }

    /// Streams the CSV of [`UsageLog::to_csv`] to `out`, a row at a time.
    pub fn write_csv(&self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        out.write_all(b"time_ms,session,node,provider,model,amount,currency,status,idempotency_key\r\n")?;
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
            out.write_all(row.join(",").as_bytes())?;
            out.write_all(b"\r\n")?;
        }
        Ok(())
    }
}

impl From<UsageLog> for UsageLogFile {
    fn from(log: UsageLog) -> Self {
        Self {
            entries: log.entries.into(),
            project: log.project,
            sessions: log.sessions,
            retired: log.retired.iter().map(hex).collect(),
            pending: log.pending.into_values().collect(),
        }
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
        if file.retired.len() > MAX_RETIRED_KEYS {
            return Err(bad("too many retired charges"));
        }
        if file.pending.len() > MAX_PENDING {
            return Err(bad("too many pending charges"));
        }
        // Sessions past the limit are only kept for open charges (see `MAX_SESSIONS`), and only
        // fold when a new session arrives, so a log can hold up to one per possible open charge
        // even after those charges settle.
        if file.sessions.len() > MAX_SESSIONS.saturating_add(1).saturating_add(MAX_USAGE_ENTRIES).saturating_add(MAX_PENDING) {
            return Err(bad("too many sessions"));
        }
        let text_bytes = file.entries.iter().fold(0usize, |sum, e| sum.saturating_add(entry_text(e)));
        if text_bytes > MAX_LISTED_TEXT {
            return Err(bad("the entries hold too much text"));
        }
        let mut retired = VecDeque::with_capacity(file.retired.len());
        let mut retired_set = BTreeSet::new();
        for text in &file.retired {
            let id = unhex(text).ok_or_else(|| bad("a retired charge's identity is malformed"))?;
            if !retired_set.insert(id) {
                return Err(bad("a retired charge appears twice"));
            }
            retired.push_back(id);
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
        let mut pending = BTreeMap::new();
        for entry in &file.pending {
            check_entry(entry)?;
            if entry.status == Charge::Actual {
                return Err(bad("a pending charge is already settled"));
            }
            let key = (entry.provider.clone(), entry.idempotency_key.clone());
            if keys.contains_key(&key) || pending.insert(key, entry.clone()).is_some() {
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
        // What a listed session's own total doesn't cover was folded into the earlier sessions'
        // total (a session folded, then charging again, keeps only its new charges).
        let mut shortfall = Totals::new();
        for (name, sums) in &listed_sessions {
            for (currency, sum) in sums {
                let own = file.sessions.get(*name).and_then(|t| t.get(currency)).map_or(0, |m| m.micros);
                if sum.micros > own {
                    add_to(&mut shortfall, Money::new(*currency, sum.micros - own))?;
                }
            }
        }
        if !covers(file.sessions.get(EARLIER_SESSIONS), &shortfall) {
            return Err(bad("a session's totals don't cover its charges"));
        }
        if keys.keys().chain(pending.keys()).any(|(provider, key)| retired_set.contains(&retired_id(provider, key))) {
            return Err(bad("a listed charge is also retired"));
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
        let mut session_clock = BTreeMap::new();
        for (seq, entry) in file.entries.iter().enumerate() {
            session_clock.insert(entry.session.clone(), (seq as u64).saturating_add(1));
        }
        let clock = file.entries.len() as u64;
        Ok(Self {
            entries: file.entries.into(),
            keys,
            first_seq: 0,
            project: file.project,
            sessions: file.sessions,
            retired,
            retired_set,
            pending,
            text_bytes,
            session_clock,
            clock,
        })
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

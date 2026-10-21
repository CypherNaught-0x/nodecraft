//! Cost quotes, budgets and the usage log: what a run will cost before it starts, whether the
//! caps allow it, and what has been spent.
//!
//! Everything here is pure. A node is paid when its kind has a model parameter
//! ([`nodecraft_types::ParamControl::Model`]); its price is the chosen catalog model's price per job times the jobs
//! the step plans. Nodes without a model parameter (sources, local actions) are free, except
//! generation kinds, which can't be priced without one. Totals are
//! kept per currency (never converted), and a step whose price isn't known blocks any capped run,
//! so a budget can't be passed by a quote that was silently incomplete.

use crate::planner::RunPlan;
use nodecraft_catalog::{Catalog, ModelDescriptor};
use nodecraft_graph::{Canvas, Node};
use nodecraft_nodes::Registry;
use nodecraft_types::{Currency, Error, ErrorKind, Money, NodeId, ParamValues};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

mod metadata;
use metadata::Metadata;
pub use metadata::{LedgerIdentity, MAX_METADATA_SHELLS, MetadataFinalizeError, MetadataPermit, MetadataStatistics, UsageMetadata};
mod ownership;
use ownership::Ownership;
pub use ownership::{ChargeOwnership, LegacyGroupEvidence, QuarantineLocation, QuarantinedCharge};
mod reconcile;
mod resolve;
pub use reconcile::{
    Holder, MAX_RECONCILE_EXCEPTIONS, MAX_STAGE_CHARGES, OwnershipDecision, OwnershipGroup, OwnershipPlan, OwnershipReconciliation, ReconciledCharge,
};
pub use resolve::{ChargeResolution, Resolution, ResolutionDecision, ResolvableCharge};

/// Why a step has no price yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unpriced {
    /// The node's model parameter is empty.
    NoModel,
    /// The model isn't in the catalog.
    UnknownModel(String),
    /// The model is priced by usage (tokens, characters) measured during the run.
    ByUsage(String),
    /// The model is priced per second of the source video it edits (#179), and that length isn't
    /// known yet: the source is made earlier in the run, or its length wasn't read.
    AwaitsSource(String),
    /// The node, its kind, the model or its parameters couldn't be priced.
    Invalid(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepQuote {
    pub node: NodeId,
    /// Validated catalog selections in schema order; unknown raw IDs are never report identities.
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
    crate::admission::quote(plan, canvas, kinds, catalog)
}

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

/// Explicit separation of descriptive bucket totals and conservative budget admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageBudgetView {
    pub reported: Spent,
    pub admission: Spent,
    pub has_uncertain_session_charges: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<nodecraft_types::ProviderInstanceId>,
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
/// Unresolved charges stay pending until settled as actual; hosts find them with
/// [`UsageLog::pending`] to settle them. A full pending list only stops later charges from being
/// settleable, never from counting. (An age or size policy for old unresolved charges is
/// deferred.)
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
    provider_identity_version: u8,
    entries: VecDeque<UsageEntry>,
    /// `(provider, idempotency key)` → sequence number of its entry; keys are only unique per
    /// provider. An entry's index is its sequence number minus `first_seq`.
    keys: BTreeMap<[u8; 32], u64>,
    first_seq: u64,
    project: Totals,
    sessions: BTreeMap<String, Totals>,
    /// Identities of charges that left the list, oldest first (see [`MAX_RETIRED_KEYS`]).
    retired: retirement::Store,
    /// Open charges that left the list, by `(provider, key)` (see [`MAX_PENDING`]).
    pending: BTreeMap<(String, Option<nodecraft_types::ProviderInstanceId>, String), UsageEntry>,
    pending_identities: BTreeSet<[u8; 32]>,
    /// Text bytes of the listed entries (see [`MAX_LISTED_TEXT`]).
    text_bytes: usize,
    /// When each session last charged (a counter, not saved: rebuilt from the listed entries), so
    /// folding picks the least recently used.
    session_clock: BTreeMap<String, u64>,
    clock: u64,
    ownership: Ownership,
    metadata: Metadata,
    entry_order: VecDeque<u64>,
    next_order: u64,
}

/// A retired charge's identity: 16 bytes of a hash of its provider and key.
type Retired = retirement::Key;

fn retired_id(provider: &str, idempotency_key: &str) -> Retired {
    let mut hasher = blake3::Hasher::new();
    for text in [provider, idempotency_key] {
        hasher.update(&u64::try_from(text.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(text.as_bytes());
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(hasher.finalize().as_bytes().get(..16).unwrap_or(&[0; 16]));
    let mut tagged = [0u8; 33];
    if let Some(prefix) = tagged.get_mut(1..17) {
        prefix.copy_from_slice(&id);
    }
    retirement::Key(tagged)
}

fn retired_target_id(provider: &str, instance: Option<nodecraft_types::ProviderInstanceId>, key: &str) -> Retired {
    let Some(instance) = instance else {
        return retired_id(provider, key);
    };
    let mut hash = blake3::Hasher::new();
    hash.update(b"nodecraft.provider-instance-retired.v1");
    hash.update(&u64::try_from(provider.len()).unwrap_or(u64::MAX).to_le_bytes());
    hash.update(provider.as_bytes());
    hash.update(&[1]);
    hash.update(instance.as_bytes());
    hash.update(&u64::try_from(key.len()).unwrap_or(u64::MAX).to_le_bytes());
    hash.update(key.as_bytes());
    let mut id = [0u8; 33];
    if let Some(tag) = id.first_mut() {
        *tag = 1;
    }
    if let Some(digest) = id.get_mut(1..) {
        digest.copy_from_slice(hash.finalize().as_bytes());
    }
    retirement::Key(id)
}

/// Open charges always belong to their named session after load admission. Never debit a
/// newer session generation using an own-first/folded fallback (legacy files lacked ownership).
fn take_from_session(sessions: &mut BTreeMap<String, Totals>, name: &str, amount: Money) -> Result<(), Error> {
    let own = sessions.get_mut(name).ok_or_else(|| Error::new(ErrorKind::Internal, "usage totals are missing an open charge's session"))?;
    sub_from(own, amount)
}
fn give_to_session(sessions: &mut BTreeMap<String, Totals>, name: &str, amount: Money) -> Result<(), Error> {
    let own = sessions.get_mut(name).ok_or_else(|| Error::new(ErrorKind::Internal, "usage totals are missing an open charge's session"))?;
    add_to(own, amount)
}

/// Copy the established owner so a failed settlement leaves all totals unchanged.
fn touched_sessions(sessions: &BTreeMap<String, Totals>, name: &str) -> BTreeMap<String, Totals> {
    sessions.get(name).map(|totals| (name.to_owned(), totals.clone())).into_iter().collect()
}

fn entry_text(entry: &UsageEntry) -> usize {
    [&entry.session, &entry.provider, &entry.model, &entry.idempotency_key].iter().fold(0usize, |sum, t| sum.saturating_add(t.len()))
}

fn hex(id: &Retired) -> String {
    id.0.get(1..17).unwrap_or_default().iter().map(|b| format!("{b:02x}")).collect::<String>().into_boxed_str().into_string()
}

fn unhex(text: &str) -> Option<Retired> {
    if text.len() != 32 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let mut id = [0u8; 16];
    for (i, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    let mut tagged = [0u8; 33];
    if let Some(head) = tagged.get_mut(1..17) {
        head.copy_from_slice(&id);
    }
    Some(retirement::Key(tagged))
}

/// Logs are equal when they list the same charges with the same totals (the key index is
/// derived from the entries).
impl PartialEq for UsageLog {
    fn eq(&self, other: &Self) -> bool {
        self.entries().eq(other.entries())
            && self.reported_spent("") == other.reported_spent("")
            && self.merged_sessions() == other.merged_sessions()
            && self.retired.fifo == other.retired.fifo
            && self.provider_identity_version == other.provider_identity_version
            && self.pending == other.pending
            && self.ownership == other.ownership
    }
}
impl Eq for UsageLog {}

/// How a usage log is saved: the listed entries and the totals.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageLogFile {
    /// Explicit ownership contract: 0 legacy input, 1 established, 2 immutable quarantine,
    /// 3 quarantine with confirmed reconciliations (#325), 4 those with resolved charges (#332).
    #[serde(default)]
    pub open_session_ownership: u8,
    #[serde(default)]
    pub provider_identity_version: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncertain_listed: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncertain_pending: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quarantined: Vec<UsageEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legacy_groups: Vec<LegacyGroupEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ownership_reconciliations: Vec<OwnershipReconciliation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub charge_resolutions: Vec<ChargeResolution>,
    pub entries: Vec<UsageEntry>,
    pub project: Totals,
    #[serde(default)]
    pub sessions: BTreeMap<String, Totals>,
    /// Identities (hex) of charges that left the list, oldest first.
    #[serde(default)]
    pub retired: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_targets: Vec<RetiredTargetWire>,
    /// Open charges that left the list.
    #[serde(default)]
    pub pending: Vec<UsageEntry>,
}

/// One tagged fixed digest. This is finite deduplication, never ownership proof.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetiredTargetWire {
    pub tag: u8,
    pub digest: String,
}
fn retired_wire(key: &Retired) -> RetiredTargetWire {
    let tag = key.0.first().copied().unwrap_or(0);
    let range = if tag == 0 { 1..17 } else { 1..33 };
    let digest = key.0.get(range).unwrap_or_default().iter().map(|b| format!("{b:02x}")).collect::<String>().into_boxed_str().into_string();
    RetiredTargetWire { tag, digest }
}
fn parse_retired_wire(wire: &RetiredTargetWire) -> Option<Retired> {
    if wire.tag > 1
        || wire.digest.len() != if wire.tag == 0 { 32 } else { 64 }
        || !wire.digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut key = [0u8; 33];
    *key.first_mut()? = wire.tag;
    for (index, byte) in key.get_mut(1..if wire.tag == 0 { 17 } else { 33 })?.iter_mut().enumerate() {
        *byte = u8::from_str_radix(wire.digest.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(retirement::Key(key))
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
    if amount.micros == 0 {
        return Ok(());
    }
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
    pub fn record(&mut self, mut entry: UsageEntry) -> Result<bool, Error> {
        check_entry(&entry)?;
        self.metadata.sweep();
        if let Some(shell) = self.metadata.lookup(&entry.provider, entry.instance, &entry.idempotency_key) {
            if !shell.matches(&entry.provider, entry.instance, &entry.idempotency_key) {
                return Err(Error::new(ErrorKind::Conflict, "usage identity digest collision"));
            }
            if self.metadata.charge(&entry.provider, entry.instance, &entry.idempotency_key).is_some() {
                return Ok(false);
            }
            return Err(Error::new(ErrorKind::Busy, "that accounting identity has a metadata reservation"));
        }
        let key = ownership::identity_target(&entry.provider, entry.instance, &entry.idempotency_key);
        if let Some(existing) = self.charge_identity(&entry.provider, entry.instance, &entry.idempotency_key)
            && existing.provider == entry.provider
            && existing.instance == entry.instance
            && existing.idempotency_key == entry.idempotency_key
        {
            return Ok(false);
        }
        if self.keys.contains_key(&key) || self.pending_identities.contains(&key) || self.ownership.quarantined.contains_key(&key) {
            return Err(Error::new(ErrorKind::Conflict, "usage identity digest collision"));
        }
        if self.retired.contains(&retired_target_id(&entry.provider, entry.instance, &entry.idempotency_key)) {
            return Ok(false);
        }
        let evict = self.room_for(entry_text(&entry))?;
        let next_order = self.next_order.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "accounting sequence exhausted"))?;
        // New totals first, so an overflow leaves the log as it was.
        let mut project = self.reported_spent(&entry.session).project;
        add_to(&mut project, entry.cost)?;
        let mut session = self.reported_spent(&entry.session).session;
        add_to(&mut session, entry.cost)?;
        // The same ceiling a saved log is held to, so every log that records can be reloaded.
        if project.values().any(|m| m.micros > MAX_TOTAL_MICROS) {
            return Err(Error::new(ErrorKind::Overflow, "the project's recorded spend is at its limit"));
        }
        // A new session past the limit folds the least recently used one (into a copy, so an error
        // leaves the log as it was).
        let folded = if self.sessions.contains_key(&entry.session) { None } else { self.folded_sessions(&entry.session)? };
        let text = entry_text(&entry);
        metadata::compact_entry(&mut entry)?;
        // If no overlay flush will provide its preallocated spare slot, prepare any
        // order growth OFF ledger before later fallible retirement preparation.
        let order = if self.metadata.actuals().next().is_none() && self.entry_order.capacity() < self.entry_order.len().saturating_add(1) {
            let mut order = VecDeque::new();
            order
                .try_reserve_exact(self.entry_order.len().saturating_add(1).max(self.entry_order.capacity().saturating_mul(2)).min(MAX_USAGE_ENTRIES + 1))
                .map_err(|_| Error::new(ErrorKind::Busy, "cannot allocate accounting sequence"))?;
            order.extend(self.entry_order.iter().copied());
            Some(order)
        } else {
            None
        };
        let retired = self.retired.prepare(self.retirement_batch(evict))?;
        self.flush_metadata()?;
        // All fallible admission is complete; commit the planned moves.
        if let Some(order) = order {
            self.entry_order = order;
        }
        for _ in 0..evict {
            self.retire_oldest();
        }
        retired.commit(&mut self.retired);
        let seq = self.first_seq.saturating_add(self.entries.len() as u64);
        self.project = project;
        if let Some(sessions) = folded {
            // Folded sessions are forgotten by the clock too, so it stays as small as the map.
            self.session_clock.retain(|name, _| sessions.contains_key(name));
            self.sessions = sessions;
        }
        self.sessions.insert(entry.session.clone(), session);
        self.clock = self.clock.saturating_add(1);
        self.session_clock.insert(entry.session.clone(), self.clock);
        self.keys.insert(key, seq);
        self.text_bytes = self.text_bytes.saturating_add(text);
        let entry_instance_named = entry.instance.is_some();
        self.entries.push_back(entry);
        self.entry_order.push_back(next_order);
        self.next_order = next_order;
        if entry_instance_named {
            self.provider_identity_version = 1;
        }
        Ok(true)
    }

    /// The oldest entry leaves the list; the totals already include it. An open charge is kept
    /// pending (room permitting) so it can still settle; otherwise its identity is kept so a late
    /// retry isn't charged again.
    fn retire_oldest(&mut self) {
        let Some(oldest) = self.entries.pop_front() else { return };
        self.entry_order.pop_front();
        self.text_bytes = self.text_bytes.saturating_sub(entry_text(&oldest));
        self.first_seq = self.first_seq.saturating_add(1);
        self.keys.remove(&ownership::identity_target(&oldest.provider, oldest.instance, &oldest.idempotency_key));
        if self.ownership.is_uncertain(&oldest) {
            self.ownership.quarantined.insert(ownership::identity_target(&oldest.provider, oldest.instance, &oldest.idempotency_key), oldest);
            return;
        }
        if oldest.status != Charge::Actual && self.pending.len() < MAX_PENDING {
            self.pending_identities.insert(ownership::identity_target(&oldest.provider, oldest.instance, &oldest.idempotency_key));
            self.pending.insert((oldest.provider.clone(), oldest.instance, oldest.idempotency_key.clone()), oldest);
        }
        // The enclosing admitted transaction commits its staged retirement delta.
    }

    fn retirement_batch(&self, evict: usize) -> impl Iterator<Item = Retired> + '_ {
        let mut pending = self.pending.len();
        self.entries().take(evict).filter_map(move |entry| {
            if self.ownership.is_uncertain(entry) {
                return None;
            }
            if entry.status != Charge::Actual && pending < MAX_PENDING {
                pending += 1;
                return None;
            }
            Some(retired_target_id(&entry.provider, entry.instance, &entry.idempotency_key))
        })
    }

    /// The session totals with room for `current`'s: `None` when there is room already; past
    /// [`MAX_SESSIONS`], the least recently charged sessions other than `current` move into
    /// [`EARLIER_SESSIONS`] (the project total is unchanged).
    fn folded_sessions(&self, current: &str) -> Result<Option<BTreeMap<String, Totals>>, Error> {
        if self.merged_sessions().len() < MAX_SESSIONS {
            return Ok(None);
        }
        let open: BTreeSet<&str> =
            self.entries.iter().filter(|e| e.status != Charge::Actual).chain(self.pending.values()).map(|e| e.session.as_str()).collect();
        let mut sessions = self.merged_sessions();
        // The earlier-sessions total exists before counting, so the first fold frees one slot.
        sessions.entry(EARLIER_SESSIONS.to_owned()).or_default();
        // Least recently charged first; sorted once, so catching up after the limit was exceeded
        // (sessions kept for open charges that have since settled) stays linear.
        let mut candidates: Vec<(u64, String)> = sessions
            .keys()
            .filter(|name| {
                name.as_str() != current
                    && name.as_str() != EARLIER_SESSIONS
                    && !open.contains(name.as_str())
                    && !self.ownership.groups.contains_key(name.as_str())
                    && !self.metadata.protects(name.as_str())
            })
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
    fn open_entry(
        &self,
        provider: &str,
        instance: Option<nodecraft_types::ProviderInstanceId>,
        idempotency_key: &str,
    ) -> Result<(Location, &UsageEntry), Error> {
        ownership::check_key(provider, idempotency_key)?;
        if self.ownership.uncertain.contains(&ownership::identity_target(provider, instance, idempotency_key)) {
            return Err(ownership::uncertain_error());
        }
        let key = ownership::identity_target(provider, instance, idempotency_key);
        if self.pending_identities.contains(&key) {
            let entry = self
                .pending
                .values()
                .find(|e| e.provider == provider && e.instance == instance && e.idempotency_key == idempotency_key)
                .ok_or_else(|| Error::new(ErrorKind::Conflict, "usage identity digest collision"))?;
            return Ok((Location::Pending, entry));
        }
        let seq = *self.keys.get(&key).ok_or_else(|| Error::new(ErrorKind::NotFound, "no open charge has that key (it may have been exported and dropped)"))?;
        let index = usize::try_from(seq.saturating_sub(self.first_seq)).map_err(|_| Error::new(ErrorKind::Internal, "usage log index out of range"))?;
        let entry = self.entries.get(index).ok_or_else(|| Error::new(ErrorKind::Internal, "usage log index out of range"))?;
        if entry.provider != provider || entry.instance != instance || entry.idempotency_key != idempotency_key {
            return Err(Error::new(ErrorKind::Conflict, "usage identity digest collision"));
        }
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
    /// `Conflict` means a listed charge is already settled as actual. `NotFound` means no open
    /// charge has that key: never recorded, released, settled after leaving the list, or left the
    /// list while pending was full (its last amount stays in the totals, the conservative
    /// outcome). Don't `record` it again, which would count it twice.
    pub fn settle(&mut self, provider: &str, idempotency_key: &str, cost: Money, status: Charge) -> Result<(), Error> {
        self.settle_identity(provider, None, idempotency_key, cost, status)
    }
    pub fn settle_target(&mut self, target: &nodecraft_types::ProviderTarget, idempotency_key: &str, cost: Money, status: Charge) -> Result<(), Error> {
        self.settle_identity(target.adapter(), target.instance(), idempotency_key, cost, status)
    }
    fn settle_identity(
        &mut self,
        provider: &str,
        instance: Option<nodecraft_types::ProviderInstanceId>,
        idempotency_key: &str,
        cost: Money,
        status: Charge,
    ) -> Result<(), Error> {
        if cost.micros > MAX_CHARGE_MICROS {
            return Err(Error::new(ErrorKind::InvalidInput, "a charge is larger than any job could cost"));
        }
        if status == Charge::Estimate {
            return Err(Error::new(ErrorKind::InvalidInput, "settle to an actual or unresolved charge"));
        }
        let (location, entry) = self.open_entry(provider, instance, idempotency_key)?;
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
        if project
            .get(&cost.currency)
            .map_or(0, |m| m.micros)
            .checked_add(self.metadata.currency_sum(None, cost.currency).unwrap_or(u64::MAX))
            .is_none_or(|n| n > MAX_TOTAL_MICROS)
        {
            return Err(Error::new(ErrorKind::Overflow, "the project's recorded spend is at its limit"));
        }
        let mut sessions = touched_sessions(&self.sessions, &entry.session);
        take_from_session(&mut sessions, &entry.session, entry.cost)?;
        give_to_session(&mut sessions, &entry.session, cost)?;
        let retired = self.retired.prepare(
            if matches!(location, Location::Pending) && status == Charge::Actual { Some(retired_target_id(provider, instance, idempotency_key)) } else { None }
                .into_iter(),
        )?;
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
                let key = (provider.to_owned(), instance, idempotency_key.to_owned());
                if status == Charge::Actual {
                    // Settled for good: off the list, so only its identity is kept.
                    self.pending.remove(&key);
                    self.pending_identities.remove(&ownership::identity_target(provider, instance, idempotency_key));
                    // Finite dedup delta was admitted before the monetary update.
                } else if let Some(entry) = self.pending.get_mut(&key) {
                    entry.cost = cost;
                    entry.status = status;
                }
            }
        }
        retired.commit(&mut self.retired);
        Ok(())
    }

    /// Removes an unsettled charge for a job the provider certainly never billed (a rejected key,
    /// invalid input), taking it out of the totals and freeing its key for a later attempt.
    /// Only an estimate can be released: an unresolved charge may have been billed.
    pub fn release(&mut self, provider: &str, idempotency_key: &str) -> Result<(), Error> {
        self.release_identity(provider, None, idempotency_key)
    }
    pub fn release_target(&mut self, target: &nodecraft_types::ProviderTarget, idempotency_key: &str) -> Result<(), Error> {
        self.release_identity(target.adapter(), target.instance(), idempotency_key)
    }
    fn release_identity(&mut self, provider: &str, instance: Option<nodecraft_types::ProviderInstanceId>, idempotency_key: &str) -> Result<(), Error> {
        self.release_open(provider, instance, idempotency_key, false)
    }
    /// Releases an open charge a person confirmed the provider never billed, even an unresolved
    /// one (#332).
    fn release_confirmed(&mut self, provider: &str, idempotency_key: &str) -> Result<(), Error> {
        self.release_open(provider, None, idempotency_key, true)
    }
    fn release_open(
        &mut self,
        provider: &str,
        instance: Option<nodecraft_types::ProviderInstanceId>,
        idempotency_key: &str,
        confirmed: bool,
    ) -> Result<(), Error> {
        let (location, entry) = self.open_entry(provider, instance, idempotency_key)?;
        if !confirmed && entry.status != Charge::Estimate {
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
                self.pending.remove(&(provider.to_owned(), instance, idempotency_key.to_owned()));
                self.pending_identities.remove(&ownership::identity_target(provider, instance, idempotency_key));
                return Ok(());
            }
        };
        self.entry_order.remove(index);
        if let Some(removed) = self.entries.remove(index) {
            self.text_bytes = self.text_bytes.saturating_sub(entry_text(&removed));
        }
        // Later entries moved up one: renumber the index (rare, so a rebuild is fine).
        self.first_seq = 0;
        self.keys = self.entries.iter().enumerate().map(|(i, e)| (ownership::identity_target(&e.provider, e.instance, &e.idempotency_key), i as u64)).collect();
        Ok(())
    }

    /// The listed (or pending) charge for `(provider, idempotency_key)`, if any.
    pub fn charge(&self, provider: &str, idempotency_key: &str) -> Option<&UsageEntry> {
        self.charge_identity(provider, None, idempotency_key)
    }
    pub fn charge_target(&self, target: &nodecraft_types::ProviderTarget, idempotency_key: &str) -> Option<&UsageEntry> {
        self.charge_identity(target.adapter(), target.instance(), idempotency_key)
    }
    fn charge_identity(&self, provider: &str, instance: Option<nodecraft_types::ProviderInstanceId>, idempotency_key: &str) -> Option<&UsageEntry> {
        if ownership::check_key(provider, idempotency_key).is_err() {
            return None;
        }
        if let Some(entry) = self.metadata.charge(provider, instance, idempotency_key) {
            return Some(entry);
        }
        if let Some(entry) = self.ownership.quarantined.get(&ownership::identity_target(provider, instance, idempotency_key)) {
            return (entry.provider == provider && entry.instance == instance && entry.idempotency_key == idempotency_key).then_some(entry);
        }
        let key = ownership::identity_target(provider, instance, idempotency_key);
        if self.pending_identities.contains(&key) {
            return self.pending.values().find(|e| e.provider == provider && e.instance == instance && e.idempotency_key == idempotency_key);
        }
        let seq = *self.keys.get(&key)?;
        self.entries
            .get(usize::try_from(seq.checked_sub(self.first_seq)?).ok()?)
            .filter(|e| e.provider == provider && e.instance == instance && e.idempotency_key == idempotency_key)
    }

    /// Open charges that left the list, still to be settled or released.
    pub fn pending(&self) -> impl Iterator<Item = &UsageEntry> {
        self.pending.values()
    }

    /// How many entries are listed.
    pub fn listed(&self) -> usize {
        self.entries.len().saturating_add(self.metadata.actuals().count())
    }

    pub fn entries(&self) -> impl Iterator<Item = &UsageEntry> {
        let mut ordinary = self.entry_order.iter().zip(self.entries.iter()).peekable();
        let mut overlay = self.metadata.actuals().peekable();
        std::iter::from_fn(move || match (ordinary.peek(), overlay.peek()) {
            (Some((a, _)), Some((b, _))) if **a <= *b => ordinary.next().map(|(_, e)| e),
            (Some(_), Some(_)) | (None, Some(_)) => overlay.next().map(|(_, e)| e),
            (Some(_), None) => ordinary.next().map(|(_, e)| e),
            (None, None) => None,
        })
    }

    /// Totals per currency for `session` and for the whole project, including entries that have
    /// left the list.
    pub fn spent(&self, session: &str) -> Spent {
        let mut spent = self.reported_spent(session);
        for group in self.ownership.groups.get(session).into_iter().flat_map(|groups| groups.values()) {
            let currency = group.original_open.currency;
            let named = spent.session.get(&currency).map_or(0, |m| m.micros);
            let project = spent.project.get(&currency).map_or(0, |m| m.micros);
            let admitted = (u128::from(named) + u128::from(group.original_open.micros)).min(u128::from(project));
            spent.session.insert(currency, Money::new(currency, u64::try_from(admitted).unwrap_or(project)));
        }
        spent
    }

    /// Exact declared bucket totals; use `spent` for conservative budget admission.
    pub fn reported_spent(&self, session: &str) -> Spent {
        let mut spent = Spent { session: self.sessions.get(session).cloned().unwrap_or_default(), project: self.project.clone() };
        for (_, e) in self.metadata.actuals() {
            // Finalization proved every combined total below the fixed ceiling.
            let p = spent.project.entry(e.cost.currency).or_insert(Money::zero(e.cost.currency));
            p.micros = p.micros.saturating_add(e.cost.micros);
            if e.session == session {
                let s = spent.session.entry(e.cost.currency).or_insert(Money::zero(e.cost.currency));
                s.micros = s.micros.saturating_add(e.cost.micros);
            }
        }
        spent
    }

    pub fn budget_view(&self, session: &str) -> UsageBudgetView {
        UsageBudgetView {
            reported: self.reported_spent(session),
            admission: self.spent(session),
            has_uncertain_session_charges: self.ownership.groups.contains_key(session),
        }
    }

    fn merged_sessions(&self) -> BTreeMap<String, Totals> {
        let mut sessions = self.sessions.clone();
        for (_, e) in self.metadata.actuals() {
            let value = sessions.entry(e.session.clone()).or_default().entry(e.cost.currency).or_insert(Money::zero(e.cost.currency));
            value.micros = value.micros.saturating_add(e.cost.micros);
        }
        sessions
    }

    pub fn ownership(&self, provider: &str, key: &str) -> Result<Option<ChargeOwnership>, Error> {
        self.ownership_identity(provider, None, key)
    }
    pub fn ownership_target(&self, target: &nodecraft_types::ProviderTarget, key: &str) -> Result<Option<ChargeOwnership>, Error> {
        self.ownership_identity(target.adapter(), target.instance(), key)
    }
    fn ownership_identity(&self, provider: &str, instance: Option<nodecraft_types::ProviderInstanceId>, key: &str) -> Result<Option<ChargeOwnership>, Error> {
        ownership::check_key(provider, key)?;
        let entry = self.charge_identity(provider, instance, key);
        if entry.is_none() && self.digest_present(provider, instance, key) {
            return Err(Error::new(ErrorKind::Conflict, "usage identity digest collision"));
        }
        Ok(entry.map(|entry| if self.ownership.is_uncertain(entry) { ChargeOwnership::LegacyUncertain } else { ChargeOwnership::Established }))
    }

    /// Logical retired-container charge: fixed 33-byte keys times reported capacities plus
    /// public container headers. Excludes allocator/private bucket metadata and is not RSS.
    pub fn retired_logical_bytes(&self) -> usize {
        self.retired.logical_bytes()
    }
    /// Bounded identity deduplication, including retained historical Actuals and retired keys.
    pub fn has_recorded(&self, provider: &str, key: &str) -> Result<bool, Error> {
        self.has_recorded_identity(provider, None, key)
    }
    pub fn has_recorded_target(&self, target: &nodecraft_types::ProviderTarget, key: &str) -> Result<bool, Error> {
        self.has_recorded_identity(target.adapter(), target.instance(), key)
    }
    fn has_recorded_identity(&self, provider: &str, instance: Option<nodecraft_types::ProviderInstanceId>, key: &str) -> Result<bool, Error> {
        ownership::check_key(provider, key)?;
        if self.charge_identity(provider, instance, key).is_some() {
            return Ok(true);
        }
        if self.digest_present(provider, instance, key) {
            return Err(Error::new(ErrorKind::Conflict, "usage identity digest collision"));
        }
        Ok(self.retired.contains(&retired_target_id(provider, instance, key)))
    }

    fn digest_present(&self, provider: &str, instance: Option<nodecraft_types::ProviderInstanceId>, key: &str) -> bool {
        let id = ownership::identity_target(provider, instance, key);
        self.keys.contains_key(&id)
            || self.pending_identities.contains(&id)
            || self.ownership.quarantined.contains_key(&id)
            || self.metadata.lookup(provider, instance, key).is_some_and(|s| !s.matches(provider, instance, key))
    }
    pub fn quarantine_entries(&self) -> impl Iterator<Item = QuarantinedCharge<'_>> {
        self.entries
            .iter()
            .map(|e| (e, QuarantineLocation::Listed))
            .chain(self.pending.values().map(|e| (e, QuarantineLocation::Pending)))
            .chain(self.ownership.quarantined.values().map(|e| (e, QuarantineLocation::Evicted)))
            .filter_map(|(entry, location)| {
                self.ownership
                    .is_uncertain(entry)
                    .then(|| self.ownership.evidence(entry).map(|evidence| QuarantinedCharge { entry, evidence, location }))
                    .flatten()
            })
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
        self.write_csv_rows(out, 0, usize::MAX)
    }

    /// The CSV header and up to `limit` listed rows from `offset`, including finalized Actuals.
    /// Held metadata and evicted quarantine are not listed rows.
    pub fn write_csv_rows(&self, out: &mut impl std::io::Write, offset: usize, limit: usize) -> std::io::Result<()> {
        self.write_csv_rows_while(out, offset, limit, |_| true).map(|_| ())
    }

    /// Writes the same header and rows as [`Self::write_csv_rows`], stopping before the first
    /// complete row rejected by `accept`. The borrowed row includes CRLF; no partial rejected
    /// row is written. Returns the number written (excluding the header). Formatting scratch is
    /// bounded by the admitted entry's four [`MAX_TEXT`] fields, not by the page size.
    pub fn write_csv_rows_while(
        &self,
        out: &mut impl std::io::Write,
        offset: usize,
        limit: usize,
        accept: impl FnMut(&[u8]) -> bool,
    ) -> std::io::Result<usize> {
        if self.entries().skip(offset).take(limit).any(|e| e.instance.is_some()) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "scoped usage requires scoped CSV export"));
        }
        self.write_csv_target_rows_while(out, offset, limit, accept, false)
    }
    /// Explicit instance column for all identities; safe for scoped/default mixed pages.
    pub fn write_scoped_csv_rows(&self, out: &mut impl std::io::Write, offset: usize, limit: usize) -> std::io::Result<()> {
        self.write_scoped_csv_rows_while(out, offset, limit, |_| true).map(|_| ())
    }
    pub fn write_scoped_csv_rows_while(
        &self,
        out: &mut impl std::io::Write,
        offset: usize,
        limit: usize,
        accept: impl FnMut(&[u8]) -> bool,
    ) -> std::io::Result<usize> {
        self.write_csv_target_rows_while(out, offset, limit, accept, true)
    }
    fn write_csv_target_rows_while(
        &self,
        out: &mut impl std::io::Write,
        offset: usize,
        limit: usize,
        mut accept: impl FnMut(&[u8]) -> bool,
        scoped: bool,
    ) -> std::io::Result<usize> {
        out.write_all(if scoped {
            b"time_ms,session,node,provider,model,amount,currency,status,idempotency_key,provider_instance\r\n"
        } else {
            b"time_ms,session,node,provider,model,amount,currency,status,idempotency_key\r\n"
        })?;
        let mut written = 0;
        for e in self.entries().skip(offset).take(limit) {
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
            let mut row = row.join(",");
            if scoped {
                row.push(',');
                if let Some(instance) = e.instance {
                    row.push_str(&instance.to_string());
                }
            }
            let body_len = row.len();
            row.push_str("\r\n");
            if !accept(row.as_bytes()) {
                break;
            }
            // Preserve the existing writer's row-body/CRLF calls and error propagation.
            out.write_all(row.get(..body_len).unwrap_or_default().as_bytes())?;
            out.write_all(b"\r\n")?;
            written += 1;
        }
        Ok(written)
    }

    /// Streaming export including evicted immutable uncertainty; ordinary CSV remains listed-only.
    pub fn write_quarantine_csv(&self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        if self.quarantine_entries().any(|row| row.entry.instance.is_some()) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "scoped usage requires scoped CSV export"));
        }
        write_entries_csv(self.quarantine_entries().map(|row| row.entry), out)
    }
}

fn write_entries_csv<'a>(entries: impl Iterator<Item = &'a UsageEntry>, out: &mut impl std::io::Write) -> std::io::Result<()> {
    out.write_all(b"time_ms,session,node,provider,model,amount,currency,status,idempotency_key\r\n")?;
    for e in entries {
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

impl From<UsageLog> for UsageLogFile {
    fn from(log: UsageLog) -> Self {
        let uncertain_listed = log.entries().enumerate().filter(|(_, e)| log.ownership.is_uncertain(e)).filter_map(|(n, _)| u32::try_from(n).ok()).collect();
        let uncertain_pending =
            log.pending.values().enumerate().filter(|(_, e)| log.ownership.is_uncertain(e)).filter_map(|(n, _)| u32::try_from(n).ok()).collect();
        let entries = log.entries().cloned().collect();
        let project = log.reported_spent("").project;
        let sessions = log.merged_sessions();
        Self {
            provider_identity_version: log.provider_identity_version,
            open_session_ownership: if !log.ownership.resolutions.is_empty() {
                4
            } else if !log.ownership.records.is_empty() {
                3
            } else if log.ownership.uncertain.is_empty() {
                1
            } else {
                2
            },
            uncertain_listed,
            uncertain_pending,
            quarantined: log.ownership.quarantined.into_values().collect(),
            legacy_groups: log.ownership.groups.into_values().flat_map(|g| g.into_values()).collect(),
            ownership_reconciliations: log.ownership.records,
            charge_resolutions: log.ownership.resolutions,
            entries,
            project,
            sessions,
            retired: if log.provider_identity_version == 0 { log.retired.fifo.iter().map(hex).collect() } else { Vec::new() },
            retired_targets: if log.provider_identity_version == 1 {
                log.retired.fifo.iter().map(retired_wire).collect::<Vec<_>>().into_boxed_slice().into_vec()
            } else {
                Vec::new()
            },
            pending: log.pending.into_values().collect(),
        }
    }
}

/// A saved log is checked like new input: bounded, keyed, and with totals that cover its
/// entries (a file can't claim less was spent than it lists).
impl TryFrom<UsageLogFile> for UsageLog {
    type Error = Error;

    fn try_from(mut file: UsageLogFile) -> Result<Self, Error> {
        let bad = |m: &str| Error::new(ErrorKind::InvalidInput, format!("usage log: {m}"));
        ownership::check_file_bounds(&file)?;
        if file.entries.len() > MAX_USAGE_ENTRIES {
            return Err(bad("too many entries"));
        }
        if file.retired.len().saturating_add(file.retired_targets.len()) > MAX_RETIRED_KEYS {
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
        if (file.provider_identity_version == 0 && !file.retired_targets.is_empty()) || (file.provider_identity_version == 1 && !file.retired.is_empty()) {
            return Err(bad("retired identity representation disagrees with provider identity version"));
        }
        // Length/tag/hex validation precedes owned fixed-size digest construction.
        if file.retired.iter().any(|s| unhex(s).is_none()) || file.retired_targets.iter().any(|s| parse_retired_wire(s).is_none()) {
            return Err(bad("a retired charge identity is malformed"));
        }
        let retired = retirement::Store::admit(file.retired.iter().filter_map(|s| unhex(s)).chain(file.retired_targets.iter().filter_map(parse_retired_wire)))?;
        let mut listed = Totals::new();
        let mut listed_sessions: BTreeMap<&str, Totals> = BTreeMap::new();
        let mut keys = BTreeMap::new();
        for (seq, entry) in file.entries.iter().enumerate() {
            check_entry(entry)?;
            if keys.insert((entry.provider.clone(), entry.instance, entry.idempotency_key.clone()), seq as u64).is_some() {
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
            let key = (entry.provider.clone(), entry.instance, entry.idempotency_key.clone());
            if keys.contains_key(&key) || pending.insert(key, entry.clone()).is_some() {
                return Err(bad("an idempotency key appears twice"));
            }
            add_to(&mut listed, entry.cost)?;
            add_to(listed_sessions.entry(entry.session.as_str()).or_default(), entry.cost)?;
        }
        let mut quarantine_keys = BTreeSet::new();
        for entry in &file.quarantined {
            check_entry(entry)?;
            if entry.status == Charge::Actual || entry.cost.micros == 0 {
                return Err(bad("quarantined charges must remain positive and open"));
            }
            let key = (entry.provider.clone(), entry.instance, entry.idempotency_key.clone());
            if keys.contains_key(&key) || pending.contains_key(&key) || !quarantine_keys.insert(key) {
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
        if keys
            .keys()
            .chain(pending.keys())
            .chain(quarantine_keys.iter())
            .any(|(provider, instance, key)| retired.contains(&retired_target_id(provider, *instance, key)))
        {
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
        let ownership = Ownership::admit(&mut file)?;
        let mut session_clock = BTreeMap::new();
        for (seq, entry) in file.entries.iter().enumerate() {
            session_clock.insert(entry.session.clone(), (seq as u64).saturating_add(1));
        }
        let clock = file.entries.len() as u64;
        let entry_order = (0..clock).collect();
        let mut listed_entries = VecDeque::new();
        listed_entries.try_reserve_exact(file.entries.len()).map_err(|_| Error::new(ErrorKind::Busy, "cannot allocate admitted usage entries"))?;
        for mut entry in file.entries {
            metadata::compact_entry(&mut entry)?;
            listed_entries.push_back(entry);
        }
        for entry in pending.values_mut() {
            metadata::compact_entry(entry)?;
        }
        let mut ownership = ownership;
        for entry in ownership.quarantined.values_mut() {
            metadata::compact_entry(entry)?;
        }
        let sessions = file
            .sessions
            .into_iter()
            .map(|(name, totals)| metadata::compact_session(&name).map(|name| (name, totals)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(Self {
            provider_identity_version: file.provider_identity_version,
            entries: listed_entries,
            keys: keys.into_iter().map(|((p, instance, k), seq)| (ownership::identity_target(&p, instance, &k), seq)).collect(),
            first_seq: 0,
            project: file.project,
            sessions,
            retired,
            pending_identities: pending.values().map(|e| ownership::identity_target(&e.provider, e.instance, &e.idempotency_key)).collect(),
            pending,
            text_bytes,
            session_clock,
            clock,
            ownership,
            metadata: Metadata::default(),
            entry_order,
            next_order: clock,
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

mod retirement;

#[cfg(test)]
mod target_tests;

//! Run commands (#201): price a run before it starts, start it within the budgets, record its
//! results, and export what was spent.
//!
//! - `run.quote` (read-only): plans a request and prices it. The response has the steps, each
//!   step's price or why it has none, totals per currency, the budget blocks, and the steps
//!   admitted only provisionally (their job counts aren't known yet).
//! - `run.start`: quotes again and refuses when the budgets block the run. The caller must also
//!   `accept` the price it was shown: amounts covering the current totals, and `partial: true`
//!   when some step can't be priced. So no caller (UI, CLI, agent) starts a paid run without
//!   seeing its cost first, and a price that grew since the quote is refused. The run itself goes
//!   to the host's `run.start` service, which plans the same request and runs it. The accepted
//!   amounts and the accepted count of unknown-price jobs stay with the authorized run as its
//!   cumulative ceiling (see [`crate::run_ceiling`]), so deferred fan-out and retries can't
//!   reserve more, or admit more unknown-price jobs, than was accepted.
//! - `run.record` (host only): records a run's results, one generation per node, as one undo
//!   step. A result marked `keep` (a partial answer kept by hand) doesn't replace the node's
//!   active result.
//! - `usage.export_csv` (read-only): spreadsheet-safe CSV pages, at most 2,000 rows and
//!   768 KiB of encoded JSON including the envelope. Continue at the returned actual row offset.
//!
//! Requiring a configured budget for automation-started paid runs remains desktop-host policy
//! for #201 part 2 / #293; these commands do not implement that future host.
//!
//! The model catalog, the caps with what has been spent, and the usage log are host resources
//! ([`RunCatalog`], [`BudgetState`], [`UsageSnapshot`]); without them the commands are
//! unsupported.

use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_catalog::Catalog;
use nodecraft_exec::cost::{Budgets, Spent, StepQuote, Unpriced, UsageLog, check, format_money};
use nodecraft_exec::planner::{Planned, RunMode, plan_expanded};
use nodecraft_exec::run::{keep_changes, record_changes};
use nodecraft_graph::{Changeset, Generation};
use nodecraft_types::{CanvasId, Error, ErrorKind, FieldId, Money, NodeId};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The host service that runs a started run.
pub const RUN_SERVICE: &str = "run.start";
/// Most nodes one run request may name.
pub const MAX_REQUESTED: usize = 10_000;
/// Most results one `run.record` may carry. Results carry asset references, not media bytes:
/// the whole request must also fit the command parameter limits (1 MiB of text).
pub const MAX_RESULTS: usize = 1_000;
/// Most steps, blockers and node ids a quote lists (the totals always cover the whole run).
pub const MAX_LISTED: usize = 500;
/// Most usage rows one `usage.export_csv` page returns.
pub const MAX_EXPORT_ROWS: usize = 2_000;

/// Immutable catalog definition snapshot. Clone preserves identity; a newly constructed
/// snapshot invalidates earlier launch consent, even when all prices happen to match.
#[derive(Clone)]
pub struct RunCatalog(Arc<CatalogSnapshot>);
struct CatalogSnapshot {
    catalog: Catalog,
    nonce: [u8; 32],
}
impl RunCatalog {
    pub fn new(catalog: Catalog) -> Result<Self, Error> {
        Ok(Self(Arc::new(CatalogSnapshot { catalog, nonce: crate::execution_identity::nonce()? })))
    }
    pub(crate) fn definition_identity(&self) -> [u8; 32] {
        self.0.nonce
    }
    /// Same immutable catalog admission snapshot; content equality alone is not consent.
    pub fn same_snapshot(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn catalog(&self) -> &Catalog {
        &self.0.catalog
    }
}

/// The caps and conservative admission spend (a host resource).
/// Prefer [`BudgetState::from_usage`] when constructing this from a usage log; exact reporting
/// totals alone can understate an uncertain historical session's admission exposure.
pub struct BudgetState {
    pub budgets: Budgets,
    pub spent: Spent,
}

impl BudgetState {
    /// Includes finalized Actuals and the conservative uncertain-session upper bound.
    pub fn from_usage(budgets: Budgets, usage: &UsageLog, session: &str) -> Self {
        Self { budgets, spent: usage.spent(session) }
    }
}

/// The usage log, for listed-row export (a host resource). Finalized Actuals are included;
/// evicted uncertain rows remain available through UsageLog's separate quarantine export.
pub struct UsageSnapshot(pub Arc<UsageLog>);

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum ModeParam {
    Selected,
    WithUpstream,
    Field,
    AllStale,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunParams {
    canvas: CanvasId,
    mode: ModeParam,
    #[serde(default)]
    nodes: Vec<NodeId>,
    #[serde(default)]
    field: Option<FieldId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Accept {
    /// The `token` of the quote the caller was shown.
    token: String,
    /// The most the caller agreed to, per currency code, in micro-units.
    #[serde(default)]
    amounts: BTreeMap<String, u64>,
    /// The caller agreed although part of the price isn't known: a step has no price, or its job
    /// count is known only at run time (it could cost more than quoted).
    #[serde(default)]
    partial: bool,
    /// How many jobs of unknown price the run may admit (#366). Required when a step has no
    /// price; absent means none, never unlimited.
    #[serde(default)]
    unpriced_jobs: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartParams {
    canvas: CanvasId,
    mode: ModeParam,
    #[serde(default)]
    nodes: Vec<NodeId>,
    #[serde(default)]
    field: Option<FieldId>,
    accept: Accept,
}

impl StartParams {
    fn split(self) -> (RunParams, Accept) {
        (RunParams { canvas: self.canvas, mode: self.mode, nodes: self.nodes, field: self.field }, self.accept)
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn check_requested(value: &Value) -> Result<(), Error> {
    if value.get("nodes").and_then(Value::as_array).is_some_and(|nodes| nodes.len() > MAX_REQUESTED) {
        return Err(invalid(format!("a run names at most {MAX_REQUESTED} nodes")));
    }
    Ok(())
}

fn mode_of(p: &RunParams) -> Result<RunMode, Error> {
    if p.nodes.len() > MAX_REQUESTED {
        return Err(invalid(format!("a run names at most {MAX_REQUESTED} nodes")));
    }
    Ok(match p.mode {
        ModeParam::Selected => RunMode::Selected,
        ModeParam::WithUpstream => RunMode::WithUpstream,
        ModeParam::AllStale => RunMode::AllStale,
        ModeParam::Field => RunMode::Field(p.field.ok_or_else(|| invalid("a field run needs `field`"))?),
    })
}

fn money(m: Money) -> Value {
    json!({"currency": String::from(m.currency), "micros": m.micros, "text": format_money(m)})
}

fn unpriced_reason(why: &Unpriced) -> String {
    match why {
        Unpriced::NoModel => "no model is chosen".into(),
        Unpriced::UnknownModel(_) => "the selected model is not in the catalog".into(),
        Unpriced::ByUsage(id) => format!("{id} is priced by usage, measured during the run"),
        Unpriced::Invalid(message) => message.clone(),
    }
}

/// A quoted step, named by the node on the real canvas it runs for: a node inlined from a technique
/// is priced on the technique node.
fn step_json(step: &StepQuote, node: NodeId) -> Value {
    let (price, reason) = match step.total() {
        Ok(Some(total)) => (money(total), Value::Null),
        Ok(None) => (Value::Null, Value::Null),
        Err(why) => (Value::Null, json!(unpriced_reason(&why))),
    };
    // StepQuote.models contains validated catalog identities, independent of which
    // later control supplied the final unpriced reason.
    json!({"node": node, "jobs": step.jobs_known.then_some(step.jobs), "models": step.models, "price": price, "unpriced": reason})
}

/// A request priced against a document: the response `run.quote` returns, and what `run.start`
/// checks.
pub struct Priced {
    /// The `run.quote` response.
    pub response: Value,
    /// Totals per currency code, in micro-units.
    pub totals: BTreeMap<String, u64>,
    /// Every step priced and every job count known: the totals are the whole cost.
    pub complete: bool,
    /// The budgets allow the run (possibly provisionally).
    pub allowed: bool,
    /// Live Session/source-generation, immutable catalog identity and canonical request.
    /// Budgets remain live rechecks; the trusted host must retain the verified execution
    /// snapshot/definitions after checking this token and before scheduling.
    pub token: String,
}

/// Prices the ACTUAL live Session. The returned launch authority cannot be minted
/// from a copied Project and a caller-asserted revision. Hosts enqueue service requests,
/// then revalidate against this Session before reserving/scheduling. They must freeze the
/// verified inputs/assets/plan and retain these same immutable kind/catalog definitions.
/// NodeKind callbacks are trusted pure definitions; hidden interior mutation is unsupported.
pub fn price(session: &crate::Session, request: &Value) -> Result<Priced, Error> {
    crate::session::guard(|| {
        crate::commands::validate_json(request)?;
        check_requested(request)?;
        let p: RunParams = params(request)?;
        priced_in(&session.context(crate::commands::Phase::Preview), &p)
    })
}

const MAX_BINDING_BYTES: usize = 256 * 1024;
fn binding(c: &Context<'_>, catalog: &RunCatalog, p: &RunParams, limit: usize) -> Result<String, Error> {
    let mode = mode_of(p)?;
    let uses_nodes = matches!(mode, RunMode::Selected | RunMode::WithUpstream);
    let requested: BTreeSet<_> = if uses_nodes { p.nodes.iter().copied().collect() } else { BTreeSet::new() };
    // Fixed fields, explicit mode/optional field and counted UUIDs. Check BEFORE hashing;
    // no project/plan serialization, recursive walk or large temporary JSON allocation.
    let bytes = b"nodecraft.run.quote.v3"
        .len()
        .checked_add(32 + 8 + 8 + 16 + 16 + 32 + 32 + 1 + 8 + if matches!(mode, RunMode::Field(_)) { 16 } else { 0 })
        .and_then(|n| requested.len().checked_mul(16).and_then(|ids| n.checked_add(ids)))
        .ok_or_else(|| invalid("run quote binding exceeds its byte budget"))?;
    if bytes > limit {
        return Err(invalid("run quote binding exceeds its byte budget"));
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nodecraft.run.quote.v3");
    c.execution.bind(&mut hash);
    hash.update(&c.revision.to_le_bytes());
    hash.update(c.document.id().as_bytes());
    hash.update(p.canvas.as_bytes());
    hash.update(&catalog.0.nonce);
    c.routing.bind(&mut hash);
    let (tag, field) = match mode {
        RunMode::Selected => (0, None),
        RunMode::WithUpstream => (1, None),
        RunMode::Field(id) => (2, Some(id)),
        RunMode::AllStale => (3, None),
    };
    hash.update(&[tag]);
    if let Some(field) = field {
        hash.update(field.as_bytes());
    }
    hash.update(&u64::try_from(requested.len()).map_err(|_| invalid("too many requested nodes"))?.to_le_bytes());
    for id in requested {
        hash.update(id.as_bytes());
    }
    Ok(hash.finalize().to_hex().to_string())
}

// Fixed UUID/static reason records only. Bound before each retained format, including
// separators when reused by run.start. Native hook strings never enter this report.
const BLOCKED_BYTES: usize = 16 * 1024;
fn blocked_reasons(blockers: &[nodecraft_exec::planner::Blocker]) -> Vec<String> {
    use nodecraft_exec::planner::{Reason, TargetReason};
    let mut bytes = 0usize;
    let mut out = Vec::new();
    for blocker in blockers.iter().take(if blockers.len() > 100 { 99 } else { 100 }) {
        let (code, text) = match blocker.reason {
            Reason::NoResult => ("input_unavailable", "input result is unavailable; run its upstream node first"),
            Reason::UnknownKind => ("unknown_kind", "node kind is not installed; install its definition"),
            Reason::UnsupportedInputs => ("unsupported_inputs", "input types are unsupported; check the node connections"),
            Reason::Invalid(_) => ("node_validation", "check node parameters, connections and installed kind"),
            Reason::NotAnOption => ("not_an_option", "a value is not on the list the host fetched; refresh the list or pick another"),
            Reason::ProviderTarget(TargetReason::MissingSelection) => ("endpoint_missing", "no provider endpoint is selected for this node"),
            Reason::ProviderTarget(TargetReason::Mismatch) => ("endpoint_mismatch", "work does not match the selected provider endpoint"),
        };
        let extent = 36 + code.len() + text.len() + 12;
        if bytes + extent > BLOCKED_BYTES {
            break;
        }
        bytes += extent;
        out.push(format!("node {} [{code}]: {text}", blocker.node));
    }
    if out.len() < blockers.len() {
        // Pre-reserved fixed trailer space is within the conservative per-row allowance.
        if bytes + 80 <= BLOCKED_BYTES {
            out.push(format!("[blockers_omitted]: {} additional blockers", blockers.len() - out.len()));
        }
    }
    out
}
fn admission_reason(error: &Error) -> &'static str {
    if error.kind == ErrorKind::Budget {
        "[admission_limit]: run exceeds admission resource limits; reduce the selection or request size"
    } else {
        "[catalog_mismatch]: select an installed catalog model and check its visible parameters"
    }
}

/// The canvas a run is on. A published technique version is frozen, so a run's results couldn't
/// be recorded there: it is refused before anything is quoted or charged.
fn run_canvas(document: &nodecraft_graph::Project, id: nodecraft_types::CanvasId) -> Result<&nodecraft_graph::Canvas, Error> {
    let canvas = document.canvases().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    if canvas.kind() == nodecraft_graph::CanvasKind::Technique {
        return Err(Error::new(ErrorKind::Unsupported, "a published technique version can't be run in place"));
    }
    Ok(canvas)
}

fn priced(c: &Context<'_>, p: &RunParams, token: String) -> Result<Priced, Error> {
    let document = c.document;
    let kinds = c.kinds;
    let catalog = c.resources.require::<RunCatalog>()?;
    let catalog = catalog.catalog();
    let budget = c.resources.require::<BudgetState>()?;
    let targets = c.routing.routing.targets();
    let resources = c.resources;
    let canvas = run_canvas(document, p.canvas)?;
    let mode = mode_of(p)?;
    let requested: BTreeSet<NodeId> = p.nodes.iter().copied().collect();
    // Only these modes look at staleness; a plain selection shouldn't pay to hash the canvas.
    let stale = if matches!(mode, RunMode::WithUpstream | RunMode::AllStale) { nodecraft_exec::staleness::stale(canvas, kinds)? } else { BTreeSet::new() };
    // Technique nodes run their versions' nodes, inlined for planning, quoting and admission.
    let expanded = nodecraft_exec::technique::expand(document.canvases(), canvas, kinds)?;
    // Planning consults the option lists the host has fetched, so a value off a known list is
    // refused here: before a quote, before admission, before an idempotency key exists.
    let run_plan = match plan_expanded(&expanded, kinds, &mode, &requested, &stale, targets, crate::option_cmds::lookup(resources))? {
        Planned::Ready(run_plan) => run_plan,
        Planned::Blocked(blockers) => {
            // Admission diagnostics never copy native hook messages, node titles or
            // unknown model values into a report. One hundred fixed UUID/code records.
            let reasons = blocked_reasons(&blockers);
            let response = json!({"token": token, "blocked": reasons, "steps": [], "totals": {}, "complete": false, "allowed": false, "blocks": [], "provisional": [], "estimated": [], "resumable": []});
            return Ok(Priced { response, totals: BTreeMap::new(), complete: false, allowed: false, token });
        }
    };
    let canvas = &*expanded.canvas;
    crate::generation_catalog::validate_plan(resources, kinds, canvas, run_plan.steps.iter().map(|s| s.node))?;
    let scratch_headroom = request_scratch(p)?;
    let priced = match nodecraft_exec::admission::quote_with_scratch(
        &run_plan,
        canvas,
        kinds,
        catalog,
        nodecraft_exec::admission::MAX_SCRATCH_BYTES.checked_sub(scratch_headroom).ok_or_else(|| invalid("run request metadata exceeds scratch limit"))?,
    ) {
        Ok(priced) => priced,
        Err(error) => {
            let reason = admission_reason(&error);
            return Ok(Priced {
                response: json!({"token":token,"blocked":[reason],"steps":[],"totals":{},"complete":false,"allowed":false,"blocks":[],"provisional":[],"estimated":[],"resumable":[]}),
                totals: BTreeMap::new(),
                complete: false,
                allowed: false,
                token,
            });
        }
    };
    let admission = check(&priced, &budget.budgets, &budget.spent)?;
    let totals: BTreeMap<String, u64> = priced.totals.iter().map(|(currency, m)| (String::from(*currency), m.micros)).collect();
    let mut totals_json = Map::new();
    for m in priced.totals.values() {
        totals_json.insert(String::from(m.currency), money(*m));
    }
    let owned =
        |nodes: &mut dyn Iterator<Item = NodeId>| nodes.map(|id| expanded.owner(id)).collect::<BTreeSet<_>>().into_iter().take(MAX_LISTED).collect::<Vec<_>>();
    let estimated = owned(&mut priced.estimated());
    let resumable = owned(&mut run_plan.resumable(canvas));
    let provisional = owned(&mut admission.provisional.iter().copied());
    let response = json!({
        "steps": priced.steps.iter().take(MAX_LISTED).map(|step| step_json(step, expanded.owner(step.node))).collect::<Vec<_>>(),
        "steps_total": priced.steps.len(),
        "totals": Value::Object(totals_json),
        "complete": priced.is_complete(),
        "allowed": admission.allowed(),
        "blocks": admission.blocks.iter().take(MAX_LISTED).map(|b| b.message()).collect::<Vec<_>>(),
        "provisional": provisional,
        "estimated": estimated,
        "resumable": resumable,
        "token": token,
    });
    Ok(Priced { response, totals, complete: priced.is_complete(), allowed: admission.allowed(), token })
}

fn priced_in(c: &Context<'_>, p: &RunParams) -> Result<Priced, Error> {
    let catalog = c.resources.require::<RunCatalog>()?;
    let token = binding(c, catalog, p, MAX_BINDING_BYTES)?;
    priced(c, p, token)
}

fn run_quote(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    if c.phase == crate::commands::Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    check_requested(value)?;
    let p: RunParams = params(value)?;
    Ok(Effect::None { result: priced_in(c, &p)?.response })
}

fn enabled_quote(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    check_requested(value)?;
    let p: RunParams = params(value)?;
    run_canvas(c.document, p.canvas)?;
    c.resources.require::<RunCatalog>()?;
    c.resources.require::<BudgetState>()?;
    mode_of(&p).map(|_| ())
}

// Fixed allowance covers binding tokens, bounded command envelopes, formatting and
// fixed typed shells; per-node entries cover parsed Vec plus simultaneous BTreeSet.
fn request_scratch(p: &RunParams) -> Result<usize, Error> {
    use nodecraft_exec::admission::measure::{add, mul};
    add(16 * 1024, mul(p.nodes.len(), std::mem::size_of::<NodeId>() + std::mem::size_of::<(NodeId, ())>() + 48)?)
}
fn start_metadata(value: &Value) -> Result<usize, Error> {
    use nodecraft_exec::admission::measure::{add, entries, mul};
    let nodes = value.get("nodes").and_then(Value::as_array).map_or(0, Vec::len);
    let accept = value.get("accept");
    let token = accept.and_then(|a| a.get("token")).and_then(Value::as_str).map_or(0, str::len);
    let amounts = accept.and_then(|a| a.get("amounts")).and_then(Value::as_object);
    let mut n = add(
        std::mem::size_of::<PreparedRun>()
            .checked_sub(std::mem::size_of::<nodecraft_exec::admission::AdmittedRunPlan>() + std::mem::size_of::<nodecraft_exec::cost::Quote>())
            .ok_or_else(|| invalid("run metadata representation overflow"))?
            + std::mem::size_of::<Box<PreparedRun>>(),
        add(mul(nodes, std::mem::size_of::<NodeId>())?, token)?,
    )?;
    if let Some(amounts) = amounts {
        n = add(n, entries::<String, u64>(amounts.len())?)?;
        for k in amounts.keys() {
            n = add(n, k.len())?;
        }
    }
    if n > nodecraft_exec::admission::MAX_SCRATCH_BYTES / 2 {
        return Err(invalid("run request metadata exceeds scratch limit"));
    }
    Ok(n)
}
#[cfg(test)]
thread_local! {static PREPARED_COPIES:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
/// Private owned execution authority; its preparation envelope is per object, not per queue.
pub struct PreparedRun {
    admitted: nodecraft_exec::admission::AdmittedRunPlan,
    quote: nodecraft_exec::cost::Quote,
    run: RunParams,
    accept: Accept,
    routing_nonce: Option<[u8; 32]>,
    bindings: Vec<(NodeId, crate::provider_routing::RegisteredProvider)>,
}
impl PreparedRun {
    pub fn quote(&self) -> &nodecraft_exec::cost::Quote {
        &self.quote
    }
    pub fn canvas_id(&self) -> CanvasId {
        self.run.canvas
    }
    /// The accepted amounts per currency: the most the run may reserve.
    pub fn ceiling(&self) -> Result<BTreeMap<nodecraft_types::Currency, u64>, Error> {
        accepted_limits(&self.accept)
    }
    /// The caller accepted a price that is partly unknown.
    pub fn partial(&self) -> bool {
        self.accept.partial
    }
    /// How many jobs of unknown price the run may admit.
    pub fn unpriced_jobs(&self) -> u64 {
        self.accept.unpriced_jobs.unwrap_or(0)
    }
}
/// The accepted amounts by currency: the run's ceiling (#366). A code that isn't a currency is
/// refused rather than ignored, so the caller never believes it accepted an amount it didn't.
fn accepted_limits(accept: &Accept) -> Result<BTreeMap<nodecraft_types::Currency, u64>, Error> {
    accept
        .amounts
        .iter()
        .map(|(code, micros)| {
            nodecraft_types::Currency::try_from(code.clone())
                .map(|currency| (currency, *micros))
                .map_err(|_| invalid("an accepted amount names an invalid currency"))
        })
        .collect()
}
fn accept_quote(c: &Context<'_>, quote: &nodecraft_exec::cost::Quote, accept: &Accept) -> Result<(), Error> {
    accepted_limits(accept)?;
    let budget = c.resources.require::<BudgetState>()?;
    if !check(quote, &budget.budgets, &budget.spent)?.allowed() {
        return Err(invalid("the run exceeds current budgets"));
    }
    if !quote.is_complete() && !accept.partial {
        return Err(invalid("part of the price is unknown; explicit partial consent is required"));
    }
    // Unknown-price steps need an explicit count of such jobs, at least what the quote shows
    // (a deferred step counts as one job here, as it does for its price).
    let nodes: BTreeSet<NodeId> = quote.unpriced().map(|(node, _)| node).collect();
    let unpriced = quote
        .steps
        .iter()
        .filter(|step| nodes.contains(&step.node))
        .try_fold(0u64, |n, step| n.checked_add(step.jobs))
        .ok_or_else(|| invalid("the quote's job count is out of range"))?;
    if unpriced > 0 && accept.unpriced_jobs.is_none_or(|accepted| accepted < unpriced) {
        return Err(invalid("part of the price is unknown; accept a count of unknown-price jobs (`unpriced_jobs`) covering the quote"));
    }
    for (currency, total) in &quote.totals {
        let agreed = accept.amounts.get(&String::from(*currency)).copied().unwrap_or(0);
        if total.micros > agreed {
            return Err(invalid("the run costs more than was accepted"));
        }
    }
    Ok(())
}
pub(crate) fn authorize(
    c: &Context<'_>,
    prepared: PreparedRun,
    options: nodecraft_exec::run::RunOptions,
) -> Result<(AuthorizedRun, nodecraft_exec::run::Progress), Error> {
    let catalog = c.resources.require::<RunCatalog>()?;
    if prepared.routing_nonce != c.routing.nonce || binding(c, catalog, &prepared.run, MAX_BINDING_BYTES)? != prepared.accept.token {
        return Err(invalid("the prepared run is stale; quote and prepare again"));
    }
    let canvas = c.document.canvases().get(&prepared.run.canvas).ok_or_else(|| invalid("canvas does not exist"))?;
    // Nodes inlined from a technique are only on the admitted copy; the rest are checked again
    // against the document.
    let admitted = &prepared.admitted;
    crate::generation_catalog::validate_plan(c.resources, c.kinds, canvas, admitted.planned_nodes().filter(|id| !admitted.is_inlined(*id)))?;
    crate::generation_catalog::validate_plan(c.resources, c.kinds, admitted.canvas(), admitted.planned_nodes().filter(|id| admitted.is_inlined(*id)))?;
    accept_quote(c, &prepared.quote, &prepared.accept)?;
    let ceiling = crate::run_ceiling::RunCeiling::new(accepted_limits(&prepared.accept)?, prepared.accept.unpriced_jobs.unwrap_or(0));
    let (run, progress) = nodecraft_exec::run::Run::start_admitted(prepared.admitted, c.kinds, options)?;
    // Measured usage is charged at a model's token rates only when the step runs that one model:
    // a combined price has no single set of rates to apply.
    let rates = prepared
        .quote
        .steps
        .iter()
        .filter_map(|step| match step.models.as_slice() {
            [model] => catalog.catalog().get(model).and_then(|m| m.usage.clone()).map(|rates| (step.node, rates)),
            _ => None,
        })
        .collect();
    Ok((AuthorizedRun { run, bindings: prepared.bindings, quote: prepared.quote, ceiling, rates }, progress))
}
fn run_start(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    if c.phase == crate::commands::Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    check_requested(value)?;
    // Parsed request is admitted temporary ownership before deserialization, and moves
    // into witness ownership only after its aggregate reservation succeeds below.
    let metadata = start_metadata(value)?;
    let (run, accept) = params::<StartParams>(value)?.split();
    let catalog = c.resources.require::<RunCatalog>()?;
    let token = binding(c, catalog, &run, MAX_BINDING_BYTES)?;
    if token != accept.token {
        return Err(invalid("the run changed since quotation; quote again"));
    }
    let canvas = run_canvas(c.document, run.canvas)?;
    let expanded = nodecraft_exec::technique::expand(c.document.canvases(), canvas, c.kinds)?;
    // Preflight ALL snapshot ownership before the single snapshot clone.
    nodecraft_exec::admission::measure::canvas(&expanded.canvas)?;
    let mode = mode_of(&run)?;
    let requested = run.nodes.iter().copied().collect();
    let stale = if matches!(mode, RunMode::WithUpstream | RunMode::AllStale) { nodecraft_exec::staleness::stale(canvas, c.kinds)? } else { BTreeSet::new() };
    let plan = match plan_expanded(&expanded, c.kinds, &mode, &requested, &stale, c.routing.routing.targets(), crate::option_cmds::lookup(c.resources))? {
        Planned::Ready(plan) => plan,
        Planned::Blocked(blockers) => return Err(invalid(blocked_reasons(&blockers).join("; "))),
    };
    crate::generation_catalog::validate_plan(c.resources, c.kinds, &expanded.canvas, plan.steps.iter().map(|s| s.node))?;
    #[cfg(test)]
    PREPARED_COPIES.with(|n| n.set(n.get() + 1));
    let mut limits = nodecraft_exec::admission::AdmissionLimits::default();
    let temporary = nodecraft_exec::admission::measure::add(metadata, request_scratch(&run)?)?;
    limits.scratch_bytes = limits.scratch_bytes.checked_sub(temporary).ok_or_else(|| invalid("run request metadata exceeds scratch limit"))?;
    let (mut admitted, quote) = nodecraft_exec::admission::admit_run_with_targets(
        plan,
        run.canvas,
        expanded.canvas.into_owned(),
        c.kinds,
        catalog.catalog(),
        limits,
        c.routing.routing.targets(),
    )
    .map_err(|error| Error::new(error.kind, admission_reason(&error)))?;
    // Parsed request ownership is transferred, never cloned. Reserve all extra metadata
    // before Box/PreparedRun retention; quote ownership was pre-admitted inside exec.
    admitted.reserve_metadata(metadata)?;
    let bindings = capture_bindings(&mut admitted, &c.routing.routing)?;
    accept_quote(c, &quote, &accept)?;
    Ok(Effect::PreparedRun { prepared: Box::new(PreparedRun { admitted, quote, run, accept, routing_nonce: c.routing.nonce, bindings }) })
}

fn enabled_start(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    check_requested(value)?;
    let (run, _) = params::<StartParams>(value)?.split();
    run_canvas(c.document, run.canvas)?;
    c.resources.require::<RunCatalog>()?;
    c.resources.require::<BudgetState>()?;
    if !c.services.contains_run_host() {
        return Err(Error::new(ErrorKind::Unsupported, "this host can't run jobs"));
    }
    mode_of(&run).map(|_| ())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordResult {
    node: NodeId,
    generation: Generation,
    /// Kept by hand, like a partial answer (#504): it doesn't replace the node's active result.
    #[serde(default)]
    keep: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordParams {
    canvas: CanvasId,
    results: Vec<RecordResult>,
}

fn run_record(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: RecordParams = params(value)?;
    if p.results.is_empty() || p.results.len() > MAX_RESULTS {
        return Err(invalid(format!("record 1 to {MAX_RESULTS} results")));
    }
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let mut seen = BTreeSet::new();
    let mut changes = Vec::new();
    for result in p.results {
        if !seen.insert(result.node) {
            return Err(invalid("a node is recorded twice"));
        }
        let node = canvas.nodes().get(&result.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "a recorded node is not on the canvas"))?;
        changes.extend(if result.keep { keep_changes(p.canvas, node, result.generation)? } else { record_changes(p.canvas, node, result.generation)? });
    }
    Ok(Effect::Edit { changes: Changeset::new(changes), result: json!({"recorded": seen.len()}), gesture: None })
}

fn enabled_record(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let p: RecordParams = params(value)?;
    c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist")).map(|_| ())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportParams {
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}

/// Encoded JSON page ceiling, including CSV escapes and the pagination envelope.
const MAX_EXPORT_JSON_BYTES: usize = 768 * 1024;

fn export_params(value: &Value) -> Result<ExportParams, Error> {
    let p: ExportParams = params(value)?;
    if p.limit == Some(0) {
        return Err(invalid("export limit must be at least 1"));
    }
    Ok(p)
}

/// JSON string content bytes (quotes belong to the envelope). UTF-8 stays literal in serde_json.
fn escaped_csv_bytes(bytes: &[u8]) -> Option<usize> {
    bytes.iter().try_fold(0_usize, |sum, byte| {
        let size = match byte {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 8 | 12 => 2,
            0..=31 => 6,
            _ => 1,
        };
        sum.checked_add(size)
    })
}

fn csv_page(usage: &UsageLog, p: &ExportParams, ceiling: usize) -> Result<Value, Error> {
    let total = usage.listed();
    let limit = p.limit.unwrap_or(MAX_EXPORT_ROWS).min(MAX_EXPORT_ROWS);
    // Reserve the longest possible continuation number, even on the terminal page.
    let envelope = serde_json::to_vec(&json!({"csv":"", "total":total, "next_offset":usize::MAX}))
        .map_err(|_| Error::new(ErrorKind::Internal, "the export envelope couldn't be encoded"))?
        .len();
    let mut header = Vec::new();
    usage.write_scoped_csv_rows(&mut header, usize::MAX, 0).map_err(|_| Error::new(ErrorKind::Internal, "the usage header couldn't be written"))?;
    let mut encoded = escaped_csv_bytes(&header)
        .and_then(|n| n.checked_add(envelope))
        .filter(|n| *n <= ceiling)
        .ok_or_else(|| invalid("export byte limit cannot fit the CSV header"))?;
    let mut out = Vec::new();
    out.try_reserve_exact(ceiling).map_err(|_| Error::new(ErrorKind::Internal, "the export page couldn't be allocated"))?;
    let written = usage
        .write_scoped_csv_rows_while(&mut out, p.offset, limit, |row| {
            let Some(next) = escaped_csv_bytes(row).and_then(|n| encoded.checked_add(n)).filter(|n| *n <= ceiling) else { return false };
            encoded = next;
            true
        })
        .map_err(|_| Error::new(ErrorKind::Internal, "the usage log couldn't be written"))?;
    if written == 0 && p.offset < total {
        return Err(invalid("export byte limit cannot fit a usage row"));
    }
    let csv = String::from_utf8(out).map_err(|_| Error::new(ErrorKind::Internal, "the usage log isn't UTF-8"))?;
    let next = p.offset.saturating_add(written);
    Ok(json!({"csv":csv,"total":total,"next_offset":(next < total).then_some(next)}))
}

fn export_csv(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p = export_params(value)?;
    let usage = c.resources.require::<UsageSnapshot>()?;
    Ok(Effect::None { result: csv_page(&usage.0, &p, MAX_EXPORT_JSON_BYTES)? })
}

fn enabled_export(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    export_params(value)?;
    c.resources.require::<UsageSnapshot>().map(|_| ())
}

fn run_schema_properties() -> Value {
    json!({
        "canvas": {"type": "string", "format": "uuid"},
        "mode": {"type": "string", "enum": ["selected", "with_upstream", "field", "all_stale"]},
        "nodes": {"type": "array", "maxItems": MAX_REQUESTED, "items": {"type": "string", "format": "uuid"}},
        "field": {"type": "string", "format": "uuid"}
    })
}

fn quote_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "mode"], "properties": run_schema_properties()})
}

fn start_schema() -> Value {
    let mut properties = run_schema_properties();
    if let Some(map) = properties.as_object_mut() {
        map.insert(
            "accept".into(),
            json!({"type": "object", "additionalProperties": false, "required": ["token"], "properties": {
                "token": {"type": "string"},
                "amounts": {"type": "object", "additionalProperties": {"type": "integer", "minimum": 0}},
                "partial": {"type": "boolean"},
                "unpriced_jobs": {"type": "integer", "minimum": 0}}}),
        );
    }
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "mode", "accept"], "properties": properties})
}

fn record_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "results"], "properties": {
            "canvas": {"type": "string", "format": "uuid"},
            "results": {"type": "array", "minItems": 1, "maxItems": MAX_RESULTS, "items": {"type": "object", "additionalProperties": false,
                "required": ["node", "generation"], "properties": {"node": {"type": "string", "format": "uuid"}, "generation": {"type": "object",
                    "required": ["id"], "properties": {"id": {"type": "string", "format": "uuid"}, "outputs": {"type": "array"}, "input_hash": {"type": "string"}}},
                    "keep": {"type": "boolean"}}}}}})
}

fn export_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [], "properties": {
        "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": MAX_EXPORT_ROWS}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "run.quote",
            label: "Price a run",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: quote_schema,
            params_doc: "{canvas, mode: selected|with_upstream|field|all_stale, nodes?: [NodeId], field?: FieldId} -> {steps: [{node, jobs, models, price, unpriced}] (first 500), steps_total, totals: {CUR: {currency, micros, text}}, complete, allowed, blocks, provisional, estimated, resumable, blocked?, token}. `nodes` is used by selected and with_upstream. `resumable` lists batch nodes priced for every item that may run only the items their partial result lacks. Read-only.",
            enabled: enabled_quote,
            run: run_quote,
        },
        CommandSpec {
            id: "run.start",
            label: "Run",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: start_schema,
            params_doc: "{canvas, mode, nodes?, field?, accept: {token, amounts: {CUR: micros}, partial?: bool, unpriced_jobs?: count}}; starts the run only if it is still exactly what run.quote priced (`token`), the budgets allow it, the accepted amounts cover every currency's total, `partial` is set when part of the price isn't known, and `unpriced_jobs` covers the quote's jobs of unknown price. While it runs, the amounts are a ceiling on what it may reserve per currency and `unpriced_jobs` on how many unknown-price jobs it may admit, including deferred fan-out and retries under new keys.",
            enabled: enabled_start,
            run: run_start,
        },
        CommandSpec {
            id: "run.record",
            label: "Record run results",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: record_schema,
            params_doc: "{canvas, results: [{node: NodeId, generation: graph::Generation, keep?: bool}]}; records each result as the node's new active generation, all in one undo step. A `keep` result (a partial answer kept by hand) joins the history without replacing the active generation, and becomes it only when there is none. For the host that ran the jobs.",
            enabled: enabled_record,
            run: run_record,
        },
        CommandSpec {
            id: "usage.export_csv",
            label: "Export usage as CSV",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: export_schema,
            params_doc: "{offset?, limit? (≤ 2000)} -> {csv, total, next_offset}; one page of the usage log as CSV (formula cells neutralised). Read-only.",
            enabled: enabled_export,
            run: export_csv,
        },
    ]
}

#[cfg(test)]
mod tests;

/// Authorized work and only the bindings selected by its immutable admission witness.
pub struct AuthorizedRun {
    quote: nodecraft_exec::cost::Quote,
    /// What the caller accepted, as a cumulative ceiling on the run's priced reservations.
    ceiling: crate::run_ceiling::RunCeiling,
    run: nodecraft_exec::run::Run,
    bindings: Vec<(NodeId, crate::provider_routing::RegisteredProvider)>,
    /// Per-token rates of each step that runs one model with them (#595).
    rates: BTreeMap<NodeId, nodecraft_catalog::TokenRates>,
}
impl std::ops::Deref for AuthorizedRun {
    type Target = nodecraft_exec::run::Run;
    fn deref(&self) -> &Self::Target {
        &self.run
    }
}
impl AuthorizedRun {
    pub fn binding(&self, job: nodecraft_exec::scheduler::JobId) -> Result<&crate::provider_routing::RegisteredProvider, Error> {
        let Some(nodecraft_nodes::JobSpec::Generate { node, provider, instance, .. }) = self.run.work(job) else {
            return Err(invalid("job has no provider binding"));
        };
        self.bindings
            .iter()
            .find(|(n, b)| n == node && b.target().adapter() == provider && b.target().instance() == *instance)
            .map(|(_, b)| b)
            .ok_or_else(|| invalid("job has no provider binding"))
    }
    /// Convert admitted work using its captured binding, without another registry lookup. Work
    /// with media inputs is refused; see [`Self::queued_job_with_media`].
    pub fn queued_job(&self, id: nodecraft_exec::scheduler::JobId, key: &str) -> Result<crate::jobs::QueuedJob, Error> {
        self.queued(id, key, None)
    }
    /// [`Self::queued_job`], reading the job's media inputs from `media` (#502), normally the
    /// session's asset store.
    pub fn queued_job_with_media(
        &self,
        id: nodecraft_exec::scheduler::JobId,
        key: &str,
        media: &dyn crate::generate::MediaSource,
    ) -> Result<crate::jobs::QueuedJob, Error> {
        self.queued(id, key, Some(media))
    }
    fn queued(
        &self,
        id: nodecraft_exec::scheduler::JobId,
        key: &str,
        media: Option<&dyn crate::generate::MediaSource>,
    ) -> Result<crate::jobs::QueuedJob, Error> {
        let binding = self.binding(id)?;
        let work = self.run.work(id).ok_or_else(|| invalid("job is unavailable"))?;
        let nodecraft_nodes::JobSpec::Generate { node, .. } = work else { return Err(invalid("job is not generated work")) };
        let step = self.quote.steps.iter().find(|step| step.node == *node).ok_or_else(|| invalid("job has no admitted price"))?;
        let price = match &step.per_job {
            Ok(None) => None, // Explicitly free catalog selections, not absent host metadata: free work is
            // neither debited nor counted (it isn't unknown-price).
            amount => {
                let length = step
                    .models
                    .iter()
                    .try_fold(0usize, |n, model| n.checked_add(model.len()))
                    .and_then(|n| n.checked_add(step.models.len().saturating_sub(1)))
                    .filter(|n| *n <= nodecraft_exec::cost::MAX_TEXT)
                    .ok_or_else(|| invalid("admitted price model label exceeds accounting bounds"))?;
                let mut model = String::new();
                model.try_reserve_exact(length).map_err(|_| invalid("admitted price label could not be allocated"))?;
                for name in &step.models {
                    if !model.is_empty() {
                        model.push('+');
                    }
                    model.push_str(name);
                }
                Some(crate::jobs::JobPrice {
                    node: *node,
                    model,
                    estimate: amount.as_ref().ok().copied().flatten(),
                    ceiling: Some(self.ceiling.clone()),
                    rates: self.rates.get(node).cloned(),
                })
            }
        };
        let (_, job) = match media {
            Some(media) => crate::generate::provider_job_with_media(work, key, media)?,
            None => crate::generate::provider_job(work, key)?,
        };
        Ok(crate::jobs::QueuedJob { provider: binding.clone(), job, bound: Some(price.clone()), price, resume_only: false })
    }
    /// The run's accepted amount and what it has reserved against it, in micros.
    pub fn ceiling(&self, currency: nodecraft_types::Currency) -> (u64, u64) {
        self.ceiling.status(currency)
    }
    /// The accepted count of unknown-price jobs and how many the run has admitted.
    pub fn unpriced_jobs(&self) -> (u64, u64) {
        self.ceiling.unpriced_status()
    }
    pub fn job_done(
        &mut self,
        kinds: &nodecraft_nodes::Registry,
        job: nodecraft_exec::scheduler::JobId,
        outputs: Vec<(String, nodecraft_types::Value)>,
    ) -> nodecraft_exec::run::Progress {
        self.run.job_done(kinds, job, outputs)
    }
    /// The units `job`'s provider said it billed (#437), recorded with its settings.
    pub fn job_billed(&mut self, job: nodecraft_exec::scheduler::JobId, units: nodecraft_types::BillableUnits) {
        self.run.job_billed(job, units)
    }
    /// `job` may have been billed under `key` although it didn't finish (#517); call before
    /// [`Self::job_failed`].
    pub fn job_may_be_billed(&mut self, job: nodecraft_exec::scheduler::JobId, key: &str) {
        self.run.job_may_be_billed(job, key)
    }
    pub fn job_failed(&mut self, kinds: &nodecraft_nodes::Registry, job: nodecraft_exec::scheduler::JobId, reason: &str) -> nodecraft_exec::run::Progress {
        self.run.job_failed(kinds, job, reason)
    }
    pub fn job_cancelled(&mut self, kinds: &nodecraft_nodes::Registry, job: nodecraft_exec::scheduler::JobId, reason: &str) -> nodecraft_exec::run::Progress {
        self.run.job_cancelled(kinds, job, reason)
    }
    /// What recording `job`'s result needs once this run is gone (#508); see
    /// [`nodecraft_exec::run::ResumeRecipe`].
    pub fn resume_recipe(&mut self, kinds: &nodecraft_nodes::Registry, job: nodecraft_exec::scheduler::JobId) -> Option<nodecraft_exec::run::ResumeRecipe> {
        self.run.resume_recipe(kinds, job)
    }
    /// The canvas node a step's jobs belong to: its technique node when it is inlined (#94).
    pub fn owner(&self, node: nodecraft_types::NodeId) -> nodecraft_types::NodeId {
        self.run.owner(node)
    }
}

#[cfg(test)]
thread_local! {static BINDING_COPIES:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
fn capture_bindings(
    admitted: &mut nodecraft_exec::admission::AdmittedRunPlan,
    routing: &crate::provider_routing::ProviderRouting,
) -> Result<Vec<(NodeId, crate::provider_routing::RegisteredProvider)>, Error> {
    // Resolve and meter every retained independent binding before the first Arc clone.
    let mut binding_bytes = 0usize;
    for (_, adapter, instance) in admitted.selected_targets() {
        let binding = routing.find_parts(adapter, instance)?;
        binding_bytes =
            binding_bytes.checked_add(std::mem::size_of::<NodeId>() + binding.retained_bytes()).ok_or_else(|| invalid("run binding ownership overflow"))?;
    }
    admitted.reserve_metadata(binding_bytes)?;
    let mut bindings = Vec::new();
    bindings.try_reserve_exact(admitted.selected_targets().count()).map_err(|_| invalid("run bindings cannot be allocated"))?;
    for (node, adapter, instance) in admitted.selected_targets() {
        #[cfg(test)]
        BINDING_COPIES.with(|n| n.set(n.get() + 1));
        bindings.push((node, routing.find_parts(adapter, instance)?.clone()));
    }
    Ok(bindings)
}

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
//!   to the host's `run.start` service, which plans the same request and runs it.
//! - `run.record` (host only): records a run's results, one generation per node, as one undo
//!   step.
//! - `usage.export_csv` (read-only): the usage log as spreadsheet-safe CSV.
//!
//! The model catalog, the caps with what has been spent, and the usage log are host resources
//! ([`RunCatalog`], [`BudgetState`], [`UsageSnapshot`]); without them the commands are
//! unsupported.

use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_catalog::Catalog;
use nodecraft_exec::cost::{Budgets, Spent, StepQuote, Unpriced, UsageLog, check, format_money, quote};
use nodecraft_exec::planner::{Planned, RunMode, plan};
use nodecraft_exec::run::record_changes;
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
    pub fn catalog(&self) -> &Catalog {
        &self.0.catalog
    }
}

/// The caps, and what has been spent so far, as of now (a host resource).
pub struct BudgetState {
    pub budgets: Budgets,
    pub spent: Spent,
}

/// The usage log, for export (a host resource).
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
        Unpriced::UnknownModel(id) => format!("the model {id} isn't in the catalog"),
        Unpriced::ByUsage(id) => format!("{id} is priced by usage, measured during the run"),
        Unpriced::Invalid(message) => message.clone(),
    }
}

fn step_json(step: &StepQuote) -> Value {
    let (price, reason) = match step.total() {
        Ok(Some(total)) => (money(total), Value::Null),
        Ok(None) => (Value::Null, Value::Null),
        Err(why) => (Value::Null, json!(unpriced_reason(&why))),
    };
    json!({"node": step.node, "jobs": step.jobs_known.then_some(step.jobs), "models": step.models, "price": price, "unpriced": reason})
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
    let bytes = b"nodecraft.run.quote.v2"
        .len()
        .checked_add(32 + 8 + 8 + 16 + 16 + 32 + 1 + 8 + if matches!(mode, RunMode::Field(_)) { 16 } else { 0 })
        .and_then(|n| requested.len().checked_mul(16).and_then(|ids| n.checked_add(ids)))
        .ok_or_else(|| invalid("run quote binding exceeds its byte budget"))?;
    if bytes > limit {
        return Err(invalid("run quote binding exceeds its byte budget"));
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nodecraft.run.quote.v2");
    c.execution.bind(&mut hash);
    hash.update(&c.revision.to_le_bytes());
    hash.update(c.document.id().as_bytes());
    hash.update(p.canvas.as_bytes());
    hash.update(&catalog.0.nonce);
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

fn priced(
    document: &nodecraft_graph::Project,
    kinds: &nodecraft_nodes::Registry,
    catalog: &Catalog,
    budget: &BudgetState,
    p: &RunParams,
    token: String,
) -> Result<Priced, Error> {
    let canvas = document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let mode = mode_of(p)?;
    let requested: BTreeSet<NodeId> = p.nodes.iter().copied().collect();
    // Only these modes look at staleness; a plain selection shouldn't pay to hash the canvas.
    let stale = if matches!(mode, RunMode::WithUpstream | RunMode::AllStale) { nodecraft_exec::staleness::stale(canvas, kinds)? } else { BTreeSet::new() };
    let run_plan = match plan(canvas, kinds, &mode, &requested, &stale)? {
        Planned::Ready(run_plan) => run_plan,
        Planned::Blocked(blockers) => {
            let reasons: Vec<String> = blockers.iter().take(MAX_LISTED).map(|b| b.message(canvas)).collect();
            let response =
                json!({"blocked": reasons, "steps": [], "totals": {}, "complete": false, "allowed": false, "blocks": [], "provisional": [], "estimated": []});
            return Ok(Priced { response, totals: BTreeMap::new(), complete: false, allowed: false, token });
        }
    };
    let priced = quote(&run_plan, canvas, kinds, catalog)?;
    let admission = check(&priced, &budget.budgets, &budget.spent)?;
    let totals: BTreeMap<String, u64> = priced.totals.iter().map(|(currency, m)| (String::from(*currency), m.micros)).collect();
    let mut totals_json = Map::new();
    for m in priced.totals.values() {
        totals_json.insert(String::from(m.currency), money(*m));
    }
    let estimated: Vec<NodeId> = priced.estimated().collect();
    let response = json!({
        "steps": priced.steps.iter().take(MAX_LISTED).map(step_json).collect::<Vec<_>>(),
        "steps_total": priced.steps.len(),
        "totals": Value::Object(totals_json),
        "complete": priced.is_complete(),
        "allowed": admission.allowed(),
        "blocks": admission.blocks.iter().take(MAX_LISTED).map(|b| b.message()).collect::<Vec<_>>(),
        "provisional": admission.provisional.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "estimated": estimated.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "token": token,
    });
    Ok(Priced { response, totals, complete: priced.is_complete(), allowed: admission.allowed(), token })
}

fn priced_in(c: &Context<'_>, p: &RunParams) -> Result<Priced, Error> {
    let catalog = c.resources.require::<RunCatalog>()?;
    let budget = c.resources.require::<BudgetState>()?;
    let token = binding(c, catalog, p, MAX_BINDING_BYTES)?;
    priced(c.document, c.kinds, catalog.catalog(), budget, p, token)
}

fn run_quote(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    check_requested(value)?;
    let p: RunParams = params(value)?;
    Ok(Effect::None { result: priced_in(c, &p)?.response })
}

fn enabled_quote(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    check_requested(value)?;
    let p: RunParams = params(value)?;
    c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    c.resources.require::<RunCatalog>()?;
    c.resources.require::<BudgetState>()?;
    mode_of(&p).map(|_| ())
}

fn run_start(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    check_requested(value)?;
    let (run, accept) = params::<StartParams>(value)?.split();
    let priced = priced_in(c, &run)?;
    if !priced.allowed {
        let reasons = priced
            .response
            .get("blocked")
            .filter(|b| b.as_array().is_some_and(|a| !a.is_empty()))
            .or_else(|| priced.response.get("blocks"))
            .cloned()
            .unwrap_or(Value::Null);
        return Err(invalid(format!("the run can't start: {reasons}")));
    }
    if accept.token != priced.token {
        return Err(invalid("the run changed since it was quoted (or wasn't quoted); quote it again with run.quote"));
    }
    if !priced.complete && !accept.partial {
        return Err(invalid("part of the price isn't known (unpriced steps, or job counts known only at run time); accept a partial price to run anyway"));
    }
    for (currency, total) in &priced.totals {
        let agreed = accept.amounts.get(currency).copied().unwrap_or(0);
        if *total > agreed {
            return Err(invalid(format!("the run costs more than was accepted ({total} > {agreed} micro-{currency}); quote again")));
        }
    }
    let mode = mode_of(&run)?;
    let nodes: BTreeSet<NodeId> = if matches!(mode, RunMode::Selected | RunMode::WithUpstream) { run.nodes.iter().copied().collect() } else { BTreeSet::new() };
    let (mode, field) = match mode {
        RunMode::Selected => ("selected", None),
        RunMode::WithUpstream => ("with_upstream", None),
        RunMode::Field(field) => ("field", Some(field)),
        RunMode::AllStale => ("all_stale", None),
    };
    let request = json!({
        "run": {"canvas": run.canvas, "mode": mode, "nodes": nodes, "field": field},
        "revision": c.revision,
        "token": priced.token,
        "accept": {"amounts": accept.amounts, "partial": accept.partial},
    });
    Ok(Effect::Service { capability: RUN_SERVICE, request })
}

fn enabled_start(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    check_requested(value)?;
    let (run, _) = params::<StartParams>(value)?.split();
    c.document.canvases().get(&run.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    c.resources.require::<RunCatalog>()?;
    c.resources.require::<BudgetState>()?;
    if !c.services.contains(RUN_SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "this host can't run jobs"));
    }
    mode_of(&run).map(|_| ())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordResult {
    node: NodeId,
    generation: Generation,
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
        changes.extend(record_changes(p.canvas, node, result.generation)?);
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

fn export_csv(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: ExportParams = params(value)?;
    let usage = c.resources.require::<UsageSnapshot>()?;
    let limit = p.limit.unwrap_or(MAX_EXPORT_ROWS).min(MAX_EXPORT_ROWS);
    let mut out = Vec::new();
    usage.0.write_csv_rows(&mut out, p.offset, limit).map_err(|_| Error::new(ErrorKind::Internal, "the usage log couldn't be written"))?;
    let csv = String::from_utf8(out).map_err(|_| Error::new(ErrorKind::Internal, "the usage log isn't UTF-8"))?;
    let total = usage.0.listed();
    let next = p.offset.saturating_add(limit);
    Ok(Effect::None { result: json!({"csv": csv, "total": total, "next_offset": (next < total).then_some(next)}) })
}

fn enabled_export(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: ExportParams = params(value)?;
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
                "partial": {"type": "boolean"}}}),
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
                    "required": ["id"], "properties": {"id": {"type": "string", "format": "uuid"}, "outputs": {"type": "array"}, "input_hash": {"type": "string"}}}}}}}})
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
            params_doc: "{canvas, mode: selected|with_upstream|field|all_stale, nodes?: [NodeId], field?: FieldId} -> {steps: [{node, jobs, models, price, unpriced}] (first 500), steps_total, totals: {CUR: {currency, micros, text}}, complete, allowed, blocks, provisional, estimated, blocked?, token}. `nodes` is used by selected and with_upstream. Read-only.",
            enabled: enabled_quote,
            run: run_quote,
        },
        CommandSpec {
            id: "run.start",
            label: "Run",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: start_schema,
            params_doc: "{canvas, mode, nodes?, field?, accept: {token, amounts: {CUR: micros}, partial?: bool}}; starts the run only if it is still exactly what run.quote priced (`token`), the budgets allow it, the accepted amounts cover every currency's total, and `partial` is set when part of the price isn't known.",
            enabled: enabled_start,
            run: run_start,
        },
        CommandSpec {
            id: "run.record",
            label: "Record run results",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: record_schema,
            params_doc: "{canvas, results: [{node: NodeId, generation: graph::Generation}]}; records each result as the node's new active generation, all in one undo step. For the host that ran the jobs.",
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

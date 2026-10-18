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
use nodecraft_graph::{Changeset, Generation, MAX_CHANGES};
use nodecraft_types::{CanvasId, Error, ErrorKind, FieldId, Money, NodeId};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The host service that runs a started run.
pub const RUN_SERVICE: &str = "run.start";
/// Most nodes one run request may name.
pub const MAX_REQUESTED: usize = 10_000;
/// Most results one `run.record` may carry (each is up to three changes).
pub const MAX_RESULTS: usize = MAX_CHANGES / 3;

/// The model catalog runs are priced against (a host resource).
pub struct RunCatalog(pub Arc<Catalog>);

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
    /// The most the caller agreed to, per currency code, in micro-units.
    #[serde(default)]
    amounts: BTreeMap<String, u64>,
    /// The caller agreed although some step has no price (it could cost anything).
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

/// What a quote found: the JSON response, the totals, whether every step is priced, and whether
/// the budgets allow the run.
struct Priced {
    response: Value,
    totals: BTreeMap<String, u64>,
    complete: bool,
    allowed: bool,
}

fn priced(c: &Context<'_>, p: &RunParams) -> Result<Priced, Error> {
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let catalog = c.resources.require::<RunCatalog>()?;
    let budget = c.resources.require::<BudgetState>()?;
    let mode = mode_of(p)?;
    let requested: BTreeSet<NodeId> = p.nodes.iter().copied().collect();
    let stale = nodecraft_exec::staleness::stale(canvas, c.kinds)?;
    let run_plan = match plan(canvas, c.kinds, &mode, &requested, &stale)? {
        Planned::Ready(run_plan) => run_plan,
        Planned::Blocked(blockers) => {
            let reasons: Vec<String> = blockers.iter().map(|b| b.message(canvas)).collect();
            return Ok(Priced { response: json!({"blocked": reasons, "allowed": false}), totals: BTreeMap::new(), complete: false, allowed: false });
        }
    };
    let priced = quote(&run_plan, canvas, c.kinds, &catalog.0)?;
    let admission = check(&priced, &budget.budgets, &budget.spent)?;
    let totals: BTreeMap<String, u64> = priced.totals.iter().map(|(currency, m)| (String::from(*currency), m.micros)).collect();
    let mut totals_json = Map::new();
    for m in priced.totals.values() {
        totals_json.insert(String::from(m.currency), money(*m));
    }
    let complete = priced.unpriced().next().is_none();
    let response = json!({
        "steps": priced.steps.iter().map(step_json).collect::<Vec<_>>(),
        "totals": Value::Object(totals_json),
        "complete": complete,
        "allowed": admission.allowed(),
        "blocks": admission.blocks.iter().map(|b| b.message()).collect::<Vec<_>>(),
        "provisional": admission.provisional,
    });
    Ok(Priced { response, totals, complete, allowed: admission.allowed() })
}

fn run_quote(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: RunParams = params(value)?;
    Ok(Effect::None { result: priced(c, &p)?.response })
}

fn enabled_quote(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let p: RunParams = params(value)?;
    c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    c.resources.require::<RunCatalog>()?;
    c.resources.require::<BudgetState>()?;
    mode_of(&p).map(|_| ())
}

fn run_start(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let (run, accept) = params::<StartParams>(value)?.split();
    let priced = priced(c, &run)?;
    if !priced.allowed {
        let reasons = priced.response.get("blocked").or_else(|| priced.response.get("blocks")).cloned().unwrap_or(Value::Null);
        return Err(invalid(format!("the run can't start: {reasons}")));
    }
    if !priced.complete && !accept.partial {
        return Err(invalid("some steps have no price; quote again and accept a partial price to run anyway"));
    }
    for (currency, total) in &priced.totals {
        let agreed = accept.amounts.get(currency).copied().unwrap_or(0);
        if *total > agreed {
            return Err(invalid(format!("the run costs more than was accepted ({total} > {agreed} micro-{currency}); quote again")));
        }
    }
    let request = json!({"canvas": run.canvas, "mode": value.get("mode"), "nodes": run.nodes, "field": run.field, "quote": priced.response});
    Ok(Effect::Service { capability: RUN_SERVICE, request })
}

fn enabled_start(c: &Context<'_>, value: &Value) -> Result<(), Error> {
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
struct NoParams {}

fn export_csv(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let _: NoParams = params(value)?;
    let usage = c.resources.require::<UsageSnapshot>()?;
    Ok(Effect::None { result: json!({"csv": usage.0.to_csv()}) })
}

fn enabled_export(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: NoParams = params(value)?;
    c.resources.require::<UsageSnapshot>().map(|_| ())
}

fn run_schema_properties() -> Value {
    json!({
        "canvas": {"type": "string", "format": "uuid"},
        "mode": {"enum": ["selected", "with_upstream", "field", "all_stale"]},
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
            json!({"type": "object", "additionalProperties": false, "properties": {
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
                "required": ["node", "generation"], "properties": {"node": {"type": "string", "format": "uuid"}, "generation": {"type": "object"}}}}}})
}

fn empty_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [], "properties": {}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "run.quote",
            label: "Price a run",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: quote_schema,
            params_doc: "{canvas, mode: selected|with_upstream|field|all_stale, nodes?: [NodeId], field?: FieldId} -> {steps: [{node, jobs, models, price, unpriced}], totals: {CUR: {currency, micros, text}}, complete, allowed, blocks: [text], provisional: [NodeId]} or {blocked: [text], allowed: false}. Read-only.",
            enabled: enabled_quote,
            run: run_quote,
        },
        CommandSpec {
            id: "run.start",
            label: "Run",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: start_schema,
            params_doc: "{canvas, mode, nodes?, field?, accept: {amounts: {CUR: micros}, partial?: bool}}; quotes again and starts the run only when the budgets allow it, the accepted amounts cover every currency's total, and `partial` is set if some step has no price. Quote first with run.quote.",
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
            input_schema: empty_schema,
            params_doc: "{} -> {csv: String}; the usage log as CSV (formula cells neutralised). Read-only.",
            enabled: enabled_export,
            run: export_csv,
        },
    ]
}

#[cfg(test)]
mod tests;

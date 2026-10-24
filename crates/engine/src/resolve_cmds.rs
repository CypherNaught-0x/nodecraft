//! Settling or releasing reconciled legacy charges on a person's decision (#332).
//!
//! - `usage.unsettled` (read-only): pages of the reconciled charges still open, each with the
//!   token a decision must carry. At most the MCP page budget of encoded JSON per page.
//! - `usage.resolve_charge` (human only): states what the provider's own records show for one
//!   such charge, `settled` at the billed amount or `released` (never billed), bound to the
//!   token the person reviewed. The host's [`RESOLUTION_SERVICE`] applies it to the live usage
//!   log, which checks the token again. `dry_run: true` returns the record and the totals it
//!   would produce, changing nothing. Agents may list the charges but can't decide them.
//!
//! Host contract: as for `usage.reconcile_ownership` (see [`crate::ownership_cmds`]): save the
//! log and replace the [`UsageSnapshot`] after the service succeeds. The desktop's run host
//! does both ([`crate::run_host::RunHost::register_ownership`]), and refuses a charge a saved
//! run still follows.

use crate::commands::{CommandSpec, Context, Effect, params};
use crate::ownership_cmds::{MAX_PAGE_JSON_BYTES, fill, hex, invalid, money, status, unhex};
use crate::run_cmds::UsageSnapshot;
use nodecraft_exec::cost::{ChargeResolution, Holder, QuarantineLocation, Resolution, ResolutionDecision, UsageLog};
use nodecraft_types::{Currency, Error, ErrorKind, Money};
use serde::Deserialize;
use serde_json::{Value, json};

/// The host service that applies a confirmed resolution to the live usage log.
pub const RESOLUTION_SERVICE: &str = "usage.resolve_charge";
/// Most charges one `usage.unsettled` page lists.
pub const MAX_UNSETTLED_PER_PAGE: usize = 500;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageParams {
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Outcome {
    Settled,
    Released,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionParams {
    provider: String,
    key: String,
    token: String,
    outcome: Outcome,
    #[serde(default)]
    currency: Option<Currency>,
    #[serde(default)]
    micros: Option<u64>,
    #[serde(default)]
    dry_run: bool,
}

fn decision_and_mode(value: &Value) -> Result<(ResolutionDecision, bool), Error> {
    let p = params::<DecisionParams>(value)?;
    let token = unhex(&p.token).ok_or_else(|| invalid("the token must be the 64 hex digits usage.unsettled returned"))?;
    let resolution = match (p.outcome, p.currency, p.micros) {
        (Outcome::Settled, Some(currency), Some(micros)) => Resolution::Settled { cost: Money::new(currency, micros) },
        (Outcome::Settled, _, _) => return Err(invalid("a settled charge needs the billed `currency` and `micros`")),
        (Outcome::Released, None, None) => Resolution::Released,
        (Outcome::Released, _, _) => return Err(invalid("a released charge has no amount")),
    };
    Ok((ResolutionDecision { provider: p.provider, idempotency_key: p.key, token, resolution }, p.dry_run))
}

/// Applies the decision in a service `request` to `log`, recorded at `at_ms`, for a host that
/// saves the result before keeping it.
pub fn resolve(log: &mut UsageLog, request: &Value, at_ms: u64) -> Result<Value, Error> {
    let (decision, dry_run) = decision_and_mode(request)?;
    if dry_run {
        return Err(invalid("a dry run is answered by the command, not the host service"));
    }
    let record = log.resolve_charge(&decision, at_ms)?;
    Ok(record_json(&record))
}

/// The `(provider, key)` a service request names, for a host that checks it isn't still in use.
pub fn requested_charge(request: &Value) -> Result<(String, String), Error> {
    decision_and_mode(request).map(|(d, _)| (d.provider, d.idempotency_key))
}

fn record_json(record: &ChargeResolution) -> Value {
    let (outcome, cost) = match record.resolution {
        Resolution::Settled { cost } => ("settled", money(cost)),
        Resolution::Released => ("released", Value::Null),
    };
    json!({"provider": record.provider, "key": record.idempotency_key, "session": record.session, "was": money(record.was),
        "was_status": status(record.was_status), "outcome": outcome, "cost": cost, "at_ms": record.at_ms})
}

fn total(totals: Option<&std::collections::BTreeMap<Currency, Money>>, currency: Currency) -> Money {
    totals.and_then(|t| t.get(&currency)).copied().unwrap_or(Money::zero(currency))
}

fn page(usage: &UsageLog, p: &PageParams) -> Result<Value, Error> {
    let mut budget = MAX_PAGE_JSON_BYTES.saturating_sub(16 * 1024);
    let charges = usage.resolvable_charges();
    let limit = p.limit.unwrap_or(MAX_UNSETTLED_PER_PAGE).min(MAX_UNSETTLED_PER_PAGE);
    let rows = charges.iter().skip(p.offset).map(|c| {
        let e = c.entry;
        let location = if c.location == QuarantineLocation::Pending { "pending" } else { "listed" };
        let holder = if c.holder == Holder::Named { "named" } else { "earlier" };
        json!({"provider": e.provider, "key": e.idempotency_key, "session": e.session, "model": e.model, "node": e.node, "at_ms": e.at_ms,
            "amount": money(e.cost), "status": status(e.status), "location": location, "holder": holder,
            "reconciled_at_ms": c.reconciled_at_ms, "token": hex(&c.token)})
    });
    let mut out = Vec::new();
    let next = fill(&mut out, rows, p.offset, charges.len(), limit, &mut budget)?;
    Ok(json!({"charges": out, "total": charges.len(), "next_offset": next, "resolved": usage.charge_resolutions().len()}))
}

fn page_params(value: &Value) -> Result<PageParams, Error> {
    let p = params::<PageParams>(value)?;
    if p.limit == Some(0) {
        return Err(invalid("limit must be at least 1"));
    }
    Ok(p)
}

fn run_page(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p = page_params(value)?;
    let usage = c.resources.require::<UsageSnapshot>()?;
    Ok(Effect::None { result: page(&usage.0, &p)? })
}

fn enabled_page(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    page_params(value)?;
    c.resources.require::<UsageSnapshot>().map(|_| ())
}

fn run_resolve(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let (decision, dry_run) = decision_and_mode(value)?;
    let usage = c.resources.require::<UsageSnapshot>()?;
    if !dry_run {
        // Refused here when stale or impossible; the service checks again on the live log.
        usage.0.plan_charge_resolution(&decision, 0)?;
        return Ok(Effect::Service { capability: RESOLUTION_SERVICE, request: value.clone() });
    }
    // The exact outcome, on a copy of the snapshot.
    let mut copy = UsageLog::clone(&usage.0);
    let record = copy.resolve_charge(&decision, 0)?;
    let currency = record.was.currency;
    let spent = copy.reported_spent(&record.session);
    let mut result = record_json(&record);
    if let Some(map) = result.as_object_mut() {
        map.remove("at_ms");
        map.insert("session_total".into(), money(total(Some(&spent.session), currency)));
        map.insert("project_total".into(), money(total(Some(&spent.project), currency)));
        map.insert("dry_run".into(), json!(true));
    }
    Ok(Effect::None { result })
}

fn enabled_resolve(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let (_, dry_run) = decision_and_mode(value)?;
    c.resources.require::<UsageSnapshot>()?;
    if !dry_run && !c.services.contains(RESOLUTION_SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "this host can't change its usage log"));
    }
    Ok(())
}

fn page_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [], "properties": {
        "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": MAX_UNSETTLED_PER_PAGE}}})
}

fn resolve_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["provider", "key", "token", "outcome"], "properties": {
            "provider": {"type": "string"},
            "key": {"type": "string"},
            "token": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
            "outcome": {"type": "string", "enum": ["settled", "released"]},
            "currency": {"type": "string", "pattern": "^[A-Z]{3}$"},
            "micros": {"type": "integer", "minimum": 0},
            "dry_run": {"type": "boolean"}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "usage.unsettled",
            label: "Review reconciled open charges",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: page_schema,
            params_doc: "{offset?, limit? (≤ 500)} -> {charges: [{provider, key, session, model, node, at_ms, amount, status, location, holder, reconciled_at_ms, token}], total, next_offset, resolved}. Historical charges whose ownership was reconciled but which no run is left to settle. Read-only.",
            enabled: enabled_page,
            run: run_page,
        },
        CommandSpec {
            id: "usage.resolve_charge",
            label: "Settle or release a reconciled charge",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: resolve_schema,
            params_doc: "{provider, key, token, outcome: settled|released, currency? and micros? (the billed amount; settled only), dry_run?: bool} -> {provider, key, session, was, was_status, outcome, cost, ...}; records what the provider's own records show for one charge from usage.unsettled, as confirmed by a person: settled at the billed amount, or released because it was never billed. Refused when the charge changed since `token` was returned. `dry_run` returns the record and the session and project totals afterwards without changing anything. Human only.",
            enabled: enabled_resolve,
            run: run_resolve,
        },
    ]
}

#[cfg(test)]
mod tests;

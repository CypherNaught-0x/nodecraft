//! Settling or releasing ordinary unresolved charges on a person's decision (#298).
//!
//! A job that may have been billed but whose outcome isn't known keeps its estimate as an
//! unresolved charge, so caps hold; nothing settles or releases it from an error kind, a status
//! or a timer. These commands let a person who checked their provider's records say what they
//! show. They are separate from `usage.unsettled` / `usage.resolve_charge` (#332), which decide
//! reconciled legacy charges: no charge is offered on both.
//!
//! - `usage.unresolved` (read-only): pages of the unresolved charges waiting for a decision, each
//!   with the token a decision must carry. At most the MCP page budget of encoded JSON per page.
//! - `usage.resolve_unresolved` (human only): `settled` at the billed amount or `released`
//!   (never billed), bound to the token the person reviewed, with an optional note. The host's
//!   [`BILLING_SERVICE`] applies it to the live usage log, which checks the token again.
//!   `dry_run: true` returns the record and the totals it would produce, changing nothing.
//!
//! Host contract: as for `usage.resolve_charge` (see [`crate::resolve_cmds`]): save the log and
//! replace the [`UsageSnapshot`] after the service succeeds, and refuse a decision while a run
//! might still settle the charge. The desktop's run host does all of it
//! ([`crate::run_host::RunHost::register_resolution`]).

use crate::commands::{CommandSpec, Context, Effect, params};
use crate::ownership_cmds::{MAX_PAGE_JSON_BYTES, fill, hex, invalid, money, status, unhex};
use crate::run_cmds::UsageSnapshot;
use nodecraft_exec::cost::{BillingDecision, BillingResolution, MAX_BILLING_NOTE, QuarantineLocation, Resolution, UsageLog};
use nodecraft_types::{Currency, Error, ErrorKind, Money, ProviderInstanceId};
use serde::Deserialize;
use serde_json::{Value, json};

/// The host service that applies a confirmed decision to the live usage log.
pub const BILLING_SERVICE: &str = "usage.resolve_unresolved";
/// Most charges one `usage.unresolved` page lists.
pub const MAX_UNRESOLVED_PER_PAGE: usize = 500;

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
    #[serde(default)]
    instance: Option<ProviderInstanceId>,
    key: String,
    token: String,
    outcome: Outcome,
    #[serde(default)]
    currency: Option<Currency>,
    #[serde(default)]
    micros: Option<u64>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    dry_run: bool,
}

fn decision_and_mode(value: &Value) -> Result<(BillingDecision, bool), Error> {
    let p = params::<DecisionParams>(value)?;
    let token = unhex(&p.token).ok_or_else(|| invalid("the token must be the 64 hex digits usage.unresolved returned"))?;
    let resolution = match (p.outcome, p.currency, p.micros) {
        (Outcome::Settled, Some(currency), Some(micros)) => Resolution::Settled { cost: Money::new(currency, micros) },
        (Outcome::Settled, _, _) => return Err(invalid("a settled charge needs the billed `currency` and `micros`")),
        (Outcome::Released, None, None) => Resolution::Released,
        (Outcome::Released, _, _) => return Err(invalid("a released charge has no amount")),
    };
    // An empty note is no note.
    let note = p.note.map(|note| note.trim().to_owned()).filter(|note| !note.is_empty());
    if note.as_ref().is_some_and(|note| note.len() > MAX_BILLING_NOTE) {
        return Err(invalid("the note is too long"));
    }
    Ok((BillingDecision { provider: p.provider, instance: p.instance, idempotency_key: p.key, token, resolution, note }, p.dry_run))
}

/// Applies the decision in a service `request` to `log`, recorded at `at_ms`, for a host that
/// saves the result before keeping it.
pub fn resolve(log: &mut UsageLog, request: &Value, at_ms: u64) -> Result<Value, Error> {
    let (decision, dry_run) = decision_and_mode(request)?;
    if dry_run {
        return Err(invalid("a dry run is answered by the command, not the host service"));
    }
    let record = log.resolve_unresolved(&decision, at_ms)?;
    Ok(record_json(&record))
}

/// The `(provider, instance, key)` a service request names, for a host that checks no run
/// still follows it.
pub fn requested_charge(request: &Value) -> Result<(String, Option<ProviderInstanceId>, String), Error> {
    decision_and_mode(request).map(|(d, _)| (d.provider, d.instance, d.idempotency_key))
}

fn record_json(record: &BillingResolution) -> Value {
    let (outcome, cost) = match record.resolution {
        Resolution::Settled { cost } => ("settled", money(cost)),
        Resolution::Released => ("released", Value::Null),
    };
    json!({"provider": record.provider, "instance": record.instance, "key": record.idempotency_key, "session": record.session,
        "node": record.node, "model": record.model, "project": record.project, "recorded_at_ms": record.recorded_at_ms,
        "was": money(record.was), "outcome": outcome, "cost": cost, "note": record.note, "at_ms": record.at_ms})
}

fn total(totals: &std::collections::BTreeMap<Currency, Money>, currency: Currency) -> Money {
    totals.get(&currency).copied().unwrap_or(Money::zero(currency))
}

fn page(usage: &UsageLog, p: &PageParams) -> Result<Value, Error> {
    let mut budget = MAX_PAGE_JSON_BYTES.saturating_sub(16 * 1024);
    let charges = usage.unresolved_charges();
    let limit = p.limit.unwrap_or(MAX_UNRESOLVED_PER_PAGE).min(MAX_UNRESOLVED_PER_PAGE);
    let rows = charges.iter().skip(p.offset).map(|c| {
        let e = c.entry;
        let location = if c.location == QuarantineLocation::Pending { "pending" } else { "listed" };
        json!({"provider": e.provider, "instance": e.instance, "key": e.idempotency_key, "session": e.session, "model": e.model, "node": e.node,
            "project": e.project, "at_ms": e.at_ms, "amount": money(e.cost), "status": status(e.status), "location": location, "token": hex(&c.token)})
    });
    let mut out = Vec::new();
    let next = fill(&mut out, rows, p.offset, charges.len(), limit, &mut budget)?;
    Ok(json!({"charges": out, "total": charges.len(), "next_offset": next, "resolved": usage.billing_resolutions().len(),
        "dropped": usage.billing_resolutions_dropped()}))
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
        usage.0.plan_billing_resolution(&decision, 0)?;
        return Ok(Effect::Service { capability: BILLING_SERVICE, request: value.clone() });
    }
    // The exact outcome, on a copy of the snapshot.
    let mut copy = UsageLog::clone(&usage.0);
    let record = copy.resolve_unresolved(&decision, 0)?;
    let currency = record.was.currency;
    let spent = copy.reported_spent_in(&record.session, record.project);
    let mut result = record_json(&record);
    if let Some(map) = result.as_object_mut() {
        map.remove("at_ms");
        map.insert("session_total".into(), money(total(&spent.session, currency)));
        map.insert("project_total".into(), money(total(&spent.project, currency)));
        if record.project.is_some() {
            map.insert("this_project_total".into(), money(total(&spent.this_project, currency)));
        }
        map.insert("dry_run".into(), json!(true));
    }
    Ok(Effect::None { result })
}

fn enabled_resolve(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let (_, dry_run) = decision_and_mode(value)?;
    c.resources.require::<UsageSnapshot>()?;
    if !dry_run && !c.services.contains(BILLING_SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "this host can't change its usage log"));
    }
    Ok(())
}

fn page_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [], "properties": {
        "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": MAX_UNRESOLVED_PER_PAGE}}})
}

fn resolve_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["provider", "key", "token", "outcome"], "properties": {
            "provider": {"type": "string"},
            "instance": {"type": ["string", "null"], "format": "uuid"},
            "key": {"type": "string"},
            "token": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
            "outcome": {"type": "string", "enum": ["settled", "released"]},
            "currency": {"type": "string", "pattern": "^[A-Z]{3}$"},
            "micros": {"type": "integer", "minimum": 0},
            // At most MAX_BILLING_NOTE bytes of UTF-8, which JSON Schema's character count
            // can't state; the command checks the bytes.
            "note": {"type": "string", "description": "what was checked; at most 256 bytes of UTF-8, no control or invisible formatting characters"},
            "dry_run": {"type": "boolean"}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "usage.unresolved",
            label: "Review charges that may have been billed",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: page_schema,
            params_doc: "{offset?, limit? (≤ 500)} -> {charges: [{provider, instance, key, session, model, node, project, at_ms, amount, status, location, token}], total, next_offset, resolved, dropped (older decisions no longer kept)}. Unresolved charges: jobs that may have been billed although their outcome isn't known (a lost reply, an error after sending, a run given up on), kept at their estimate so budgets hold. Read-only.",
            enabled: enabled_page,
            run: run_page,
        },
        CommandSpec {
            id: "usage.resolve_unresolved",
            label: "Settle or release an unresolved charge",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: resolve_schema,
            params_doc: "{provider, instance?, key, token, outcome: settled|released, currency? and micros? (the billed amount; settled only), note? (what was checked, ≤ 256 bytes of UTF-8), dry_run?: bool} -> {provider, instance, key, session, was, outcome, cost, note, ...}; records what the provider's own records show for one charge from usage.unresolved, as confirmed by a person: settled at the billed amount, or released because it was never billed. Refused when the charge changed since `token` was returned, and while a run might still settle it. `dry_run` returns the record and the totals afterwards without changing anything. Human only.",
            enabled: enabled_resolve,
            run: run_resolve,
        },
    ]
}

#[cfg(test)]
mod tests;

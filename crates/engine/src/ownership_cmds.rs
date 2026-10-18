//! Reconciling quarantined legacy charge ownership (#325).
//!
//! - `usage.ownership` (read-only): pages of the quarantined groups, or of one group's evidence,
//!   totals and charges with the token a decision must carry. At most 768 KiB of encoded JSON
//!   per page.
//! - `usage.reconcile_ownership` (human only): states which total holds each quarantined charge
//!   of one group, bound to the token the person reviewed. The host's
//!   [`OWNERSHIP_SERVICE`] applies it to the live usage log, which checks the token again, so a
//!   stale preview or a ledger that changed meanwhile is refused. Agents may review groups like
//!   any other usage row, but can't run this: the decision needs the person's own records.
//!   `dry_run: true` checks the decision against the host's snapshot and returns the record and
//!   totals it would produce, changing nothing.
//!
//! Host contract: after the service succeeds, save the log and replace the [`UsageSnapshot`]
//! resource, or every later review is refused as stale.
//!
//! Reconciliation never settles, releases or refunds a charge; see `nodecraft_exec::cost`.

use crate::commands::{CommandSpec, Context, Effect, params};
use crate::run_cmds::UsageSnapshot;
use nodecraft_exec::cost::{
    Charge, Holder, MAX_RECONCILE_EXCEPTIONS, OwnershipDecision, OwnershipGroup, OwnershipReconciliation, QuarantineLocation, UsageLog, format_money,
};
use nodecraft_types::{Currency, Error, ErrorKind, Money};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// The host service that applies a confirmed decision to the live usage log.
pub const OWNERSHIP_SERVICE: &str = "usage.reconcile_ownership";
/// Most groups one `usage.ownership` page lists.
pub const MAX_GROUPS_PER_PAGE: usize = 200;
/// Most charges one `usage.ownership` group page lists.
pub const MAX_CHARGES_PER_PAGE: usize = 500;
const MAX_PAGE_JSON_BYTES: usize = 768 * 1024;

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageParams {
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    currency: Option<Currency>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Exception {
    provider: String,
    key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum HolderParam {
    Named,
    Earlier,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionParams {
    session: String,
    currency: Currency,
    token: String,
    holder: HolderParam,
    #[serde(default)]
    except: Vec<Exception>,
    #[serde(default)]
    dry_run: bool,
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Parses `usage.reconcile_ownership` params (also the service request) into a decision.
pub fn decision(value: &Value) -> Result<OwnershipDecision, Error> {
    decision_and_mode(value).map(|(decision, _)| decision)
}

fn decision_and_mode(value: &Value) -> Result<(OwnershipDecision, bool), Error> {
    if value.get("except").and_then(Value::as_array).is_some_and(|list| list.len() > MAX_RECONCILE_EXCEPTIONS) {
        return Err(invalid("too many exceptions in one ownership decision"));
    }
    let p = params::<DecisionParams>(value)?;
    let token = unhex(&p.token).ok_or_else(|| invalid("the token must be the 64 hex digits usage.ownership returned"))?;
    let holder = match p.holder {
        HolderParam::Named => Holder::Named,
        HolderParam::Earlier => Holder::Earlier,
    };
    let decision =
        OwnershipDecision { session: p.session, currency: p.currency, token, holder, except: p.except.into_iter().map(|e| (e.provider, e.key)).collect() };
    Ok((decision, p.dry_run))
}

/// The host's [`OWNERSHIP_SERVICE`] over its live usage log. The log checks the decision's
/// token against its current state; the host saves the log after a successful call.
pub fn ownership_service(log: Arc<Mutex<UsageLog>>, wall_clock_ms: Arc<dyn Fn() -> u64>) -> impl FnMut(&Value) -> Result<Value, Error> {
    move |request| {
        let (decision, dry_run) = decision_and_mode(request)?;
        if dry_run {
            return Err(invalid("a dry run is answered by the command, not the host service"));
        }
        let mut log = log.lock().map_err(|_| Error::new(ErrorKind::Internal, "the usage log is unavailable after a failure; restart NodeCraft"))?;
        let record = log.reconcile_ownership(&decision, wall_clock_ms())?;
        Ok(record_json(&record))
    }
}

fn money(m: Money) -> Value {
    json!({"currency": String::from(m.currency), "micros": m.micros, "text": format_money(m)})
}

fn record_json(record: &OwnershipReconciliation) -> Value {
    let named = record.charges.iter().filter(|c| c.holder == Holder::Named).count();
    json!({
        "session": record.evidence.session.as_ref(),
        "currency": String::from(record.evidence.original_open.currency),
        "charges": record.charges.len(),
        "named": named,
        "earlier": record.charges.len().saturating_sub(named),
        "moved_from_earlier": money(record.moved_from_earlier),
        "at_ms": record.at_ms,
    })
}

fn group_json(session: &str, group: &OwnershipGroup<'_>) -> Value {
    let e = group.evidence;
    json!({
        "session": session,
        "currency": String::from(e.original_open.currency),
        "charges_total": group.charges.len(),
        "evidence": {
            "original_named": money(e.original_named),
            "original_earlier": money(e.original_earlier),
            "remaining_earlier": money(e.remaining_earlier_after_exact_migrations),
            "original_project": money(e.original_project),
            "original_open": money(e.original_open),
        },
        "named_total": money(group.named_total),
        "earlier_total": money(group.earlier_total),
        "established_open": money(group.established_open),
        "max_named": money(group.max_named),
        "max_earlier": money(group.max_earlier),
        "token": hex(&group.token),
    })
}

fn summary_json(session: &str, currency: Currency, charges: usize, evidence: &nodecraft_exec::cost::LegacyGroupEvidence) -> Value {
    json!({"session": session, "currency": String::from(currency), "charges_total": charges, "original_open": money(evidence.original_open)})
}

fn status(charge: Charge) -> &'static str {
    match charge {
        Charge::Actual => "actual",
        Charge::Estimate => "estimate",
        Charge::Unresolved => "unresolved",
    }
}

/// Adds items until `limit` or the byte budget is reached. Returns the next offset, if any.
fn fill(
    out: &mut Vec<Value>,
    items: impl Iterator<Item = Value>,
    offset: usize,
    total: usize,
    limit: usize,
    budget: &mut usize,
) -> Result<Option<usize>, Error> {
    for item in items.take(limit) {
        let size = serde_json::to_vec(&item).map_err(|_| Error::new(ErrorKind::Internal, "an ownership row couldn't be encoded"))?.len().saturating_add(1);
        if size > *budget {
            break;
        }
        *budget -= size;
        out.push(item);
    }
    if out.is_empty() && offset < total {
        return Err(invalid("the page byte limit cannot fit an ownership row"));
    }
    let next = offset.saturating_add(out.len());
    Ok((next < total).then_some(next))
}

fn page(usage: &UsageLog, p: &PageParams) -> Result<Value, Error> {
    // Reserve the envelope, the group summary and the longest continuation number.
    let mut budget = MAX_PAGE_JSON_BYTES.saturating_sub(16 * 1024);
    let reconciled = usage.ownership_reconciliations().len();
    match (&p.session, p.currency) {
        (None, None) => {
            // Summaries only: counting every group in one pass keeps a page cheap on a large log.
            // Review one group (with session and currency) to get its evidence and token.
            let sizes = usage.ownership_group_sizes();
            let groups: Vec<_> = usage.ownership_groups_with_evidence().collect();
            let limit = p.limit.unwrap_or(MAX_GROUPS_PER_PAGE).min(MAX_GROUPS_PER_PAGE);
            let mut out = Vec::new();
            let rows = groups
                .iter()
                .skip(p.offset)
                .map(|(session, currency, evidence)| summary_json(session, *currency, sizes.get(&(*session, *currency)).copied().unwrap_or(0), evidence));
            let next = fill(&mut out, rows, p.offset, groups.len(), limit, &mut budget)?;
            Ok(json!({"groups": out, "total": groups.len(), "next_offset": next, "reconciled": reconciled}))
        }
        (Some(session), Some(currency)) => {
            let group = usage.ownership_group(session, currency)?;
            let summary = group_json(session, &group);
            let limit = p.limit.unwrap_or(MAX_CHARGES_PER_PAGE).min(MAX_CHARGES_PER_PAGE);
            let rows = group.charges.iter().skip(p.offset).map(|row| {
                let e = row.entry;
                let location = match row.location {
                    QuarantineLocation::Listed => "listed",
                    QuarantineLocation::Pending => "pending",
                    QuarantineLocation::Evicted => "evicted",
                };
                json!({"provider": e.provider, "key": e.idempotency_key, "model": e.model, "node": e.node, "at_ms": e.at_ms,
                    "amount": money(e.cost), "status": status(e.status), "location": location})
            });
            let mut out = Vec::new();
            let next = fill(&mut out, rows, p.offset, group.charges.len(), limit, &mut budget)?;
            Ok(json!({"group": summary, "charges": out, "total": group.charges.len(), "next_offset": next, "reconciled": reconciled}))
        }
        _ => Err(invalid("give both `session` and `currency` to list one group's charges, or neither to list the groups")),
    }
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

fn run_reconcile(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let (decision, dry_run) = decision_and_mode(value)?;
    let usage = c.resources.require::<UsageSnapshot>()?;
    // The checks the live log applies, against the snapshot: a dry run reports the outcome;
    // otherwise a stale or impossible decision is refused before reaching the host, whose
    // service checks again against the live log, which is authoritative.
    let plan = usage.0.plan_ownership_reconciliation(&decision, 0)?;
    if dry_run {
        let mut result = record_json(&plan.record);
        if let Some(map) = result.as_object_mut() {
            map.remove("at_ms");
            map.insert("named_total".into(), money(plan.named_total));
            map.insert("earlier_total".into(), money(plan.earlier_total));
            map.insert("dry_run".into(), json!(true));
        }
        return Ok(Effect::None { result });
    }
    Ok(Effect::Service { capability: OWNERSHIP_SERVICE, request: value.clone() })
}

fn enabled_reconcile(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let (_, dry_run) = decision_and_mode(value)?;
    c.resources.require::<UsageSnapshot>()?;
    if !dry_run && !c.services.contains(OWNERSHIP_SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "this host can't change its usage log"));
    }
    Ok(())
}

fn page_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [], "properties": {
        "session": {"type": "string"}, "currency": {"type": "string", "pattern": "^[A-Z]{3}$"},
        "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": MAX_CHARGES_PER_PAGE}}})
}

fn reconcile_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["session", "currency", "token", "holder"], "properties": {
            "session": {"type": "string"},
            "currency": {"type": "string", "pattern": "^[A-Z]{3}$"},
            "token": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
            "holder": {"type": "string", "enum": ["named", "earlier"]},
            "except": {"type": "array", "maxItems": MAX_RECONCILE_EXCEPTIONS, "items": {"type": "object", "additionalProperties": false,
                "required": ["provider", "key"], "properties": {"provider": {"type": "string"}, "key": {"type": "string"}}}},
            "dry_run": {"type": "boolean"}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "usage.ownership",
            label: "Review quarantined charges",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: page_schema,
            params_doc: "{session?, currency?, offset?, limit? (≤ 200 groups or 500 charges)} -> without session/currency: {groups: [{session, currency, charges_total, original_open}], total, next_offset, reconciled}; with both: {group: {session, currency, evidence, named_total, earlier_total, established_open, max_named, max_earlier, token, ...}, charges: [{provider, key, model, node, at_ms, amount, status, location}], total, next_offset, reconciled}. Historical charges whose owning session total is uncertain. Read-only.",
            enabled: enabled_page,
            run: run_page,
        },
        CommandSpec {
            id: "usage.reconcile_ownership",
            label: "Reconcile quarantined charges",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: reconcile_schema,
            params_doc: "{session, currency, token, holder: named|earlier, except?: [{provider, key}] (≤ 1000), dry_run?: bool}; states which total holds every quarantined charge of the group (`holder`, the listed exceptions the other), as confirmed by a person against their own records. Refused when the group changed since usage.ownership returned `token`, or when the totals at import couldn't have held the charges. `dry_run` returns the record and resulting totals without changing anything. Settles nothing. Human only.",
            enabled: enabled_reconcile,
            run: run_reconcile,
        },
    ]
}

#[cfg(test)]
mod tests;

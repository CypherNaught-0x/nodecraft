//! Explicit, person-confirmed reconciliation of quarantined legacy charge ownership (#325).
//!
//! A legacy log can hold open charges whose money sits either in their named session's total or
//! in the folded earlier-sessions total, with nothing in the log to tell which (#257). Nothing
//! here infers it. A person who has checked their own records states, for every quarantined
//! charge of one (session, currency) group, which total holds it. The decision is bound to a
//! token over the exact evidence, charges and totals it was previewed against, and is refused
//! unless it is consistent with the immutable import evidence:
//!
//! - charges said to be in the named total fit within the named total recorded at import;
//! - charges said to be in the earlier total fit within what that total held at import (across
//!   every reconciliation of the currency) and within what it holds now.
//!
//! Earlier-held money moves to the named session (the project total is unchanged), and the
//! charges become ordinary established charges, which can then be settled or released through
//! the normal paths. Reconciliation itself never settles, releases or refunds anything. The
//! group's evidence and the decision are kept as an immutable audit record.
//!
//! A large group can be decided in stages: a decision with `only` covers just the listed
//! charges (at most [`MAX_STAGE_CHARGES`]) and leaves the rest quarantined under the same
//! evidence. Each stage is its own record, reviewed against a fresh token; the caps above are
//! cumulative across a group's stages, and the stages of a group together account for exactly
//! its open amount at import, once each.
use super::ownership::{Identity, identity, identity_target};
use super::*;
use std::sync::Arc;

/// Most exceptions one decision lists; everything else takes the decision's default holder.
pub const MAX_RECONCILE_EXCEPTIONS: usize = 1_000;
/// Most charges one staged decision (`only`) covers.
pub const MAX_STAGE_CHARGES: usize = 1_000;

/// Which total holds a quarantined charge's money.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Holder {
    /// The charge's named session total.
    Named,
    /// The folded earlier-sessions total.
    Earlier,
}

impl Holder {
    fn other(self) -> Self {
        match self {
            Self::Named => Self::Earlier,
            Self::Earlier => Self::Named,
        }
    }
}

/// One charge as it was reconciled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciledCharge {
    pub provider: String,
    pub idempotency_key: String,
    /// The charge's open amount when it was reconciled.
    pub cost: Money,
    pub holder: Holder,
}

/// The audit record of one confirmed reconciliation, saved with the log and never changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipReconciliation {
    /// The group's import evidence, exactly as it was quarantined.
    pub evidence: LegacyGroupEvidence,
    pub charges: Vec<ReconciledCharge>,
    /// The earlier-held total moved into the named session.
    pub moved_from_earlier: Money,
    /// Milliseconds since the Unix epoch, from the host.
    pub at_ms: u64,
    /// The group still has quarantined charges after this stage; a later record completes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
}

/// A person's decision for one group: every quarantined charge in scope takes `holder`, except
/// the listed `(provider, idempotency key)`s, which take the other one. The scope is the whole
/// group, or only the listed charges when `only` is given (one stage).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnershipDecision {
    pub session: String,
    pub currency: Currency,
    /// The token of the preview the person confirmed.
    pub token: [u8; 32],
    pub holder: Holder,
    pub except: Vec<(String, String)>,
    /// The charges this stage covers; `None` covers every charge still quarantined.
    pub only: Option<Vec<(String, String)>>,
}

/// What a person reviews before deciding: the immutable evidence, every quarantined charge of
/// the group, the totals involved and how much each holder can still take.
#[derive(Clone, Debug)]
pub struct OwnershipGroup<'a> {
    pub evidence: &'a LegacyGroupEvidence,
    /// In identity order, which is stable across saves.
    pub charges: Vec<QuarantinedCharge<'a>>,
    /// The named session's recorded total in this currency.
    pub named_total: Money,
    /// The earlier-sessions total in this currency.
    pub earlier_total: Money,
    /// Open charges of this session and currency whose ownership is established.
    pub established_open: Money,
    /// The most the charges can hold in the named total together (what the named total held at
    /// import, less what earlier stages of this group already placed there).
    pub max_named: Money,
    /// The most the charges can hold in the earlier total together.
    pub max_earlier: Money,
    pub token: [u8; 32],
}

/// A checked decision and its consequences, before anything changes (a dry run).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnershipPlan {
    /// The audit record a commit would save.
    pub record: OwnershipReconciliation,
    /// The named session's total afterwards.
    pub named_total: Money,
    /// The earlier-sessions total afterwards.
    pub earlier_total: Money,
    evicted: Vec<Identity>,
}

fn bad(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn same_group(record: &OwnershipReconciliation, evidence: &LegacyGroupEvidence) -> bool {
    record.evidence.session == evidence.session && record.evidence.original_open.currency == evidence.original_open.currency
}

fn sum(currency: Currency, mut amounts: impl Iterator<Item = u64>) -> Result<Money, Error> {
    amounts.try_fold(Money::zero(currency), |total, micros| total.checked_add(Money::new(currency, micros)))
}

/// Named-held money earlier stages of a group already placed. A group is quarantined by one
/// import and never again, so while it is quarantined its records are all partial stages.
fn staged_named(records: &[OwnershipReconciliation], session: &str, currency: Currency) -> Result<Money, Error> {
    sum(
        currency,
        records
            .iter()
            .filter(|r| r.partial && r.evidence.session.as_ref() == session && r.evidence.original_open.currency == currency)
            .flat_map(|r| r.charges.iter().filter(|c| c.holder == Holder::Named).map(|c| c.cost.micros)),
    )
}

/// Earlier-held money already moved out of the earlier total by recorded reconciliations.
fn moved(records: &[OwnershipReconciliation], currency: Currency) -> Result<Money, Error> {
    sum(currency, records.iter().filter(|r| r.moved_from_earlier.currency == currency).map(|r| r.moved_from_earlier.micros))
}

impl UsageLog {
    /// The quarantined groups, as `(session, currency)`, in a stable order.
    pub fn ownership_groups(&self) -> impl Iterator<Item = (&str, Currency)> {
        self.ownership.groups.iter().flat_map(|(session, groups)| groups.keys().map(move |currency| (session.as_ref(), *currency)))
    }

    /// The quarantined groups with their import evidence, in [`Self::ownership_groups`] order.
    pub fn ownership_groups_with_evidence(&self) -> impl Iterator<Item = (&str, Currency, &LegacyGroupEvidence)> {
        self.ownership.groups.iter().flat_map(|(session, groups)| groups.iter().map(move |(currency, evidence)| (session.as_ref(), *currency, evidence)))
    }

    /// Quarantined charges per group, counted in one pass.
    pub fn ownership_group_sizes(&self) -> BTreeMap<(&str, Currency), usize> {
        let mut sizes = BTreeMap::new();
        for row in self.quarantine_entries() {
            let n: &mut usize = sizes.entry((row.entry.session.as_str(), row.entry.cost.currency)).or_default();
            *n = n.saturating_add(1);
        }
        sizes
    }

    /// The most earlier-held money moving to `session` can take while the earlier total still
    /// covers every listed session's shortfall, as load admission requires. `session`'s own
    /// shortfall shrinks as the money arrives, so only the other sessions' shortfall binds once
    /// it is gone: the cap is `max(own shortfall, earlier − others' shortfall)`.
    fn earlier_coverage(&self, session: &str, currency: Currency) -> u64 {
        let mut listed: BTreeMap<&str, u128> = BTreeMap::new();
        for e in self.entries().chain(self.pending.values()).chain(self.ownership.quarantined.values()).filter(|e| e.cost.currency == currency) {
            *listed.entry(e.session.as_str()).or_default() += u128::from(e.cost.micros);
        }
        let own = self.merged_sessions();
        let shortfall = |name: &str, sum: u128| sum.saturating_sub(u128::from(own.get(name).and_then(|t| t.get(&currency)).map_or(0, |m| m.micros)));
        let mut others = 0u128;
        let mut mine = 0u128;
        for (name, sum) in &listed {
            if *name == session {
                mine = shortfall(name, *sum);
            } else {
                others = others.saturating_add(shortfall(name, *sum));
            }
        }
        let earlier = u128::from(own.get(EARLIER_SESSIONS).and_then(|t| t.get(&currency)).map_or(0, |m| m.micros));
        u64::try_from(mine.max(earlier.saturating_sub(others))).unwrap_or(u64::MAX)
    }

    /// The confirmed reconciliations, oldest first.
    pub fn ownership_reconciliations(&self) -> &[OwnershipReconciliation] {
        &self.ownership.records
    }

    /// Everything a person needs to decide one group, and the token that binds the decision.
    pub fn ownership_group(&self, session: &str, currency: Currency) -> Result<OwnershipGroup<'_>, Error> {
        let evidence = self
            .ownership
            .groups
            .get(session)
            .and_then(|groups| groups.get(&currency))
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "no quarantined charges for that session and currency"))?;
        let mut charges: Vec<(Identity, QuarantinedCharge<'_>)> = self
            .quarantine_entries()
            .filter(|row| row.entry.session == session && row.entry.cost.currency == currency)
            .map(|row| (identity_target(&row.entry.provider, row.entry.instance, &row.entry.idempotency_key), row))
            .collect();
        charges.sort_unstable_by_key(|(id, _)| *id);
        let total = |name: &str| Money::new(currency, self.sessions.get(name).and_then(|t| t.get(&currency)).map_or(0, |m| m.micros));
        let named_total = total(session);
        let earlier_total = total(EARLIER_SESSIONS);
        let established_open = sum(
            currency,
            self.entries
                .iter()
                .chain(self.pending.values())
                .filter(|e| e.session == session && e.cost.currency == currency && e.status != Charge::Actual && !self.ownership.is_uncertain(e))
                .map(|e| e.cost.micros),
        )?;
        let moved = moved(&self.ownership.records, currency)?;
        let max_named = evidence.original_named.checked_sub(staged_named(&self.ownership.records, session, currency)?)?;
        let left_at_import = evidence.remaining_earlier_after_exact_migrations.micros.saturating_sub(moved.micros);
        let max_earlier = Money::new(currency, left_at_import.min(earlier_total.micros).min(self.earlier_coverage(session, currency)));
        let mut hash = blake3::Hasher::new();
        hash.update(b"nodecraft:usage-ownership-reconcile:v1\0");
        let mut text = |t: &str| {
            hash.update(&u64::try_from(t.len()).unwrap_or(u64::MAX).to_le_bytes());
            hash.update(t.as_bytes());
        };
        text(session);
        text(&String::from(currency));
        for money in [
            evidence.original_named,
            evidence.original_earlier,
            evidence.remaining_earlier_after_exact_migrations,
            evidence.original_project,
            evidence.original_open,
            named_total,
            earlier_total,
            established_open,
            moved,
            max_named,
            max_earlier,
        ] {
            hash.update(&money.micros.to_le_bytes());
        }
        // This group's stages only: another group's reconciliation reaches this one solely through
        // the earlier total, `moved` and `max_earlier` above, so working through two groups side
        // by side doesn't spend each other's reviews.
        let stages = self.ownership.records.iter().filter(|r| r.evidence.session.as_ref() == session && r.evidence.original_open.currency == currency).count();
        hash.update(&u64::try_from(stages).unwrap_or(u64::MAX).to_le_bytes());
        for (id, row) in &charges {
            let location = match row.location {
                QuarantineLocation::Listed => 0u8,
                QuarantineLocation::Pending => 1,
                QuarantineLocation::Evicted => 2,
            };
            hash.update(id);
            hash.update(&row.entry.cost.micros.to_le_bytes());
            hash.update(&[location, row.entry.status as u8]);
        }
        Ok(OwnershipGroup {
            evidence,
            charges: charges.into_iter().map(|(_, row)| row).collect(),
            named_total,
            earlier_total,
            established_open,
            max_named,
            max_earlier,
            token: *hash.finalize().as_bytes(),
        })
    }

    /// Applies a confirmed decision to one group, or to one stage of it. Everything is checked before anything
    /// changes; on an error the log is unchanged. Returns the saved audit record.
    pub fn reconcile_ownership(&mut self, decision: &OwnershipDecision, at_ms: u64) -> Result<OwnershipReconciliation, Error> {
        let plan = self.plan_ownership_reconciliation(decision, at_ms)?;
        self.commit(&decision.session, plan)
    }

    /// Checks a decision exactly as [`Self::reconcile_ownership`] does and returns what it would
    /// record and the totals afterwards, changing nothing.
    pub fn plan_ownership_reconciliation(&self, decision: &OwnershipDecision, at_ms: u64) -> Result<OwnershipPlan, Error> {
        if decision.except.len() > MAX_RECONCILE_EXCEPTIONS {
            return Err(bad("too many exceptions in one ownership decision"));
        }
        if decision.only.as_ref().is_some_and(|only| only.is_empty() || only.len() > MAX_STAGE_CHARGES) {
            return Err(bad("a staged ownership decision lists between 1 and 1000 charges"));
        }
        for (provider, key) in decision.except.iter().chain(decision.only.iter().flatten()) {
            ownership::check_key(provider, key)?;
        }
        let currency = decision.currency;
        let view = self.ownership_group(&decision.session, currency)?;
        if view.token != decision.token {
            return Err(Error::new(ErrorKind::Conflict, "the quarantined charges or totals changed since they were reviewed; review them again"));
        }
        let mut exceptions = BTreeSet::new();
        for (provider, key) in &decision.except {
            if !exceptions.insert(identity(provider, key)) {
                return Err(bad("an exception is listed twice"));
            }
        }
        let scope = match &decision.only {
            None => None,
            Some(only) => {
                let mut scope = BTreeSet::new();
                for (provider, key) in only {
                    if !scope.insert(identity(provider, key)) {
                        return Err(bad("a staged charge is listed twice"));
                    }
                }
                Some(scope)
            }
        };
        let mut charges = Vec::new();
        charges
            .try_reserve_exact(scope.as_ref().map_or(view.charges.len(), BTreeSet::len))
            .map_err(|_| Error::new(ErrorKind::Busy, "cannot allocate the reconciliation record"))?;
        let mut evicted = Vec::new();
        let mut matched = 0usize;
        let mut remaining = 0usize;
        for row in &view.charges {
            let e = row.entry;
            let id = identity_target(&e.provider, e.instance, &e.idempotency_key);
            if scope.as_ref().is_some_and(|scope| !scope.contains(&id)) {
                remaining += 1;
                continue;
            }
            let holder = if exceptions.contains(&id) {
                matched += 1;
                decision.holder.other()
            } else {
                decision.holder
            };
            if row.location == QuarantineLocation::Evicted {
                evicted.push(id);
            }
            charges.push(ReconciledCharge { provider: e.provider.clone(), idempotency_key: e.idempotency_key.clone(), cost: e.cost, holder });
        }
        if matched != exceptions.len() {
            return Err(bad("an exception is not a quarantined charge of this decision"));
        }
        if scope.as_ref().is_some_and(|scope| scope.len() != charges.len()) {
            return Err(bad("a staged charge is not a quarantined charge of this group"));
        }
        if charges.is_empty() {
            return Err(Error::new(ErrorKind::Internal, "the group has no quarantined charges left to reconcile"));
        }
        let named = sum(currency, charges.iter().filter(|c| c.holder == Holder::Named).map(|c| c.cost.micros))?;
        let earlier = sum(currency, charges.iter().filter(|c| c.holder == Holder::Earlier).map(|c| c.cost.micros))?;
        if named.micros > view.max_named.micros {
            return Err(Error::new(ErrorKind::Conflict, "the named session's total at import can't have held that much of these charges"));
        }
        if earlier.micros > view.max_earlier.micros {
            return Err(Error::new(ErrorKind::Conflict, "the earlier sessions' total can't hold that much of these charges"));
        }
        // The named total must still cover every open charge it will own: the established ones
        // and those said to be in it (earlier-held ones arrive with their money). For a partial
        // stage the stronger load invariant (named total ≥ remaining floor + established) carries
        // over unchanged: the floor drops by `named` exactly as the established sum rises by it.
        if u128::from(view.named_total.micros) < u128::from(view.established_open.micros) + u128::from(named.micros) {
            return Err(Error::new(ErrorKind::Conflict, "the named session's total doesn't cover its open charges"));
        }
        if self.pending.len().saturating_add(evicted.len()) > MAX_PENDING {
            return Err(Error::new(ErrorKind::Busy, "too many pending charges to take these back; settle pending charges first"));
        }
        if remaining == 0 {
            // Quarantined charges can't settle, so the group's stages account for its open
            // amount at import exactly; anything else is a corrupted log, not a decision.
            let staged = sum(
                currency,
                self.ownership.records.iter().filter(|r| r.partial && same_group(r, view.evidence)).flat_map(|r| r.charges.iter().map(|c| c.cost.micros)),
            )?;
            if staged.checked_add(named)?.checked_add(earlier)? != view.evidence.original_open {
                return Err(Error::new(ErrorKind::Internal, "the quarantined charges don't add up to the group's open amount at import"));
            }
        }
        let record = OwnershipReconciliation { evidence: view.evidence.clone(), charges, moved_from_earlier: earlier, at_ms, partial: remaining > 0 };
        let named_total = view.named_total.checked_add(earlier)?;
        let earlier_total = view.earlier_total.checked_sub(earlier)?;
        Ok(OwnershipPlan { record, named_total, earlier_total, evicted })
    }

    fn commit(&mut self, session: &str, plan: OwnershipPlan) -> Result<OwnershipReconciliation, Error> {
        let OwnershipPlan { record, evicted, .. } = plan;
        let currency = record.evidence.original_open.currency;
        let earlier = record.moved_from_earlier;
        let mut sessions = touched_sessions(&self.sessions, session);
        sessions.extend(touched_sessions(&self.sessions, EARLIER_SESSIONS));
        if let Some(totals) = sessions.get_mut(EARLIER_SESSIONS) {
            sub_from(totals, earlier)?;
        } else if earlier.micros > 0 {
            return Err(Error::new(ErrorKind::Internal, "usage totals are missing the earlier sessions"));
        }
        give_to_session(&mut sessions, session, earlier)?;
        self.ownership.records.try_reserve(1).map_err(|_| Error::new(ErrorKind::Busy, "cannot allocate the reconciliation record"))?;
        let saved = record.clone();
        // All fallible admission is complete; commit.
        self.sessions.extend(sessions);
        for charge in &record.charges {
            self.ownership.uncertain.remove(&identity(&charge.provider, &charge.idempotency_key));
        }
        for id in evicted {
            if let Some(entry) = self.ownership.quarantined.remove(&id) {
                self.pending_identities.insert(id);
                self.pending.insert((entry.provider.clone(), entry.instance, entry.idempotency_key.clone()), entry);
            }
        }
        if !record.partial
            && let Some(groups) = self.ownership.groups.get_mut(session)
        {
            groups.remove(&currency);
            if groups.is_empty() {
                self.ownership.groups.remove(session);
            }
        }
        self.ownership.records.push(record);
        Ok(saved)
    }
}

/// Load admission for saved records (marker 3). `groups` are the still-quarantined groups and
/// `uncertain` their charges. Returns, per still-quarantined group, the open amount its partial
/// stages already reconciled, so the caller can check it with the retained uncertainty.
pub(super) fn admit_records(
    records: &[OwnershipReconciliation],
    groups: &BTreeMap<Arc<str>, BTreeMap<Currency, LegacyGroupEvidence>>,
    uncertain: &BTreeSet<Identity>,
) -> Result<BTreeMap<(Arc<str>, Currency), StagedTotals>, Error> {
    let closing: BTreeSet<(Arc<str>, Currency)> =
        records.iter().filter(|r| !r.partial).map(|r| (Arc::clone(&r.evidence.session), r.evidence.original_open.currency)).collect();
    let mut closed_groups = BTreeSet::new();
    let mut staged: BTreeMap<(Arc<str>, Currency), StagedTotals> = BTreeMap::new();
    let mut seen_charges = BTreeSet::new();
    let mut moved_total: BTreeMap<Currency, u128> = BTreeMap::new();
    for record in records {
        let g = &record.evidence;
        let c = g.original_open.currency;
        let key = (Arc::clone(&g.session), c);
        let quarantined = groups.get(g.session.as_ref()).and_then(|groups| groups.get(&c));
        if record.charges.is_empty()
            || g.session.is_empty()
            || g.session.len() > MAX_TEXT
            || record.moved_from_earlier.currency != c
            // A still-quarantined group has only partial stages; a closed group's stages end in
            // one complete record and are never staged again.
            || (record.partial && quarantined.is_none() && !closing.contains(&key))
            || (!record.partial && quarantined.is_some())
            || closed_groups.contains(&key)
        {
            return Err(bad("invalid ownership reconciliation"));
        }
        // Every stage of a group carries the group's evidence unchanged.
        let earlier_stages = staged.remove(&key);
        if quarantined.is_some_and(|q| q != g) || earlier_stages.as_ref().is_some_and(|s| s.evidence != *g) {
            return Err(bad("ownership reconciliation is inconsistent with its evidence"));
        }
        let mut totals = earlier_stages.unwrap_or_else(|| StagedTotals { evidence: g.clone(), named: 0, open: 0 });
        let mut earlier = 0u128;
        for charge in &record.charges {
            ownership::check_key(&charge.provider, &charge.idempotency_key)?;
            let id = identity(&charge.provider, &charge.idempotency_key);
            if charge.cost.currency != c
                || charge.cost.micros == 0
                || charge.cost.micros > MAX_CHARGE_MICROS
                || uncertain.contains(&id)
                || !seen_charges.insert(id)
            {
                return Err(bad("invalid reconciled charge"));
            }
            let micros = u128::from(charge.cost.micros);
            totals.open += micros;
            match charge.holder {
                Holder::Named => totals.named += micros,
                Holder::Earlier => earlier += micros,
            }
        }
        let total = moved_total.entry(c).or_default();
        *total += earlier;
        if earlier != u128::from(record.moved_from_earlier.micros)
            || totals.named > u128::from(g.original_named.micros)
            || *total > u128::from(g.remaining_earlier_after_exact_migrations.micros)
            || totals.open > u128::from(g.original_open.micros)
        {
            return Err(bad("ownership reconciliation is inconsistent with its evidence"));
        }
        if record.partial {
            staged.insert(key, totals);
        } else {
            // Quarantined charges can't settle, so a closed group's stages account for its open
            // amount at import exactly, each charge once.
            if totals.open != u128::from(g.original_open.micros) {
                return Err(bad("ownership reconciliation is inconsistent with its evidence"));
            }
            closed_groups.insert(key);
        }
    }
    if seen_charges.len().saturating_add(uncertain.len()) > MAX_USAGE_ENTRIES + MAX_PENDING {
        return Err(bad("too much reconciled ownership history"));
    }
    Ok(staged)
}

/// What a still-quarantined group's partial stages reconciled so far, in micros.
pub(super) struct StagedTotals {
    evidence: LegacyGroupEvidence,
    /// Placed in the named total.
    pub(super) named: u128,
    /// Every staged charge.
    pub(super) open: u128,
}

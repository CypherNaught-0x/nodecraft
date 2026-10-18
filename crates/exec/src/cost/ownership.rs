//! Immutable historical ownership evidence; never infer chronology or a deficit's owner.
use super::*;
use std::sync::Arc;

type Identity = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargeOwnership {
    Established,
    LegacyUncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuarantineLocation {
    Listed,
    Pending,
    Evicted,
}

#[derive(Clone, Copy, Debug)]
pub struct QuarantinedCharge<'a> {
    pub entry: &'a UsageEntry,
    pub evidence: &'a LegacyGroupEvidence,
    pub location: QuarantineLocation,
}

/// Evidence from one admitted import, not a cryptographic attestation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyGroupEvidence {
    #[serde(with = "shared_session")]
    pub session: Arc<str>,
    pub original_named: Money,
    pub original_earlier: Money,
    pub remaining_earlier_after_exact_migrations: Money,
    pub original_project: Money,
    pub original_open: Money,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Ownership {
    pub(super) uncertain: BTreeSet<Identity>,
    pub(super) quarantined: BTreeMap<Identity, UsageEntry>,
    pub(super) groups: BTreeMap<Arc<str>, BTreeMap<Currency, LegacyGroupEvidence>>,
}

pub(super) fn identity(provider: &str, key: &str) -> Identity {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nodecraft:usage-quarantine:v2\0");
    for text in [provider, key] {
        hash.update(&u64::try_from(text.len()).unwrap_or(u64::MAX).to_le_bytes());
        hash.update(text.as_bytes());
    }
    *hash.finalize().as_bytes()
}

pub(super) fn check_key(provider: &str, key: &str) -> Result<(), Error> {
    if provider.len() > MAX_TEXT || key.len() > MAX_TEXT || key.is_empty() {
        Err(Error::new(ErrorKind::InvalidInput, "usage identity is empty or too long"))
    } else {
        Ok(())
    }
}

pub(super) fn uncertain_error() -> Error {
    Error::new(
        ErrorKind::Conflict,
        "Historical charge ownership is uncertain; retain the log and inspect provider records. This charge cannot be settled or released.",
    )
}

fn bad(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn amount(totals: Option<&Totals>, currency: Currency) -> u64 {
    totals.and_then(|t| t.get(&currency)).map_or(0, |m| m.micros)
}
fn all_entries(file: &UsageLogFile) -> impl Iterator<Item = &UsageEntry> {
    file.entries.iter().chain(&file.pending).chain(&file.quarantined)
}

pub(super) fn check_file_bounds(file: &UsageLogFile) -> Result<(), Error> {
    if file.open_session_ownership > 2 {
        return Err(bad("unsupported open-session ownership version; use a newer NodeCraft"));
    }
    let retained = file.entries.len().checked_add(file.pending.len()).and_then(|n| n.checked_add(file.quarantined.len()));
    if retained.is_none_or(|n| n > MAX_USAGE_ENTRIES + MAX_PENDING)
        || file.uncertain_listed.len() > file.entries.len()
        || file.uncertain_pending.len() > file.pending.len()
        || file.legacy_groups.len() > MAX_USAGE_ENTRIES + MAX_PENDING
    {
        return Err(bad("too much retained ownership history"));
    }
    let bytes = all_entries(file).fold(0usize, |n, e| n.saturating_add(entry_text(e)));
    if bytes > MAX_LISTED_TEXT + MAX_PENDING * 4 * MAX_TEXT {
        return Err(bad("too much retained ownership text"));
    }
    if file.open_session_ownership != 2
        && (!file.uncertain_listed.is_empty() || !file.uncertain_pending.is_empty() || !file.quarantined.is_empty() || !file.legacy_groups.is_empty())
    {
        return Err(bad("ownership sidecars require version 2"));
    }
    Ok(())
}

impl Ownership {
    pub(super) fn is_uncertain(&self, e: &UsageEntry) -> bool {
        self.uncertain.contains(&identity(&e.provider, &e.idempotency_key))
    }
    pub(super) fn evidence(&self, e: &UsageEntry) -> Option<&LegacyGroupEvidence> {
        self.groups.get(e.session.as_str())?.get(&e.cost.currency)
    }

    pub(super) fn admit(file: &mut UsageLogFile) -> Result<Self, Error> {
        let mut result = Self::default();
        let mut open: BTreeMap<&str, Totals> = BTreeMap::new();
        for e in all_entries(file).filter(|e| e.status != Charge::Actual) {
            add_to(open.entry(&e.session).or_default(), e.cost)?;
        }
        if file.open_session_ownership == 0 {
            let original = file.sessions.clone();
            let mut sessions = file.sessions.clone();
            // Stage every zero-owner transfer before classifying positive-owner groups.
            for (name, totals) in &open {
                for money in totals.values() {
                    if amount(original.get(*name), money.currency) == 0 {
                        if money.micros > 0 {
                            sub_from(sessions.get_mut(EARLIER_SESSIONS).ok_or_else(|| bad("legacy open charges exceed earlier totals"))?, *money)
                                .map_err(|_| bad("legacy open charges exceed earlier totals"))?;
                        }
                        add_to(sessions.entry((*name).to_owned()).or_default(), *money)?;
                    }
                }
            }
            if sessions.len() > MAX_SESSIONS + 1 + MAX_USAGE_ENTRIES + MAX_PENDING {
                return Err(bad("too many sessions after legacy ownership normalization"));
            }
            for (name, totals) in &open {
                for money in totals.values().filter(|m| m.micros > 0) {
                    let currency = money.currency;
                    let named = amount(original.get(*name), currency);
                    let earlier = amount(sessions.get(EARLIER_SESSIONS), currency);
                    if named > 0 && earlier > 0 {
                        let owner = result.groups.get_key_value(*name).map(|(owner, _)| Arc::clone(owner)).unwrap_or_else(|| Arc::from(*name));
                        let evidence = LegacyGroupEvidence {
                            session: Arc::clone(&owner),
                            original_named: Money::new(currency, named),
                            original_earlier: Money::new(currency, amount(original.get(EARLIER_SESSIONS), currency)),
                            remaining_earlier_after_exact_migrations: Money::new(currency, earlier),
                            original_project: Money::new(currency, amount(Some(&file.project), currency)),
                            original_open: *money,
                        };
                        result.groups.entry(owner).or_default().insert(currency, evidence);
                    }
                }
            }
            for e in all_entries(file).filter(|e| e.status != Charge::Actual && e.cost.micros > 0) {
                if result.evidence(e).is_some() {
                    result.uncertain.insert(identity(&e.provider, &e.idempotency_key));
                }
            }
            file.sessions = sessions;
        } else if file.open_session_ownership == 2 {
            let mut shared: BTreeMap<Currency, (Money, Money, Money, u128)> = BTreeMap::new();
            for group in &file.legacy_groups {
                let c = group.original_open.currency;
                if group.session.is_empty()
                    || group.session.len() > MAX_TEXT
                    || group.original_open.micros == 0
                    || group.original_named.micros == 0
                    || group.remaining_earlier_after_exact_migrations.micros == 0
                    || [group.original_named, group.original_earlier, group.remaining_earlier_after_exact_migrations, group.original_project]
                        .iter()
                        .any(|m| m.currency != c || m.micros > MAX_TOTAL_MICROS)
                    || group.original_open.micros > MAX_TOTAL_MICROS
                    || group.original_earlier.micros < group.remaining_earlier_after_exact_migrations.micros
                    || amount(file.sessions.get(EARLIER_SESSIONS), c) < group.remaining_earlier_after_exact_migrations.micros
                {
                    return Err(bad("invalid legacy ownership evidence"));
                }
                let row = shared.entry(c).or_insert((group.original_project, group.original_earlier, group.remaining_earlier_after_exact_migrations, 0));
                if (row.0, row.1, row.2) != (group.original_project, group.original_earlier, group.remaining_earlier_after_exact_migrations) {
                    return Err(bad("contradictory ownership import evidence"));
                }
                row.3 = row.3.saturating_add(u128::from(group.original_named.micros));
                if row.3.saturating_add(u128::from(row.1.micros)) > u128::from(row.0.micros) {
                    return Err(bad("ownership evidence exceeds original project"));
                }
                let owner =
                    result.groups.get_key_value(group.session.as_ref()).map(|(owner, _)| Arc::clone(owner)).unwrap_or_else(|| Arc::clone(&group.session));
                let mut stored = group.clone();
                stored.session = Arc::clone(&owner);
                if result.groups.entry(owner).or_default().insert(c, stored).is_some() {
                    return Err(bad("duplicate ownership group"));
                }
            }
            for (indices, entries) in [(&file.uncertain_listed, &file.entries), (&file.uncertain_pending, &file.pending)] {
                let mut previous = None;
                for index in indices {
                    if previous.is_some_and(|p| p >= *index) {
                        return Err(bad("ownership indices must be strictly ordered"));
                    }
                    let e = usize::try_from(*index).ok().and_then(|n| entries.get(n)).ok_or_else(|| bad("ownership index out of range"))?;
                    if e.status == Charge::Actual || e.cost.micros == 0 || result.evidence(e).is_none() {
                        return Err(bad("invalid uncertain entry"));
                    }
                    result.uncertain.insert(identity(&e.provider, &e.idempotency_key));
                    previous = Some(*index);
                }
            }
            for e in &file.quarantined {
                if result.evidence(e).is_none() {
                    return Err(bad("quarantined entry has no evidence"));
                }
                result.uncertain.insert(identity(&e.provider, &e.idempotency_key));
            }
            let mut sums: BTreeMap<&str, Totals> = BTreeMap::new();
            for e in all_entries(file).filter(|e| result.is_uncertain(e)) {
                add_to(sums.entry(&e.session).or_default(), e.cost)?;
            }
            if result.uncertain.is_empty() {
                return Err(bad("version 2 requires uncertain charges"));
            }
            for groups in result.groups.values() {
                for group in groups.values() {
                    if amount(sums.get(group.session.as_ref()), group.original_open.currency) != group.original_open.micros {
                        return Err(bad("ownership evidence does not match retained uncertainty"));
                    }
                }
            }
        }
        let mut identities: BTreeMap<Identity, (&str, &str)> = BTreeMap::new();
        for e in all_entries(file) {
            if identities.insert(identity(&e.provider, &e.idempotency_key), (&e.provider, &e.idempotency_key)).is_some() {
                return Err(bad("usage identity collision"));
            }
        }
        let mut established: BTreeMap<&str, Totals> = BTreeMap::new();
        for e in all_entries(file).filter(|e| e.status != Charge::Actual && !result.is_uncertain(e)) {
            add_to(established.entry(&e.session).or_default(), e.cost)?;
        }
        for (name, totals) in established {
            if !file.sessions.contains_key(name) {
                return Err(bad("named session is missing for an owned open charge"));
            }
            for money in totals.values() {
                let floor = result.groups.get(name).and_then(|g| g.get(&money.currency)).map_or(0, |g| g.original_named.micros);
                if u128::from(amount(file.sessions.get(name), money.currency)) < u128::from(floor) + u128::from(money.micros) {
                    return Err(bad("named session totals do not cover their owned open charges and legacy floor"));
                }
            }
        }
        for (name, groups) in &result.groups {
            for g in groups.values() {
                if amount(file.sessions.get(name.as_ref()), g.original_named.currency) < g.original_named.micros {
                    return Err(bad("legacy named floor was reduced"));
                }
            }
        }
        result.quarantined = std::mem::take(&mut file.quarantined).into_iter().map(|e| (identity(&e.provider, &e.idempotency_key), e)).collect();
        Ok(result)
    }
}

mod shared_session {
    use super::*;
    pub fn serialize<S: serde::Serializer>(name: &Arc<str>, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(name)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Arc<str>, D::Error> {
        String::deserialize(deserializer).map(Arc::from)
    }
}

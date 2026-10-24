//! Person-confirmed settlement of reconciled legacy charges (#332).
//!
//! A legacy charge whose ownership a person reconciled (#325) is an ordinary established open
//! charge again, but no job is left to settle it: the run that held it ended in an older
//! NodeCraft. Nothing here guesses its outcome. A person who has checked their provider's
//! records states it: **settled** at the amount the provider billed, or **released** because
//! the provider never billed it. Releasing an unresolved charge is allowed here, unlike the
//! automatic path, because the person has confirmed it against their records.
//!
//! The decision is bound to a token over the charge as it was reviewed, so a charge that
//! changed meanwhile is refused. Each decision is final (the charge is no longer open
//! afterwards) and is kept as an immutable audit record, so a charge is resolved at most once
//! and the records are bounded by the reconciled charges.
use super::ownership::{Identity, identity};
use super::*;

/// What a person found in their provider's records for one charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Resolution {
    /// The provider billed `cost`; the charge settles as actual at that amount.
    Settled { cost: Money },
    /// The provider never billed it; the charge leaves the totals.
    Released,
}

/// The audit record of one resolved charge, saved with the log and never changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChargeResolution {
    pub provider: String,
    pub idempotency_key: String,
    pub session: String,
    /// The open amount when it was resolved.
    pub was: Money,
    /// The open status (estimate or unresolved) when it was resolved.
    pub was_status: Charge,
    pub resolution: Resolution,
    /// Milliseconds since the Unix epoch, from the host.
    pub at_ms: u64,
}

/// A person's decision for one reconciled open charge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionDecision {
    pub provider: String,
    pub idempotency_key: String,
    /// The token of the review the person confirmed.
    pub token: [u8; 32],
    pub resolution: Resolution,
}

/// A reconciled charge that is still open, as a person reviews it.
#[derive(Clone, Copy, Debug)]
pub struct ResolvableCharge<'a> {
    pub entry: &'a UsageEntry,
    /// Listed, or pending (left the list while open).
    pub location: QuarantineLocation,
    /// Which total the reconciliation said held it.
    pub holder: Holder,
    /// When its ownership was reconciled.
    pub reconciled_at_ms: u64,
    pub token: [u8; 32],
}

fn bad(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn token(entry: &UsageEntry, location: QuarantineLocation) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nodecraft:usage-resolution:v1\0");
    for text in [entry.provider.as_str(), entry.idempotency_key.as_str(), entry.session.as_str(), &String::from(entry.cost.currency)] {
        hash.update(&u64::try_from(text.len()).unwrap_or(u64::MAX).to_le_bytes());
        hash.update(text.as_bytes());
    }
    hash.update(&entry.cost.micros.to_le_bytes());
    let location = match location {
        QuarantineLocation::Listed => 0u8,
        QuarantineLocation::Pending => 1,
        QuarantineLocation::Evicted => 2,
    };
    hash.update(&[location, entry.status as u8]);
    *hash.finalize().as_bytes()
}

/// Checks saved resolutions against the reconciliations they follow: each names a reconciled
/// charge, at most once, in its currency. A released key may be recorded again later by a new
/// job, so whether the charge is open now isn't checked.
pub(super) fn admit_resolutions(file: &UsageLogFile) -> Result<(), Error> {
    let reconciled: BTreeMap<Identity, Currency> = file
        .ownership_reconciliations
        .iter()
        .flat_map(|record| record.charges.iter())
        .map(|charge| (identity(&charge.provider, &charge.idempotency_key), charge.cost.currency))
        .collect();
    let mut seen = BTreeSet::new();
    for r in &file.charge_resolutions {
        ownership::check_key(&r.provider, &r.idempotency_key)?;
        let id = identity(&r.provider, &r.idempotency_key);
        let settled = match r.resolution {
            Resolution::Settled { cost } => Some(cost),
            Resolution::Released => None,
        };
        if r.session.len() > MAX_TEXT
            || r.was.micros > MAX_CHARGE_MICROS
            || r.was_status == Charge::Actual
            || reconciled.get(&id) != Some(&r.was.currency)
            || settled.is_some_and(|cost| cost.currency != r.was.currency || cost.micros > MAX_CHARGE_MICROS)
            || !seen.insert(id)
        {
            return Err(bad("invalid charge resolution"));
        }
    }
    Ok(())
}

impl UsageLog {
    /// Reconciled charges that are still open and not yet resolved, oldest reconciliation first.
    pub fn resolvable_charges(&self) -> Vec<ResolvableCharge<'_>> {
        let resolved: BTreeSet<Identity> = self.ownership.resolutions.iter().map(|r| identity(&r.provider, &r.idempotency_key)).collect();
        let mut out = Vec::new();
        for record in &self.ownership.records {
            for charge in record.charges.iter().filter(|c| !resolved.contains(&identity(&c.provider, &c.idempotency_key))) {
                let Ok((location, entry)) = self.open_entry(&charge.provider, None, &charge.idempotency_key) else { continue };
                let location = match location {
                    Location::Listed(_) => QuarantineLocation::Listed,
                    Location::Pending => QuarantineLocation::Pending,
                };
                out.push(ResolvableCharge { entry, location, holder: charge.holder, reconciled_at_ms: record.at_ms, token: token(entry, location) });
            }
        }
        out
    }

    /// The confirmed resolutions, oldest first.
    pub fn charge_resolutions(&self) -> &[ChargeResolution] {
        &self.ownership.resolutions
    }

    /// Checks a decision exactly as [`Self::resolve_charge`] does and returns the record it
    /// would save, changing nothing.
    pub fn plan_charge_resolution(&self, decision: &ResolutionDecision, at_ms: u64) -> Result<ChargeResolution, Error> {
        ownership::check_key(&decision.provider, &decision.idempotency_key)?;
        let charge = self
            .resolvable_charges()
            .into_iter()
            .find(|c| c.entry.provider == decision.provider && c.entry.idempotency_key == decision.idempotency_key)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "no reconciled charge with that key is waiting for a decision"))?;
        if charge.token != decision.token {
            return Err(Error::new(ErrorKind::Conflict, "the charge changed since it was reviewed; review it again"));
        }
        let entry = charge.entry;
        if let Resolution::Settled { cost } = decision.resolution {
            if cost.currency != entry.cost.currency {
                return Err(Error::new(ErrorKind::CurrencyMismatch, "a charge can't change currency when it settles"));
            }
            if cost.micros > MAX_CHARGE_MICROS {
                return Err(bad("a charge is larger than any job could cost"));
            }
        }
        Ok(ChargeResolution {
            provider: entry.provider.clone(),
            idempotency_key: entry.idempotency_key.clone(),
            session: entry.session.clone(),
            was: entry.cost,
            was_status: entry.status,
            resolution: decision.resolution,
            at_ms,
        })
    }

    /// Applies a person's decision to one reconciled open charge and keeps its audit record.
    /// Everything is checked before anything changes; on an error the log is unchanged.
    pub fn resolve_charge(&mut self, decision: &ResolutionDecision, at_ms: u64) -> Result<ChargeResolution, Error> {
        let record = self.plan_charge_resolution(decision, at_ms)?;
        self.ownership.resolutions.try_reserve(1).map_err(|_| Error::new(ErrorKind::Busy, "cannot allocate the resolution record"))?;
        match record.resolution {
            Resolution::Settled { cost } => self.settle_identity(&record.provider, None, &record.idempotency_key, cost, Charge::Actual)?,
            Resolution::Released => self.release_confirmed(&record.provider, &record.idempotency_key)?,
        }
        self.ownership.resolutions.push(record.clone());
        Ok(record)
    }
}

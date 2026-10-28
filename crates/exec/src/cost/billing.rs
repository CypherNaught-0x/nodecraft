//! Person-confirmed settlement of ordinary unresolved charges (#298).
//!
//! A job that may have been billed but whose outcome isn't known (a lost submit, an error after
//! the request left, a run given up on) keeps its estimate as an [`Charge::Unresolved`] charge,
//! so caps hold. Nothing here infers its outcome from an error kind, a status or a timer. A
//! person who has checked their provider's records states it: **settled** at the billed amount,
//! or **released** because it was never billed.
//!
//! This is billing uncertainty, distinct from the historical ownership uncertainty of #255/#325:
//! a charge [`UsageLog::resolvable_charges`] lists is decided there, never here, and quarantined
//! charges are never offered. Each decision is bound to a token over the charge as reviewed, is
//! final (the charge stops being unresolved), and is kept as an immutable audit record; the newest
//! [`MAX_BILLING_RESOLUTIONS`] records are kept.
use super::ownership::{self, Identity};
use super::resolve::Resolution;
use super::*;

/// Most billing decisions kept; past it the oldest record leaves (its charge is already final).
pub const MAX_BILLING_RESOLUTIONS: usize = 10_000;
/// Longest note a decision may carry, in bytes.
pub const MAX_BILLING_NOTE: usize = 256;

/// The audit record of one resolved unresolved charge, saved with the log and never changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillingResolution {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<nodecraft_types::ProviderInstanceId>,
    pub idempotency_key: String,
    pub session: String,
    pub node: NodeId,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectId>,
    /// When the charge was recorded (ms since the Unix epoch).
    pub recorded_at_ms: u64,
    /// The open amount when it was resolved.
    pub was: Money,
    pub resolution: Resolution,
    /// What the person checked, if they said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When it was decided (ms since the Unix epoch, from the host).
    pub at_ms: u64,
}

/// A person's decision for one unresolved charge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BillingDecision {
    pub provider: String,
    pub instance: Option<nodecraft_types::ProviderInstanceId>,
    pub idempotency_key: String,
    /// The token of the review the person confirmed.
    pub token: [u8; 32],
    pub resolution: Resolution,
    pub note: Option<String>,
}

/// An unresolved charge waiting for a decision, as a person reviews it.
#[derive(Clone, Copy, Debug)]
pub struct UnresolvedCharge<'a> {
    pub entry: &'a UsageEntry,
    /// Listed, or pending (left the list while open).
    pub location: QuarantineLocation,
    pub token: [u8; 32],
}

fn bad(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn token(entry: &UsageEntry, location: QuarantineLocation) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nodecraft:usage-billing-resolution:v1\0");
    let currency = String::from(entry.cost.currency);
    for text in [entry.provider.as_str(), entry.idempotency_key.as_str(), entry.session.as_str(), entry.model.as_str(), &currency] {
        hash.update(&u64::try_from(text.len()).unwrap_or(u64::MAX).to_le_bytes());
        hash.update(text.as_bytes());
    }
    match entry.instance {
        Some(instance) => hash.update(&[1]).update(instance.as_bytes()),
        None => hash.update(&[0]),
    };
    match entry.project {
        Some(project) => hash.update(&[1]).update(project.as_bytes()),
        None => hash.update(&[0]),
    };
    hash.update(entry.node.as_bytes());
    hash.update(&entry.at_ms.to_le_bytes());
    hash.update(&entry.cost.micros.to_le_bytes());
    let location = if location == QuarantineLocation::Pending { 1u8 } else { 0 };
    hash.update(&[location, entry.status as u8]);
    *hash.finalize().as_bytes()
}

/// Invisible formatting characters that could make a note read differently than it is stored:
/// bidirectional embeddings, overrides and isolates, zero-width characters and the byte-order
/// mark (the Unicode `Cf` characters a note could plausibly carry).
fn is_format(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

/// A note is optional, at most [`MAX_BILLING_NOTE`] bytes and plain: no control or invisible
/// formatting characters.
pub(super) fn check_note(note: Option<&str>) -> Result<(), Error> {
    match note {
        Some(text) if text.len() > MAX_BILLING_NOTE => Err(bad("the note is too long")),
        Some(text) if text.chars().any(|c| c.is_control() || is_format(c)) => Err(bad("the note can't hold control or invisible formatting characters")),
        _ => Ok(()),
    }
}

/// Checks saved billing decisions: bounded, keyed and consistent in currency. Whether the
/// charge is still open isn't checked: a released key may be recorded again by a new job.
pub(super) fn admit_billing(file: &UsageLogFile) -> Result<(), Error> {
    if file.billing_resolutions.len() > MAX_BILLING_RESOLUTIONS {
        return Err(bad("usage log: too many billing decisions"));
    }
    // Records are only dropped from a full list, which never shrinks again.
    if file.billing_resolutions_dropped > 0 && file.billing_resolutions.len() != MAX_BILLING_RESOLUTIONS {
        return Err(bad("usage log: dropped billing decisions without a full list"));
    }
    for r in &file.billing_resolutions {
        ownership::check_key(&r.provider, &r.idempotency_key)?;
        let settled = match r.resolution {
            Resolution::Settled { cost } => Some(cost),
            Resolution::Released => None,
        };
        if r.session.len() > MAX_TEXT
            || r.model.len() > MAX_TEXT
            || (r.instance.is_some() && file.provider_identity_version == 0)
            || r.was.micros > MAX_CHARGE_MICROS
            || settled.is_some_and(|cost| cost.currency != r.was.currency || cost.micros > MAX_CHARGE_MICROS)
            || check_note(r.note.as_deref()).is_err()
        {
            return Err(bad("usage log: invalid billing decision"));
        }
    }
    Ok(())
}

impl UsageLog {
    /// Unresolved charges waiting for a person's decision: established, not quarantined and not
    /// on the reconciled-charge path. Listed ones oldest first, then those that left the list.
    pub fn unresolved_charges(&self) -> Vec<UnresolvedCharge<'_>> {
        let reconciled: BTreeSet<Identity> =
            self.resolvable_charges().iter().map(|c| ownership::identity_target(&c.entry.provider, c.entry.instance, &c.entry.idempotency_key)).collect();
        let waiting = |e: &&UsageEntry| {
            let id = ownership::identity_target(&e.provider, e.instance, &e.idempotency_key);
            e.status == Charge::Unresolved && !self.ownership.uncertain.contains(&id) && !reconciled.contains(&id)
        };
        let listed = self.entries.iter().filter(waiting).map(|entry| (entry, QuarantineLocation::Listed));
        let pending = self.pending.values().filter(waiting).map(|entry| (entry, QuarantineLocation::Pending));
        listed.chain(pending).map(|(entry, location)| UnresolvedCharge { entry, location, token: token(entry, location) }).collect()
    }

    /// The kept billing decisions, oldest first.
    pub fn billing_resolutions(&self) -> impl ExactSizeIterator<Item = &BillingResolution> {
        self.billing.iter()
    }

    /// How many older billing decisions were dropped past [`MAX_BILLING_RESOLUTIONS`]. Their
    /// charges were already final (settled as actual or released).
    pub fn billing_resolutions_dropped(&self) -> u64 {
        self.billing_dropped
    }

    /// Checks a decision exactly as [`Self::resolve_unresolved`] does and returns the record it
    /// would save, changing nothing.
    pub fn plan_billing_resolution(&self, decision: &BillingDecision, at_ms: u64) -> Result<BillingResolution, Error> {
        ownership::check_key(&decision.provider, &decision.idempotency_key)?;
        check_note(decision.note.as_deref())?;
        let charge = self
            .unresolved_charges()
            .into_iter()
            .find(|c| c.entry.provider == decision.provider && c.entry.instance == decision.instance && c.entry.idempotency_key == decision.idempotency_key)
            .ok_or_else(|| {
                Error::new(ErrorKind::NotFound, "no unresolved charge with that key is waiting for a decision; it may have been settled or released already")
            })?;
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
        Ok(BillingResolution {
            provider: entry.provider.clone(),
            instance: entry.instance,
            idempotency_key: entry.idempotency_key.clone(),
            session: entry.session.clone(),
            node: entry.node,
            model: entry.model.clone(),
            project: entry.project,
            recorded_at_ms: entry.at_ms,
            was: entry.cost,
            resolution: decision.resolution,
            note: decision.note.clone(),
            at_ms,
        })
    }

    /// Applies a person's decision to one unresolved charge and keeps its audit record.
    /// Everything is checked before anything changes; on an error the log is unchanged.
    pub fn resolve_unresolved(&mut self, decision: &BillingDecision, at_ms: u64) -> Result<BillingResolution, Error> {
        let record = self.plan_billing_resolution(decision, at_ms)?;
        self.billing.try_reserve(1).map_err(|_| Error::new(ErrorKind::Busy, "cannot allocate the billing decision record"))?;
        match record.resolution {
            Resolution::Settled { cost } => self.settle_identity(&record.provider, record.instance, &record.idempotency_key, cost, Charge::Actual)?,
            Resolution::Released => self.release_open(&record.provider, record.instance, &record.idempotency_key, true)?,
        }
        if self.billing.len() >= MAX_BILLING_RESOLUTIONS && self.billing.pop_front().is_some() {
            self.billing_dropped = self.billing_dropped.saturating_add(1);
        }
        self.billing.push_back(record.clone());
        Ok(record)
    }
}

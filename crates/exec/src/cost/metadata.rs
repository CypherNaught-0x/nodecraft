//! Live, non-durable metadata capabilities. Actual money stays in the admitted shell.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

/// Additional metadata shells, included in the ordinary retained entry/text limits.
pub const MAX_METADATA_SHELLS: usize = 10_000;

/// Borrowed admission data: the ledger checks capacity before copying these strings.
#[derive(Clone, Copy, Debug)]
pub struct UsageMetadata<'a> {
    pub at_ms: u64,
    pub session: &'a str,
    /// The project the charge is for (#847).
    pub project: Option<nodecraft_types::ProjectId>,
    pub node: NodeId,
    pub provider: &'a str,
    pub instance: Option<nodecraft_types::ProviderInstanceId>,
    pub model: &'a str,
    pub idempotency_key: &'a str,
}

#[derive(Clone, Debug)]
struct OwnedMetadata {
    at_ms: u64,
    session: String,
    project: Option<nodecraft_types::ProjectId>,
    node: NodeId,
    provider: String,
    instance: Option<nodecraft_types::ProviderInstanceId>,
    model: String,
    key: String,
}

/// Identity of one live ledger, not transferable by Clone or serialization.
#[derive(Clone, Debug)]
pub struct LedgerIdentity(Arc<()>);
impl PartialEq for LedgerIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for LedgerIdentity {}

/// An admitted shell. Not Clone, constructible, serializable or redeemable in a snapshot.
#[derive(Debug)]
pub struct MetadataPermit {
    live: LedgerIdentity,
    serial: u64,
    state: Arc<AtomicU8>,
}
impl Drop for MetadataPermit {
    fn drop(&mut self) {
        let _ = self.state.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
    }
}

/// Allocation-free finalization error. Never construct a general owned Error in this path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataFinalizeError {
    LedgerReplaced,
    PermitConsumed,
    InvalidReportedMoney,
    TotalCeiling,
    OwnershipConflict,
    CapacityInvariant,
}
impl MetadataFinalizeError {
    pub fn kind(self) -> ErrorKind {
        match self {
            Self::LedgerReplaced | Self::PermitConsumed | Self::OwnershipConflict => ErrorKind::Conflict,
            Self::InvalidReportedMoney => ErrorKind::InvalidInput,
            Self::TotalCeiling => ErrorKind::Overflow,
            Self::CapacityInvariant => ErrorKind::Internal,
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::LedgerReplaced => "Accounting reservation belongs to another live usage log; restore the original log before retrying.",
            Self::PermitConsumed => "This accounting reservation has already been consumed.",
            Self::InvalidReportedMoney => "The provider-reported charge exceeds the accepted charge limit; retain the completed output for accounting repair.",
            Self::TotalCeiling => "Recording this charge would exceed the usage log total limit; retain the completed output for accounting repair.",
            Self::OwnershipConflict => "Charge ownership does not permit this accounting update; retain the completed output for inspection.",
            Self::CapacityInvariant => "Admitted accounting metadata is unavailable; retain the completed output and report this accounting failure.",
        }
    }
}
impl std::fmt::Display for MetadataFinalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for MetadataFinalizeError {}

#[derive(Clone, Debug)]
enum Payload {
    Held(OwnedMetadata),
    Actual(UsageEntry),
    Vacant,
}
#[derive(Debug)]
pub(super) struct Shell {
    serial: u64,
    state: Arc<AtomicU8>,
    payload: Payload,
}
impl Shell {
    fn entry(&self) -> Option<&UsageEntry> {
        if let Payload::Actual(e) = &self.payload { Some(e) } else { None }
    }
    fn fields(&self) -> Option<(&str, &str, &str, &str)> {
        match &self.payload {
            Payload::Held(m) => Some((&m.session, &m.provider, &m.model, &m.key)),
            Payload::Actual(e) => Some((&e.session, &e.provider, &e.model, &e.idempotency_key)),
            Payload::Vacant => None,
        }
    }
    pub(super) fn matches(&self, provider: &str, instance: Option<nodecraft_types::ProviderInstanceId>, key: &str) -> bool {
        match &self.payload {
            Payload::Held(m) => m.provider == provider && m.instance == instance && m.key == key,
            Payload::Actual(e) => e.provider == provider && e.instance == instance && e.idempotency_key == key,
            Payload::Vacant => false,
        }
    }
    fn text(&self) -> usize {
        self.fields().map_or(0, |(s, p, m, k)| s.len().saturating_add(p.len()).saturating_add(m.len()).saturating_add(k.len()))
    }
}

#[derive(Debug)]
pub(super) struct Metadata {
    live: LedgerIdentity,
    shells: Vec<Shell>,
    /// Full digest plus serial, sorted; no second copy of the strings.
    identities: Vec<([u8; 32], u64)>,
}
impl Default for Metadata {
    fn default() -> Self {
        Self { live: LedgerIdentity(Arc::new(())), shells: Vec::new(), identities: Vec::new() }
    }
}
impl Clone for Metadata {
    fn clone(&self) -> Self {
        let shells: Vec<_> = self
            .shells
            .iter()
            .filter_map(|s| s.entry().map(|e| Shell { serial: s.serial, state: Arc::new(AtomicU8::new(1)), payload: Payload::Actual(e.clone()) }))
            .collect();
        let mut identities: Vec<_> =
            shells.iter().filter_map(|s| s.entry().map(|e| (ownership::identity_target(&e.provider, e.instance, &e.idempotency_key), s.serial))).collect();
        identities.sort_unstable();
        Self { live: LedgerIdentity(Arc::new(())), shells, identities }
    }
}

/// Backing allocations remain charged while vacant; these are container bytes, not RSS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadataStatistics {
    pub held: usize,
    pub finalized: usize,
    pub text_bytes: usize,
    pub backing_bytes: usize,
}

impl Metadata {
    pub(super) fn actuals(&self) -> impl Iterator<Item = (u64, &UsageEntry)> {
        self.shells.iter().filter_map(|s| s.entry().map(|e| (s.serial, e)))
    }
    pub(super) fn held(&self) -> usize {
        self.shells.iter().filter(|s| s.state.load(Ordering::Acquire) == 0).count()
    }
    pub(super) fn retained(&self) -> usize {
        self.shells.iter().filter(|s| s.state.load(Ordering::Acquire) != 2).count()
    }
    pub(super) fn text(&self) -> usize {
        self.shells.iter().filter(|s| s.state.load(Ordering::Acquire) != 2).fold(0usize, |n, s| n.saturating_add(s.text()))
    }
    pub(super) fn protects(&self, name: &str) -> bool {
        self.shells.iter().any(|s| s.state.load(Ordering::Acquire) != 2 && s.fields().is_some_and(|f| f.0 == name))
    }
    pub(super) fn currency_sum(&self, session: Option<&str>, currency: Currency) -> Option<u64> {
        self.actuals()
            .filter(|(_, e)| e.cost.currency == currency && session.is_none_or(|s| e.session == s))
            .try_fold(0u64, |n, (_, e)| n.checked_add(e.cost.micros))
    }
    pub(super) fn lookup(&self, provider: &str, instance: Option<nodecraft_types::ProviderInstanceId>, key: &str) -> Option<&Shell> {
        let id = ownership::identity_target(provider, instance, key);
        let n = self.identities.binary_search_by_key(&id, |p| p.0).ok()?;
        let serial = self.identities.get(n)?.1;
        let s = self.shells.get(self.shells.binary_search_by_key(&serial, |s| s.serial).ok()?)?;
        (s.state.load(Ordering::Acquire) != 2).then_some(s)
    }
    pub(super) fn charge(&self, provider: &str, instance: Option<nodecraft_types::ProviderInstanceId>, key: &str) -> Option<&UsageEntry> {
        self.lookup(provider, instance, key)?.entry().filter(|e| e.provider == provider && e.instance == instance && e.idempotency_key == key)
    }
    pub(super) fn sweep(&mut self) {
        self.shells.retain(|s| s.state.load(Ordering::Acquire) != 2);
        self.identities.retain(|(_, serial)| self.shells.binary_search_by_key(serial, |s| s.serial).is_ok());
    }
}

fn copy_text(text: &str) -> Result<String, Error> {
    #[cfg(test)]
    COPIED_BYTES.with(|bytes| bytes.set(bytes.get().saturating_add(text.len())));
    let mut result = String::new();
    result.try_reserve_exact(text.len()).map_err(|_| Error::new(ErrorKind::Busy, "cannot allocate admitted accounting text"))?;
    result.push_str(text);
    Ok(result)
}

pub(super) fn compact_entry(entry: &mut UsageEntry) -> Result<(), Error> {
    // An owned caller may supply short text with arbitrarily large spare capacity.
    entry.session = copy_text(&entry.session)?;
    entry.provider = copy_text(&entry.provider)?;
    entry.model = copy_text(&entry.model)?;
    entry.idempotency_key = copy_text(&entry.idempotency_key)?;
    Ok(())
}

pub(super) fn compact_session(name: &str) -> Result<String, Error> {
    copy_text(name)
}

#[cfg(test)]
thread_local! { static COPIED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
pub(super) fn copied_bytes() -> usize {
    COPIED_BYTES.with(std::cell::Cell::get)
}

impl UsageLog {
    /// Borrow-only complete FIFO admission. Moving uncertainty frees no retained capacity.
    pub(super) fn room_for(&self, text: usize) -> Result<usize, Error> {
        let mut listed = self.entries.len().saturating_add(self.metadata.retained());
        let mut listed_text = self.text_bytes.saturating_add(self.metadata.text());
        let mut pending = self.pending.len();
        let mut retained = listed.saturating_add(pending).saturating_add(self.ownership.quarantined.len());
        let mut retained_text = listed_text
            .saturating_add(self.pending.values().fold(0usize, |n, e| n.saturating_add(entry_text(e))))
            .saturating_add(self.ownership.quarantined.values().fold(0usize, |n, e| n.saturating_add(entry_text(e))));
        let fits = |listed: usize, listed_text: usize, retained: usize, retained_text: usize| {
            listed < MAX_USAGE_ENTRIES
                && listed_text.saturating_add(text) <= MAX_LISTED_TEXT
                && retained < MAX_USAGE_ENTRIES + MAX_PENDING
                && retained_text.saturating_add(text) <= MAX_LISTED_TEXT + MAX_PENDING * 4 * MAX_TEXT
        };
        let mut evict = 0;
        let mut rows = self.entries();
        while !fits(listed, listed_text, retained, retained_text) {
            let Some(e) = rows.next() else {
                return Err(Error::new(
                    ErrorKind::Busy,
                    "Retained accounting history is full; export it and start an independent usage log. Uncertain charges cannot be discarded.",
                ));
            };
            evict += 1;
            listed = listed.saturating_sub(1);
            listed_text = listed_text.saturating_sub(entry_text(e));
            if self.ownership.is_uncertain(e) {
                continue;
            }
            if e.status != Charge::Actual && pending < MAX_PENDING {
                pending += 1;
            } else {
                retained = retained.saturating_sub(1);
                retained_text = retained_text.saturating_sub(entry_text(e));
            }
        }
        Ok(evict)
    }

    pub fn ledger_identity(&self) -> LedgerIdentity {
        self.metadata.live.clone()
    }
    pub fn metadata_statistics(&self) -> MetadataStatistics {
        MetadataStatistics {
            held: self.metadata.held(),
            finalized: self.metadata.actuals().count(),
            text_bytes: self.metadata.text(),
            backing_bytes: self
                .metadata
                .shells
                .capacity()
                .saturating_mul(std::mem::size_of::<Shell>())
                .saturating_add(self.metadata.identities.capacity().saturating_mul(std::mem::size_of::<([u8; 32], u64)>())),
        }
    }

    pub fn reserve_metadata(&mut self, m: UsageMetadata<'_>) -> Result<MetadataPermit, Error> {
        ownership::check_key(m.provider, m.idempotency_key)?;
        if m.session.is_empty() || m.session.len() > MAX_TEXT || m.model.len() > MAX_TEXT {
            return Err(Error::new(ErrorKind::InvalidInput, "invalid accounting metadata"));
        }
        self.metadata.sweep();
        if self.ownership_identity(m.provider, m.instance, m.idempotency_key)? == Some(ChargeOwnership::LegacyUncertain) {
            return Err(ownership::uncertain_error());
        }
        if self.charge_identity(m.provider, m.instance, m.idempotency_key).is_some()
            || self.retired.contains(&retired_target_id(m.provider, m.instance, m.idempotency_key))
        {
            return Err(Error::new(ErrorKind::Conflict, "that accounting identity is already recorded"));
        }
        if let Some(shell) = self.metadata.lookup(m.provider, m.instance, m.idempotency_key) {
            if !shell.matches(m.provider, m.instance, m.idempotency_key) {
                return Err(Error::new(ErrorKind::Conflict, "usage identity digest collision"));
            }
            return Err(Error::new(ErrorKind::Busy, "that accounting identity already has an active metadata reservation"));
        }
        if self.metadata.shells.len() >= MAX_METADATA_SHELLS {
            return Err(Error::new(ErrorKind::Busy, "accounting metadata capacity is full"));
        }
        let serial = self.next_order.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "accounting sequence exhausted"))?;
        let text = m.session.len().saturating_add(m.provider.len()).saturating_add(m.model.len()).saturating_add(m.idempotency_key.len());
        let evict = self.room_for(text)?;
        // Prepare new buffers off-ledger; failed admission leaves even backing capacities intact.
        let mut shells = Vec::new();
        shells.try_reserve_exact(self.metadata.shells.len() + 1).map_err(|_| Error::new(ErrorKind::Busy, "cannot allocate accounting metadata"))?;
        let mut identities = Vec::new();
        identities.try_reserve_exact(self.metadata.identities.len() + 1).map_err(|_| Error::new(ErrorKind::Busy, "cannot allocate accounting index"))?;
        if shells.capacity() > MAX_METADATA_SHELLS || identities.capacity() > MAX_METADATA_SHELLS {
            return Err(Error::new(ErrorKind::Busy, "accounting allocation exceeds capacity"));
        }
        let owned = OwnedMetadata {
            at_ms: m.at_ms,
            session: copy_text(m.session)?,
            project: m.project,
            node: m.node,
            provider: copy_text(m.provider)?,
            instance: m.instance,
            model: copy_text(m.model)?,
            key: copy_text(m.idempotency_key)?,
        };
        let state = Arc::new(AtomicU8::new(0));
        // This commit may materialize already-finalized overlays, never pending holds.
        let retired = self.retired.prepare(self.retirement_batch(evict))?;
        self.flush_metadata()?;
        for _ in 0..evict {
            self.retire_oldest();
        }
        retired.commit(&mut self.retired);
        shells.append(&mut self.metadata.shells);
        identities.append(&mut self.metadata.identities);
        self.metadata.shells = shells;
        self.metadata.identities = identities;
        self.metadata.shells.push(Shell { serial, state: Arc::clone(&state), payload: Payload::Held(owned) });
        let id = ownership::identity_target(m.provider, m.instance, m.idempotency_key);
        let at = self.metadata.identities.binary_search_by_key(&id, |p| p.0).unwrap_or_else(|n| n);
        self.metadata.identities.insert(at, (id, serial));
        self.next_order = serial;
        if m.instance.is_some() {
            self.provider_identity_version = 1;
        }
        Ok(MetadataPermit { live: self.metadata.live.clone(), serial, state })
    }

    /// Checks a live capability without consuming it or allocating.
    pub fn validate_metadata(&self, permit: &MetadataPermit) -> Result<(), MetadataFinalizeError> {
        use MetadataFinalizeError as F;
        if permit.live != self.metadata.live {
            return Err(F::LedgerReplaced);
        }
        if permit.state.load(Ordering::Acquire) != 0 {
            return Err(F::PermitConsumed);
        }
        let index = self.metadata.shells.binary_search_by_key(&permit.serial, |s| s.serial).map_err(|_| F::CapacityInvariant)?;
        let shell = self.metadata.shells.get(index).ok_or(F::CapacityInvariant)?;
        if !Arc::ptr_eq(&shell.state, &permit.state) || !matches!(shell.payload, Payload::Held(_)) {
            return Err(F::CapacityInvariant);
        }
        Ok(())
    }

    /// No allocation, including on failure. Successful money lives in the admitted shell.
    pub fn finalize_metadata(&mut self, permit: &mut MetadataPermit, cost: Money) -> Result<(), MetadataFinalizeError> {
        use MetadataFinalizeError as F;
        if permit.live != self.metadata.live {
            return Err(F::LedgerReplaced);
        }
        if permit.state.load(Ordering::Acquire) != 0 {
            return Err(F::PermitConsumed);
        }
        let index = self.metadata.shells.binary_search_by_key(&permit.serial, |s| s.serial).map_err(|_| F::CapacityInvariant)?;
        let shell = self.metadata.shells.get(index).ok_or(F::CapacityInvariant)?;
        if !Arc::ptr_eq(&shell.state, &permit.state) {
            return Err(F::CapacityInvariant);
        }
        let Payload::Held(m) = &shell.payload else { return Err(F::CapacityInvariant) };
        if cost.micros > MAX_CHARGE_MICROS {
            return Err(F::InvalidReportedMoney);
        }
        if self.ownership.uncertain.contains(&ownership::identity_target(&m.provider, m.instance, &m.key)) {
            return Err(F::OwnershipConflict);
        }
        let current = self.project.get(&cost.currency).map_or(0, |m| m.micros);
        let extra = self.metadata.currency_sum(None, cost.currency).ok_or(F::TotalCeiling)?;
        if current.checked_add(extra).and_then(|n| n.checked_add(cost.micros)).is_none_or(|n| n > MAX_TOTAL_MICROS) {
            return Err(F::TotalCeiling);
        }
        let shell = self.metadata.shells.get_mut(index).ok_or(F::CapacityInvariant)?;
        let previous = std::mem::replace(&mut shell.payload, Payload::Vacant);
        let Payload::Held(m) = previous else {
            shell.payload = previous;
            return Err(F::CapacityInvariant);
        };
        shell.payload = Payload::Actual(UsageEntry {
            at_ms: m.at_ms,
            session: m.session,
            project: m.project,
            node: m.node,
            provider: m.provider,
            instance: m.instance,
            model: m.model,
            idempotency_key: m.key,
            cost,
            status: Charge::Actual,
        });
        permit.state.store(1, Ordering::Release);
        Ok(())
    }

    pub fn cancel_metadata(&mut self, permit: &mut MetadataPermit) -> Result<(), MetadataFinalizeError> {
        if permit.live != self.metadata.live {
            return Err(MetadataFinalizeError::LedgerReplaced);
        }
        if permit.state.load(Ordering::Acquire) != 0 {
            return Err(MetadataFinalizeError::PermitConsumed);
        }
        permit.state.store(2, Ordering::Release);
        self.metadata.sweep();
        Ok(())
    }

    /// Ordinary admitted transaction, never called by finalization. Move finalized shells to base.
    /// The next mutation performs an O(listed rows + finalized shells) merge/rebuild under the
    /// retained-entry caps; allocation-free finalization itself does not rebuild these indexes.
    pub(super) fn flush_metadata(&mut self) -> Result<(), Error> {
        if self.metadata.actuals().next().is_none() {
            return Ok(());
        }
        let count = self.entries.len().saturating_add(self.metadata.actuals().count());
        let mut rows = Vec::new();
        rows.try_reserve_exact(count.saturating_add(1)).map_err(|_| Error::new(ErrorKind::Busy, "cannot materialize accounted entries"))?;
        let mut entries = VecDeque::new();
        entries.try_reserve_exact(count.saturating_add(1)).map_err(|_| Error::new(ErrorKind::Busy, "cannot materialize accounted entries"))?;
        let mut order = VecDeque::new();
        order.try_reserve_exact(count.saturating_add(1)).map_err(|_| Error::new(ErrorKind::Busy, "cannot materialize accounting sequence"))?;
        let mut project = self.project.clone();
        let mut sessions = self.sessions.clone();
        let mut projects = self.projects.clone();
        for (_, e) in self.metadata.actuals() {
            add_to(&mut project, e.cost)?;
            add_to(sessions.entry(e.session.clone()).or_default(), e.cost)?;
            if let Some(id) = e.project {
                add_to(projects.entry(id).or_default(), e.cost)?;
                self.project_clock.insert(id, self.clock);
            }
        }
        // Build replacement key nodes BEFORE touching existing storage.
        let keys: BTreeMap<_, _> =
            self.entries().enumerate().map(|(n, e)| (ownership::identity_target(&e.provider, e.instance, &e.idempotency_key), n as u64)).collect();
        rows.extend(self.entry_order.drain(..).zip(self.entries.drain(..)));
        for shell in &mut self.metadata.shells {
            if shell.entry().is_some()
                && let Payload::Actual(e) = std::mem::replace(&mut shell.payload, Payload::Vacant)
            {
                rows.push((shell.serial, e));
            }
        }
        rows.sort_unstable_by_key(|r| r.0);
        for (seq, e) in rows {
            order.push_back(seq);
            entries.push_back(e);
        }
        self.metadata.shells.retain(|s| !matches!(s.payload, Payload::Vacant));
        self.metadata.identities.retain(|(_, n)| self.metadata.shells.binary_search_by_key(n, |s| s.serial).is_ok());
        self.entries = entries;
        self.entry_order = order;
        self.keys = keys;
        self.first_seq = 0;
        self.text_bytes = self.entries.iter().fold(0usize, |n, e| n.saturating_add(entry_text(e)));
        self.project = project;
        self.sessions = sessions;
        self.projects = projects;
        self.trim_projects(None);
        Ok(())
    }
}

#[cfg(test)]
impl Metadata {
    pub(super) fn replace_first_digest_for_test(&mut self, digest: [u8; 32]) {
        if let Some(first) = self.identities.first_mut() {
            first.0 = digest;
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod allocation_tests {
    use super::*;
    fn m(key: &str) -> UsageMetadata<'_> {
        UsageMetadata {
            project: None,
            instance: None,
            at_ms: 1,
            session: "s",
            provider: "p",
            model: "m",
            idempotency_key: key,
            node: NodeId::from_bytes([1; 16]),
        }
    }
    fn checked(f: impl FnOnce() -> Result<(), MetadataFinalizeError>, expected: Result<(), MetadataFinalizeError>) {
        let mut result = None;
        let info = allocation_counter::measure(|| result = Some(f()));
        assert_eq!(info.count_total, 0, "metadata finalizer allocated: {info:?}");
        assert_eq!(info.bytes_total, 0);
        assert_eq!(result, Some(expected));
    }
    #[test]
    fn allocator_measurement_is_thread_local_under_parallel_allocations() {
        let signals = std::sync::Arc::new((AtomicU8::new(0), AtomicU8::new(0)));
        let other = std::sync::Arc::clone(&signals);
        let thread = std::thread::spawn(move || {
            while other.0.load(Ordering::Acquire) == 0 {
                std::hint::spin_loop();
            }
            for _ in 0..128 {
                drop(std::hint::black_box(String::from("other test thread allocation")));
            }
            other.1.store(1, Ordering::Release);
        });
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut completed = false;
        // Barrier's first wait may initialize parking state; atomic coordination keeps the
        // measured thread free of those unrelated allocations.
        let measured = allocation_counter::measure(|| {
            signals.0.store(1, Ordering::Release);
            while signals.1.load(Ordering::Acquire) == 0 && std::time::Instant::now() < until {
                std::hint::spin_loop();
            }
            completed = signals.1.load(Ordering::Acquire) == 1;
        });
        thread.join().unwrap();
        assert!(completed);
        assert_eq!(measured.count_total, 0);
        assert_eq!(measured.bytes_total, 0);
    }

    #[test]
    fn actual_allocator_observes_zero_success_and_late_failure_allocations() {
        use MetadataFinalizeError as F;
        let usd = |n| Money::new(Currency::USD, n);
        let control = allocation_counter::measure(|| drop(std::hint::black_box(String::from("allocator positive control"))));
        assert!(control.count_total > 0, "the test must really install/observe the allocator");
        let mut log = UsageLog::default();
        let mut token = log.reserve_metadata(m("success")).unwrap();
        checked(|| log.finalize_metadata(&mut token, usd(7)), Ok(()));
        checked(|| log.finalize_metadata(&mut token, usd(7)), Err(F::PermitConsumed));
        let mut invalid = log.reserve_metadata(m("invalid")).unwrap();
        checked(|| log.finalize_metadata(&mut invalid, usd(MAX_CHARGE_MICROS + 1)), Err(F::InvalidReportedMoney));
        let mut foreign = log.clone();
        checked(|| foreign.finalize_metadata(&mut invalid, usd(1)), Err(F::LedgerReplaced));
        assert_eq!(log.metadata_statistics().held, 1);
        checked(|| log.finalize_metadata(&mut invalid, usd(1)), Ok(()));

        let mut ceiling = UsageLog::default();
        ceiling.project.insert(Currency::USD, usd(MAX_TOTAL_MICROS));
        ceiling.sessions.insert("s".into(), [(Currency::USD, usd(MAX_TOTAL_MICROS))].into());
        let mut cap = ceiling.reserve_metadata(m("ceiling")).unwrap();
        checked(|| ceiling.finalize_metadata(&mut cap, usd(1)), Err(F::TotalCeiling));
        assert_eq!(ceiling.metadata_statistics().held, 1);

        let mut invariant = UsageLog::default();
        let mut missing = invariant.reserve_metadata(m("missing")).unwrap();
        invariant.metadata.shells.clear();
        checked(|| invariant.finalize_metadata(&mut missing, usd(1)), Err(F::CapacityInvariant));
        let mut ownership = UsageLog::default();
        let mut uncertain = ownership.reserve_metadata(m("uncertain")).unwrap();
        ownership.ownership.uncertain.insert(super::super::ownership::identity("p", "uncertain"));
        checked(|| ownership.finalize_metadata(&mut uncertain, usd(1)), Err(F::OwnershipConflict));
        assert_eq!(ownership.metadata_statistics().held, 1);
    }
}

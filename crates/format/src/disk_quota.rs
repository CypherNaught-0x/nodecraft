//! Session-owned logical disk bytes. Unlinked files remain charged while a handle pins them.
use nodecraft_types::{Error, ErrorKind};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
const GIB: u64 = 1024 * 1024 * 1024;
const MAX_RESERVATIONS: u64 = 4096;
const MAX_BACKINGS: u64 = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskPool {
    Verified,
    Bases,
    Journals,
    Staging,
    Metadata,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskUsage {
    pub verified: u64,
    pub bases: u64,
    pub journals: u64,
    pub staging: u64,
    pub metadata: u64,
}
impl DiskUsage {
    pub const HARD: Self = Self { verified: 128 * GIB, bases: 132 * GIB, journals: 264 * GIB, staging: 66 * GIB, metadata: GIB };
    fn get(self, pool: DiskPool) -> u64 {
        match pool {
            DiskPool::Verified => self.verified,
            DiskPool::Bases => self.bases,
            DiskPool::Journals => self.journals,
            DiskPool::Staging => self.staging,
            DiskPool::Metadata => self.metadata,
        }
    }
    fn set(&mut self, pool: DiskPool, n: u64) {
        match pool {
            DiskPool::Verified => self.verified = n,
            DiskPool::Bases => self.bases = n,
            DiskPool::Journals => self.journals = n,
            DiskPool::Staging => self.staging = n,
            DiskPool::Metadata => self.metadata = n,
        }
    }
    pub fn total(self) -> Result<u64, Error> {
        self.verified
            .checked_add(self.bases)
            .and_then(|n| n.checked_add(self.journals))
            .and_then(|n| n.checked_add(self.staging))
            .and_then(|n| n.checked_add(self.metadata))
            .ok_or_else(|| error("owned disk extent overflow"))
    }
}
struct State {
    used: DiskUsage,
    limits: DiskUsage,
    total: u64,
    reservations: u64,
    cleanup_retained: u64,
    backings: u64,
    base_backings: u64,
}
#[derive(Clone)]
pub struct DiskQuota(Arc<Mutex<State>>, Arc<Mutex<()>>);
impl Default for DiskQuota {
    fn default() -> Self {
        Self(
            Arc::new(Mutex::new(State {
                used: DiskUsage::default(),
                limits: DiskUsage::HARD,
                total: 640 * GIB,
                reservations: 0,
                cleanup_retained: 0,
                backings: 0,
                base_backings: 0,
            })),
            Arc::new(Mutex::new(())),
        )
    }
}
impl DiskQuota {
    pub fn lower(limits: DiskUsage, total: u64) -> Result<Self, Error> {
        if total > 640 * GIB
            || [DiskPool::Verified, DiskPool::Bases, DiskPool::Journals, DiskPool::Staging, DiskPool::Metadata]
                .iter()
                .any(|p| limits.get(*p) > DiskUsage::HARD.get(*p))
        {
            return Err(error("disk quotas may only be lowered"));
        }
        Ok(Self(
            Arc::new(Mutex::new(State { used: DiskUsage::default(), limits, total, reservations: 0, cleanup_retained: 0, backings: 0, base_backings: 0 })),
            Arc::new(Mutex::new(())),
        ))
    }
    /// Serialize the shared staging pool without holding accounting state during I/O.
    /// A busy operation refuses before creating or materializing a stage; callers may retry.
    pub fn try_stage(&self) -> Result<MutexGuard<'_, ()>, Error> {
        self.1.try_lock().map_err(|e| match e {
            TryLockError::WouldBlock => error("another storage transfer is using the staging pool; retry after it finishes"),
            TryLockError::Poisoned(_) => error("storage staging gate is unavailable"),
        })
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn stage_operation(&self) -> Result<StageOperation<'_>, Error> {
        Ok(StageOperation { quota: self, _guard: self.try_stage()? })
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn remaining(&self, pool: DiskPool) -> Result<u64, Error> {
        let state = self.0.lock().map_err(|_| error("disk quota lock is unavailable"))?;
        Ok(state.limits.get(pool).saturating_sub(state.used.get(pool)).min(state.total.saturating_sub(state.used.total()?)))
    }
    /// Count owned open backing files separately from byte reservations. Reader clones
    /// share this permit, so retired files remain counted until their final handle closes.
    pub fn reserve_backing(&self) -> Result<BackingReservation, Error> {
        self.reserve_backing_kind(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn reserve_backing_for(&self, pool: DiskPool) -> Result<BackingReservation, Error> {
        self.reserve_backing_kind(pool == DiskPool::Bases)
    }
    fn reserve_backing_kind(&self, base: bool) -> Result<BackingReservation, Error> {
        let mut state = self.0.lock().map_err(|_| error("disk quota lock is unavailable"))?;
        let next = state
            .backings
            .checked_add(1)
            .filter(|n| *n <= MAX_BACKINGS)
            .ok_or_else(|| error("too many retained asset backings; release readers before retrying"))?;
        let bases = state
            .base_backings
            .checked_add(u64::from(base))
            .filter(|n| *n <= 2)
            .ok_or_else(|| error("private checkpoint generations are pinned; release readers before retrying"))?;
        state.backings = next;
        state.base_backings = bases;
        Ok(BackingReservation(self.clone(), base))
    }
    pub fn backing_count(&self) -> Result<u64, Error> {
        Ok(self.0.lock().map_err(|_| error("disk quota lock is unavailable"))?.backings)
    }
    pub fn usage(&self) -> Result<DiskUsage, Error> {
        Ok(self.0.lock().map_err(|_| error("disk quota lock is unavailable"))?.used)
    }
    pub fn cleanup_retained_bytes(&self) -> Result<u64, Error> {
        Ok(self.0.lock().map_err(|_| error("disk quota lock is unavailable"))?.cleanup_retained)
    }
    pub fn reserve(&self, pool: DiskPool, bytes: u64) -> Result<DiskReservation, Error> {
        let mut state = self.0.lock().map_err(|_| error("disk quota lock is unavailable"))?;
        let count = state.reservations.checked_add(1).filter(|n| *n <= MAX_RESERVATIONS).ok_or_else(|| error("too many retained disk objects"))?;
        let mut used = state.used;
        used.set(pool, used.get(pool).checked_add(bytes).ok_or_else(|| error("disk quota extent overflow"))?);
        admit(&state, used, pool)?;
        state.used = used;
        state.reservations = count;
        Ok(DiskReservation { quota: self.clone(), pool, bytes, release: true })
    }
}
/// Private proof that a caller owns this exact shared staging gate. Nested native
/// transfers borrow it rather than acquiring a second gate or allowance.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct StageOperation<'a> {
    quota: &'a DiskQuota,
    _guard: MutexGuard<'a, ()>,
}
#[cfg(not(target_arch = "wasm32"))]
impl StageOperation<'_> {
    pub(crate) fn check(&self, quota: &DiskQuota) -> Result<(), Error> {
        if Arc::ptr_eq(&self.quota.0, &quota.0) && Arc::ptr_eq(&self.quota.1, &quota.1) {
            Ok(())
        } else {
            Err(error("storage transfer belongs to another quota operation"))
        }
    }
}
/// One owned backing file; keep it through the last open reader, including retired files.
pub struct BackingReservation(DiskQuota, bool);
impl Drop for BackingReservation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.0.lock() {
            state.backings = state.backings.saturating_sub(1);
            state.base_backings = state.base_backings.saturating_sub(u64::from(self.1));
        }
    }
}
/// Ownership token held through last close AND successful owned cleanup.
/// Cloning a file lease shares its token; cloning tokens is deliberately unavailable.
pub struct DiskReservation {
    quota: DiskQuota,
    pool: DiskPool,
    bytes: u64,
    release: bool,
}
impl DiskReservation {
    pub fn byte_len(&self) -> u64 {
        self.bytes
    }
    pub fn is_empty(&self) -> bool {
        self.bytes == 0
    }
    pub fn resize(&mut self, bytes: u64) -> Result<(), Error> {
        self.move_to(self.pool, bytes)
    }
    pub fn move_to(&mut self, pool: DiskPool, bytes: u64) -> Result<(), Error> {
        let mut state = self.quota.0.lock().map_err(|_| error("disk quota lock is unavailable"))?;
        let mut used = state.used;
        used.set(self.pool, used.get(self.pool).checked_sub(self.bytes).ok_or_else(|| error("disk quota ownership mismatch"))?);
        used.set(pool, used.get(pool).checked_add(bytes).ok_or_else(|| error("disk quota extent overflow"))?);
        admit(&state, used, pool)?;
        state.used = used;
        self.pool = pool;
        self.bytes = bytes;
        Ok(())
    }
    /// A failed remove must not pretend the bytes were reclaimed. The host retains its
    /// owned-file record for repair; this permanently holds the quota until that repair.
    pub fn retain_after_cleanup_failure(mut self) -> CleanupCharge {
        if let Ok(mut state) = self.quota.0.lock() {
            state.cleanup_retained = state.cleanup_retained.saturating_add(self.bytes);
        }
        self.release = false;
        CleanupCharge { reservation: self }
    }
}
/// Conservative charge retained while an owned file could not be removed.
/// The host may release it only after successfully repairing that cleanup.
pub struct CleanupCharge {
    reservation: DiskReservation,
}
impl CleanupCharge {
    pub fn release_after_cleanup(mut self) -> Result<(), Error> {
        self.try_release_after_cleanup()
    }
    pub(crate) fn try_release_after_cleanup(&mut self) -> Result<(), Error> {
        if self.reservation.release {
            return Ok(());
        }
        {
            let mut state = self.reservation.quota.0.lock().map_err(|_| error("disk quota lock is unavailable"))?;
            state.cleanup_retained = state.cleanup_retained.checked_sub(self.reservation.bytes).ok_or_else(|| error("cleanup quota ownership mismatch"))?;
        }
        self.reservation.release = true;
        Ok(())
    }
}
impl Drop for DiskReservation {
    fn drop(&mut self) {
        if !self.release {
            return;
        }
        if let Ok(mut state) = self.quota.0.lock()
            && let Some(next) = state.used.get(self.pool).checked_sub(self.bytes)
        {
            state.used.set(self.pool, next);
            state.reservations = state.reservations.saturating_sub(1);
        }
    }
}
fn admit(state: &State, used: DiskUsage, pool: DiskPool) -> Result<(), Error> {
    if used.get(pool) > state.limits.get(pool) || used.total()? > state.total {
        Err(error("owned disk quota is full; release readers or choose smaller input"))
    } else {
        Ok(())
    }
}
fn error(message: &'static str) -> Error {
    Error::new(ErrorKind::Conflict, message)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn small() -> DiskQuota {
        DiskQuota::lower(DiskUsage { verified: 10, bases: 10, journals: 20, staging: 10, metadata: 1 }, 40).unwrap()
    }
    #[test]
    fn counts_every_pool_and_transfers_atomically() {
        let quota = small();
        let mut stage = quota.reserve(DiskPool::Staging, 10).unwrap();
        let base = quota.reserve(DiskPool::Bases, 10).unwrap();
        let old = quota.reserve(DiskPool::Journals, 20).unwrap();
        assert_eq!(quota.usage().unwrap().total().unwrap(), 40);
        assert!(quota.reserve(DiskPool::Metadata, 1).is_err());
        stage.move_to(DiskPool::Verified, 10).unwrap();
        assert_eq!(quota.usage().unwrap().staging, 0);
        assert!(stage.resize(11).is_err());
        assert_eq!(stage.byte_len(), 10);
        drop(old);
        let new = quota.reserve(DiskPool::Journals, 20).unwrap();
        assert_eq!(quota.usage().unwrap().journals, 20);
        drop((stage, base, new));
        assert_eq!(quota.usage().unwrap().total().unwrap(), 0);
    }
    #[test]
    fn retired_readers_and_failed_cleanup_keep_their_charge() {
        let quota = small();
        let old = Arc::new(quota.reserve(DiskPool::Journals, 15).unwrap());
        let reader = old.clone();
        drop(old);
        assert!(quota.reserve(DiskPool::Journals, 6).is_err());
        drop(reader);
        let retained = quota.reserve(DiskPool::Journals, 15).unwrap();
        let repair = retained.retain_after_cleanup_failure();
        assert_eq!(quota.cleanup_retained_bytes().unwrap(), 15);
        assert!(quota.reserve(DiskPool::Journals, 6).is_err());
        repair.release_after_cleanup().unwrap();
        assert_eq!(quota.cleanup_retained_bytes().unwrap(), 0);
        assert_eq!(quota.usage().unwrap().journals, 0);
    }
    #[test]
    fn shared_staging_gate_refuses_concurrent_operations_without_locking_accounting() {
        let quota = small();
        let other = quota.clone();
        let stage = quota.try_stage().unwrap();
        assert!(other.try_stage().is_err());
        let reservation = other.reserve(DiskPool::Staging, 1).unwrap();
        assert_eq!(quota.usage().unwrap().staging, 1);
        drop((stage, reservation));
        assert!(other.try_stage().is_ok());
    }
    #[test]
    fn backing_count_is_shared_and_reader_pins_hold_the_slot() {
        let quota = small();
        let mut permits = Vec::new();
        for _ in 0..MAX_BACKINGS {
            permits.push(Arc::new(quota.reserve_backing().unwrap()));
        }
        assert!(quota.clone().reserve_backing().is_err());
        let pin = permits.first().unwrap().clone();
        drop(permits.remove(0));
        assert!(quota.reserve_backing().is_err());
        drop(pin);
        assert!(quota.reserve_backing().is_ok());
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn nested_operation_token_matches_only_its_shared_gate_without_a_second_allowance() {
        let quota = small();
        let shared = quota.clone();
        let other = small();
        let operation = quota.stage_operation().unwrap();
        operation.check(&shared).unwrap();
        assert!(operation.check(&other).is_err());
        assert!(shared.stage_operation().is_err());
        let stage = shared.reserve(DiskPool::Staging, 10).unwrap();
        assert!(shared.reserve(DiskPool::Staging, 1).is_err());
        assert_eq!(quota.usage().unwrap().staging, 10);
        drop((stage, operation));
        assert!(shared.stage_operation().is_ok());
    }
    #[test]
    fn cannot_raise_limits_or_wrap_cumulative_counts() {
        assert!(DiskQuota::lower(DiskUsage::HARD, u64::MAX).is_err());
        let quota = small();
        assert!(quota.reserve(DiskPool::Verified, u64::MAX).is_err());
        assert_eq!(quota.usage().unwrap(), DiskUsage::default());
    }
}

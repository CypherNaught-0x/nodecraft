//! Pre-reserved exact-path cleanup ownership. This module never scans a directory.
//! Files/handles must be closed before their permit is dropped.
use crate::disk_quota::{CleanupCharge, DiskPool, DiskQuota, DiskReservation};
use nodecraft_types::{Error, ErrorKind};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
const MAX_SLOTS: usize = 4096;
const MAX_PATH_BYTES: usize = 64 * 1024;
pub(crate) trait RemoveOwned: Send + Sync {
    fn remove(&self, path: &Path) -> std::io::Result<()>;
}
struct NativeRemove;
impl RemoveOwned for NativeRemove {
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }
}
enum Slot {
    Vacant,
    Reserved,
    Repairing,
    Debt(Debt),
}
struct Debt {
    path: PathBuf,
    charge: CleanupCharge,
    _path_charge: DiskReservation,
}
/// The table owns no workspace/context, so an owned source can retain it without a cycle.
pub(crate) struct CleanupTable {
    slots: Mutex<Vec<Slot>>,
    remove: Arc<dyn RemoveOwned>,
    _metadata: DiskReservation,
}
impl CleanupTable {
    pub(crate) fn new(quota: &DiskQuota) -> Result<Arc<Self>, Error> {
        Self::with_remover(quota, Arc::new(NativeRemove))
    }
    pub(crate) fn with_remover(quota: &DiskQuota, remove: Arc<dyn RemoveOwned>) -> Result<Arc<Self>, Error> {
        // Lowered metadata quotas get a smaller pre-reserved table, leaving half of
        // current metadata capacity for exact path records. Never preallocate maxima.
        let available = quota.remaining(DiskPool::Metadata)? / 2;
        let count = usize::try_from(available / std::mem::size_of::<Slot>() as u64).unwrap_or(MAX_SLOTS).min(MAX_SLOTS);
        Self::with_capacity(quota, count, remove)
    }
    pub(crate) fn with_capacity(quota: &DiskQuota, count: usize, remove: Arc<dyn RemoveOwned>) -> Result<Arc<Self>, Error> {
        if count == 0 || count > MAX_SLOTS {
            return Err(error("invalid cleanup ownership capacity"));
        }
        let bytes =
            count.checked_mul(std::mem::size_of::<Slot>()).and_then(|n| u64::try_from(n).ok()).ok_or_else(|| error("cleanup metadata extent overflow"))?;
        let metadata = quota.reserve(DiskPool::Metadata, bytes)?;
        let mut slots = Vec::new();
        slots.try_reserve_exact(count).map_err(|_| error("cannot allocate bounded cleanup ownership slots"))?;
        slots.resize_with(count, || Slot::Vacant);
        Ok(Arc::new(Self { slots: Mutex::new(slots), remove, _metadata: metadata }))
    }
    /// Reserve exact repair ownership and its path metadata before creating a file.
    pub(crate) fn reserve(self: &Arc<Self>, quota: &DiskQuota, path: PathBuf, reservation: DiskReservation) -> Result<CleanupPermit, Error> {
        let bytes = path.as_os_str().len();
        if bytes == 0 || bytes > MAX_PATH_BYTES {
            return Err(error("private storage path exceeds ownership limit"));
        }
        let path_charge = quota.reserve(DiskPool::Metadata, bytes as u64)?;
        let mut slots = self.slots.lock().map_err(|_| error("cleanup ownership table is unavailable"))?;
        let index = slots
            .iter()
            .position(|slot| matches!(slot, Slot::Vacant))
            .ok_or_else(|| error("cleanup ownership slots are full; repair retained files before retrying"))?;
        *slots.get_mut(index).ok_or_else(|| error("cleanup ownership index mismatch"))? = Slot::Reserved;
        Ok(CleanupPermit {
            table: self.clone(),
            index,
            path: Some(path),
            reservation: Some(reservation),
            path_charge: Some(path_charge),
            created: false,
            finished: false,
        })
    }
    pub(crate) fn cleanup(&self) -> Result<(), Error> {
        let count = self.slots.lock().map_err(|_| error("cleanup ownership table is unavailable"))?.len();
        let mut refused = false;
        for index in 0..count {
            let mut debt = {
                let mut slots = self.slots.lock().map_err(|_| error("cleanup ownership table is unavailable"))?;
                let slot = slots.get_mut(index).ok_or_else(|| error("cleanup ownership index mismatch"))?;
                if !matches!(slot, Slot::Debt(_)) {
                    continue;
                }
                match std::mem::replace(slot, Slot::Repairing) {
                    Slot::Debt(debt) => debt,
                    _ => return Err(error("cleanup ownership state mismatch")),
                }
            };
            // No table or quota accounting mutex is held across filesystem I/O.
            let removed = self.remove.remove(&debt.path).is_ok_or_not_found();
            let mut slots = self.slots.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let slot = slots.get_mut(index).ok_or_else(|| error("cleanup ownership index mismatch"))?;
            if removed {
                // A poisoned quota cannot invent reclaimed capacity; its charge stays retained.
                if let Err(error) = debt.charge.try_release_after_cleanup() {
                    *slot = Slot::Debt(debt);
                    return Err(error);
                }
                *slot = Slot::Vacant;
            } else {
                *slot = Slot::Debt(debt);
                refused = true;
            }
        }
        if refused { Err(error("private file cleanup failed; check permissions and retry cleanup")) } else { Ok(()) }
    }
    #[cfg(test)]
    fn debts(&self) -> usize {
        self.slots.lock().unwrap().iter().filter(|slot| matches!(slot, Slot::Debt(_))).count()
    }
}
trait RemoveResult {
    fn is_ok_or_not_found(&self) -> bool;
}
impl RemoveResult for std::io::Result<()> {
    fn is_ok_or_not_found(&self) -> bool {
        match self {
            Ok(()) => true,
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
    }
}
/// Owns both a vacant bookkeeping slot and all bytes of one exclusively created file.
/// After mark_created, Drop can record failed cleanup by moving into its existing slot.
pub(crate) struct CleanupPermit {
    table: Arc<CleanupTable>,
    index: usize,
    path: Option<PathBuf>,
    reservation: Option<DiskReservation>,
    path_charge: Option<DiskReservation>,
    created: bool,
    finished: bool,
}
impl CleanupPermit {
    pub(crate) fn path(&self) -> Result<&Path, Error> {
        self.path.as_deref().ok_or_else(|| error("private file path ownership is unavailable"))
    }
    pub(crate) fn mark_created(&mut self) {
        self.created = true;
    }
    pub(crate) fn cancel_uncreated(mut self) -> Result<DiskReservation, Error> {
        if self.created {
            return Err(error("cannot cancel ownership of a created file"));
        }
        self.reservation.take().ok_or_else(|| error("private stage reservation is unavailable"))
    }
    pub(crate) fn mark_renamed(&mut self) {
        self.created = false;
    }
    /// Caller closes every handle first. False means exact repair ownership and bytes
    /// remain in the pre-reserved debt slot, so a publication claim cannot be reused.
    pub(crate) fn cleanup_owned(mut self) -> bool {
        self.finish()
    }
    pub(crate) fn replace_reservation(&mut self, next: DiskReservation) {
        drop(self.reservation.replace(next));
    }
    pub(crate) fn resize(&mut self, bytes: u64) -> Result<(), Error> {
        self.reservation.as_mut().ok_or_else(|| error("owned byte reservation is unavailable"))?.resize(bytes)
    }
    fn release_slot(&mut self) {
        let mut slots = self.table.slots.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(slot) = slots.get_mut(self.index) {
            *slot = Slot::Vacant;
        }
    }
}
impl CleanupPermit {
    fn finish(&mut self) -> bool {
        if self.finished {
            return true;
        }
        let removed = !self.created || self.path.as_ref().is_some_and(|path| self.table.remove.remove(path).is_ok_or_not_found());
        if removed {
            self.release_slot();
            self.finished = true;
            return true;
        }
        if let (Some(path), Some(reservation), Some(path_charge)) = (self.path.take(), self.reservation.take(), self.path_charge.take()) {
            let debt = Debt { path, charge: reservation.retain_after_cleanup_failure(), _path_charge: path_charge };
            let mut slots = self.table.slots.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(slot) = slots.get_mut(self.index) {
                *slot = Slot::Debt(debt);
            }
        }
        self.finished = true;
        false
    }
}
impl Drop for CleanupPermit {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
fn error(message: &'static str) -> Error {
    Error::new(ErrorKind::Conflict, message)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct FaultRemove(AtomicU64);
    impl RemoveOwned for FaultRemove {
        fn remove(&self, path: &Path) -> std::io::Result<()> {
            if self.0.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)).is_ok() {
                return Err(std::io::Error::other("injected exact cleanup failure"));
            }
            std::fs::remove_file(path)
        }
    }
    #[test]
    fn reserved_slot_survives_failed_drop_cleanup_without_allocation_and_repairs_exact_path() {
        let dir = tempfile::tempdir().unwrap();
        let quota = DiskQuota::default();
        let fault = Arc::new(FaultRemove(AtomicU64::new(2)));
        let table = CleanupTable::with_capacity(&quota, 1, fault).unwrap();
        let path = dir.path().join("owned");
        let unrelated = dir.path().join("unrelated");
        std::fs::write(&unrelated, b"keep").unwrap();
        let bytes = quota.reserve(DiskPool::Verified, 3).unwrap();
        let mut permit = table.reserve(&quota, path.clone(), bytes).unwrap();
        let file = std::fs::OpenOptions::new().create_new(true).write(true).open(permit.path().unwrap()).unwrap();
        permit.mark_created();
        drop(file);
        std::fs::write(&path, b"abc").unwrap();
        drop(permit);
        assert_eq!(table.debts(), 1);
        assert_eq!(quota.cleanup_retained_bytes().unwrap(), 3);
        assert!(table.reserve(&quota, dir.path().join("next"), quota.reserve(DiskPool::Verified, 1).unwrap()).is_err());
        assert!(table.cleanup().is_err());
        assert!(path.exists());
        assert_eq!(quota.usage().unwrap().verified, 3);
        table.cleanup().unwrap();
        assert!(!path.exists());
        assert_eq!(quota.cleanup_retained_bytes().unwrap(), 0);
        assert_eq!(std::fs::read(unrelated).unwrap(), b"keep");
        assert!(table.reserve(&quota, dir.path().join("retry"), quota.reserve(DiskPool::Verified, 1).unwrap()).is_ok());
    }
    #[test]
    fn uncreated_or_published_file_releases_reserved_slot_without_removing_unowned_destination() {
        let dir = tempfile::tempdir().unwrap();
        let quota = DiskQuota::default();
        let table = CleanupTable::with_capacity(&quota, 1, Arc::new(NativeRemove)).unwrap();
        let path = dir.path().join("not-created");
        drop(table.reserve(&quota, path, quota.reserve(DiskPool::Staging, 9).unwrap()).unwrap());
        assert_eq!(quota.usage().unwrap().staging, 0);
        let path = dir.path().join("published");
        let mut permit = table.reserve(&quota, path.clone(), quota.reserve(DiskPool::Staging, 9).unwrap()).unwrap();
        std::fs::write(&path, b"published").unwrap();
        permit.mark_created();
        let destination = quota.reserve(DiskPool::Journals, 9).unwrap();
        permit.mark_renamed();
        drop(permit);
        assert_eq!(std::fs::read(&path).unwrap(), b"published");
        assert_eq!(quota.usage().unwrap().staging, 0);
        assert_eq!(quota.usage().unwrap().journals, 9);
        drop(destination);
    }
}

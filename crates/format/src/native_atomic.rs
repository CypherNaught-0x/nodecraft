//! Resource ownership around craft-atomic's synchronous replacement protocol.
//! Exact temporary paths are observed at exclusive creation; no directory is swept.
use crate::{
    disk_quota::{BackingReservation, DiskPool, DiskQuota, DiskReservation, StageOperation},
    native_cleanup::{CleanupPermit, CleanupTable},
};
use craft_atomic::{AtomicFs, NewFilePermissions};
use nodecraft_types::{Error, ErrorKind};
use std::{
    cell::RefCell,
    fs::{File, OpenOptions, Permissions},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
/// Readable ownership is prepared before rename. It contains no workspace/context Arc.
/// The file closes before its final backing permit is released.
pub(crate) struct AtomicFile {
    file: Mutex<Option<File>>,
    _backing: BackingReservation,
    published: AtomicBool,
    quota: DiskQuota,
}
impl AtomicFile {
    pub(crate) fn prepare(quota: &DiskQuota) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(Self { file: Mutex::new(None), _backing: quota.reserve_backing()?, published: AtomicBool::new(false), quota: quota.clone() }))
    }
    pub(crate) fn from_existing(quota: &DiskQuota, file: File) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(Self { file: Mutex::new(Some(file)), _backing: quota.reserve_backing()?, published: AtomicBool::new(true), quota: quota.clone() }))
    }
    pub(crate) fn clone_file(&self) -> Result<File, Error> {
        self.with_file(|file| file.try_clone().map_err(|_| error("cannot prepare recovery file pin")))
    }
    pub(crate) fn with_file<T>(&self, operation: impl FnOnce(&mut File) -> Result<T, Error>) -> Result<T, Error> {
        if !self.published.load(Ordering::Acquire) {
            return Err(error("recovery file has not been published"));
        }
        let mut file = self.file.lock().map_err(|_| error("recovery file handle is unavailable"))?;
        operation(file.as_mut().ok_or_else(|| error("recovery file handle is closed"))?)
    }
    fn close_unpublished(&self) {
        if !self.published.load(Ordering::Acquire) {
            drop(self.file.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take());
        }
    }
}
pub(crate) struct AtomicFailure {
    pub error: Error,
    pub cleanup_complete: bool,
}
struct State {
    reservation: Option<DiskReservation>,
    permit: Option<CleanupPermit>,
    cleanup_complete: bool,
}
struct Observer<'a> {
    quota: &'a DiskQuota,
    table: &'a Arc<CleanupTable>,
    ready: &'a Arc<AtomicFile>,
    state: RefCell<State>,
    backend: &'a dyn AtomicFs,
}
/// Preparation and future destination ownership must precede this call. The result means
/// actual rename publication; directory sync failure retains craft-atomic's success semantics.
#[allow(clippy::too_many_arguments)]
pub(crate) fn publish(
    quota: &DiskQuota,
    table: &Arc<CleanupTable>,
    operation: &StageOperation<'_>,
    path: &Path,
    length: u64,
    private: bool,
    ready: &Arc<AtomicFile>,
    write: impl FnOnce(&mut dyn io::Write) -> Result<(), Error>,
) -> Result<(), AtomicFailure> {
    publish_with(quota, table, operation, path, length, private, ready, &ReadWriteFs, write)
}
#[allow(clippy::too_many_arguments)]
fn publish_with(
    quota: &DiskQuota,
    table: &Arc<CleanupTable>,
    operation: &StageOperation<'_>,
    path: &Path,
    length: u64,
    private: bool,
    ready: &Arc<AtomicFile>,
    backend: &dyn AtomicFs,
    write: impl FnOnce(&mut dyn io::Write) -> Result<(), Error>,
) -> Result<(), AtomicFailure> {
    let clean_error = |error| AtomicFailure { error, cleanup_complete: true };
    operation.check(quota).map_err(clean_error)?;
    operation.check(&ready.quota).map_err(clean_error)?;
    if ready.published.load(Ordering::Acquire) {
        return Err(clean_error(error("atomic file publication is single-use")));
    }
    let reservation = quota.reserve(DiskPool::Staging, length).map_err(clean_error)?;
    let observer =
        Observer { quota, table, ready, state: RefCell::new(State { reservation: Some(reservation), permit: None, cleanup_complete: true }), backend };
    let mut write_error = None;
    let options = craft_atomic::WriteOptions::default().with_new_file_permissions(if private {
        NewFilePermissions::Private
    } else {
        NewFilePermissions::PlatformDefault
    });
    let result = options.atomic_write_with_custom(&observer, craft_atomic::RenameRetry::platform(), path, |file| {
        let mut bounded = LimitedWriter { file, left: length };
        let written = write(&mut bounded).and_then(|()| if bounded.left == 0 { Ok(()) } else { Err(error("atomic output did not fill its admitted extent")) });
        written.map_err(|error| {
            write_error = Some(error);
            io::Error::other("bounded atomic output failed")
        })
    });
    if ready.published.load(Ordering::Acquire) {
        // The exact destination already has a prepared owner. Releasing the stage below
        // performs no I/O or allocation and cannot turn a published Save into an error.
        drop(observer.state.into_inner());
        return Ok(());
    }
    ready.close_unpublished();
    let mut state = observer.state.into_inner();
    if let Some(permit) = state.permit.take() {
        state.cleanup_complete &= permit.cleanup_owned();
    }
    let _ = result;
    Err(AtomicFailure {
        error: write_error.unwrap_or_else(|| error("cannot publish file; check permissions and free space")),
        cleanup_complete: state.cleanup_complete,
    })
}
impl AtomicFs for Observer<'_> {
    fn create_new(&self, path: &Path, permissions: NewFilePermissions) -> io::Result<File> {
        let mut state = self.state.try_borrow_mut().map_err(|_| io::Error::other("atomic ownership is already borrowed"))?;
        if state.permit.is_some() {
            return Err(io::Error::other("atomic output already owns a temporary"));
        }
        let reservation = state.reservation.take().ok_or_else(|| io::Error::other("atomic stage reservation is unavailable"))?;
        let mut owned = PathBuf::new();
        owned.try_reserve(path.as_os_str().len()).map_err(|_| io::Error::other("cannot allocate bounded atomic path"))?;
        owned.push(path);
        let mut permit = self.table.reserve(self.quota, owned, reservation).map_err(|_| io::Error::other("cannot reserve atomic cleanup ownership"))?;
        let file = match self.backend.create_new(path, permissions) {
            Ok(file) => file,
            Err(error) => {
                state.reservation = Some(permit.cancel_uncreated().map_err(|_| io::Error::other("cannot restore uncreated stage ownership"))?);
                return Err(error);
            }
        };
        permit.mark_created();
        match file.try_clone() {
            Ok(readable) => {
                let mut slot = match self.ready.file.lock() {
                    Ok(slot) => slot,
                    Err(_) => {
                        drop(readable);
                        drop(file);
                        state.cleanup_complete &= permit.cleanup_owned();
                        return Err(io::Error::other("readable atomic handle is unavailable"));
                    }
                };
                if slot.is_some() {
                    drop(slot);
                    drop(readable);
                    drop(file);
                    state.cleanup_complete &= permit.cleanup_owned();
                    return Err(io::Error::other("readable atomic handle is already prepared"));
                }
                *slot = Some(readable);
                state.permit = Some(permit);
                Ok(file)
            }
            Err(error) => {
                drop(file);
                state.cleanup_complete &= permit.cleanup_owned();
                Err(error)
            }
        }
    }
    fn sync(&self, file: &File) -> io::Result<()> {
        self.backend.sync(file)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut state = self.state.try_borrow_mut().map_err(|_| io::Error::other("atomic ownership is already borrowed"))?;
        let permit = state.permit.as_mut().ok_or_else(|| io::Error::other("atomic output owns no temporary"))?;
        if permit.path().map_err(|_| io::Error::other("atomic path ownership is unavailable"))? != from {
            return Err(io::Error::other("atomic rename is outside owned temporary"));
        }
        if self.ready.file.lock().map_err(|_| io::Error::other("atomic readable handle is unavailable"))?.is_none() {
            return Err(io::Error::other("atomic readable handle was not prepared"));
        }
        self.backend.rename(from, to)?;
        permit.mark_renamed();
        self.ready.published.store(true, Ordering::Release);
        Ok(())
    }
    fn hard_link(&self, _: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::other("atomic replacement does not create hard links"))
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        self.ready.close_unpublished();
        let mut state = self.state.try_borrow_mut().map_err(|_| io::Error::other("atomic ownership is already borrowed"))?;
        let permit = state.permit.take().ok_or_else(|| io::Error::other("atomic output owns no temporary to remove"))?;
        if permit.path().map_err(|_| io::Error::other("atomic path ownership is unavailable"))? != path {
            state.permit = Some(permit);
            return Err(io::Error::other("atomic removal is outside owned temporary"));
        }
        let removed = permit.cleanup_owned();
        state.cleanup_complete &= removed;
        if removed { Ok(()) } else { Err(io::Error::other("atomic temporary cleanup requires repair")) }
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.backend.sync_dir(path)
    }
    fn set_permissions(&self, file: &File, permissions: Permissions) -> io::Result<()> {
        self.backend.set_permissions(file, permissions)
    }
}
/// The sole creation difference from RealFs is requesting a readable handle before
/// publication. Permissions, exclusive create, sync/rename policy remain unchanged.
struct ReadWriteFs;
impl AtomicFs for ReadWriteFs {
    fn create_new(&self, path: &Path, permissions: NewFilePermissions) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(match permissions {
                NewFilePermissions::Private => 0o600,
                NewFilePermissions::PlatformDefault => 0o666,
                _ => return Err(io::Error::other("unsupported atomic permission policy")),
            });
        }
        #[cfg(not(unix))]
        let _ = permissions;
        options.open(path)
    }
    fn sync(&self, file: &File) -> io::Result<()> {
        craft_atomic::RealFs.sync(file)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        craft_atomic::RealFs.rename(from, to)
    }
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        craft_atomic::RealFs.hard_link(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        craft_atomic::RealFs.remove(path)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        craft_atomic::RealFs.sync_dir(path)
    }
    fn set_permissions(&self, file: &File, permissions: Permissions) -> io::Result<()> {
        craft_atomic::RealFs.set_permissions(file, permissions)
    }
}
struct LimitedWriter<'a> {
    file: &'a mut File,
    left: u64,
}
impl io::Write for LimitedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.left {
            return Err(io::Error::other("atomic output exceeds its reserved extent"));
        }
        let n = io::Write::write(self.file, bytes)?;
        self.left = self.left.checked_sub(n as u64).ok_or_else(|| io::Error::other("atomic output extent overflow"))?;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        io::Write::flush(self.file)
    }
}
fn error(message: &'static str) -> Error {
    Error::new(ErrorKind::Conflict, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};
    use std::sync::atomic::AtomicU64;
    struct Backend {
        fail_sync: bool,
        fail_rename: bool,
        fail_dir: bool,
        collisions: AtomicU64,
    }
    impl AtomicFs for Backend {
        fn create_new(&self, path: &Path, permissions: NewFilePermissions) -> io::Result<File> {
            if self.collisions.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)).is_ok() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            ReadWriteFs.create_new(path, permissions)
        }
        fn sync(&self, file: &File) -> io::Result<()> {
            if self.fail_sync { Err(io::Error::other("injected prepublication sync failure")) } else { ReadWriteFs.sync(file) }
        }
        fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
            if self.fail_rename { Err(io::Error::other("injected prepublication rename failure")) } else { ReadWriteFs.rename(a, b) }
        }
        fn hard_link(&self, a: &Path, b: &Path) -> io::Result<()> {
            ReadWriteFs.hard_link(a, b)
        }
        fn remove(&self, path: &Path) -> io::Result<()> {
            ReadWriteFs.remove(path)
        }
        fn sync_dir(&self, path: &Path) -> io::Result<()> {
            if self.fail_dir { Err(io::Error::other("injected postpublication directory sync failure")) } else { ReadWriteFs.sync_dir(path) }
        }
        fn set_permissions(&self, file: &File, permissions: Permissions) -> io::Result<()> {
            ReadWriteFs.set_permissions(file, permissions)
        }
    }
    fn backend() -> Backend {
        Backend { fail_sync: false, fail_rename: false, fail_dir: false, collisions: AtomicU64::new(0) }
    }
    struct RemoveFault(AtomicU64);
    impl crate::native_cleanup::RemoveOwned for RemoveFault {
        fn remove(&self, path: &Path) -> io::Result<()> {
            if self.0.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)).is_ok() {
                return Err(io::Error::other("injected owned cleanup failure"));
            }
            std::fs::remove_file(path)
        }
    }
    #[test]
    fn readable_owner_exists_before_rename_and_survives_source_path_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("destination");
        std::fs::write(&path, b"old").unwrap();
        let quota = DiskQuota::default();
        let table = CleanupTable::new(&quota).unwrap();
        let baseline = quota.usage().unwrap();
        let ready = AtomicFile::prepare(&quota).unwrap();
        let operation = quota.stage_operation().unwrap();
        let mut fs = backend();
        fs.fail_dir = true;
        publish_with(&quota, &table, &operation, &path, 3, true, &ready, &fs, |file| file.write_all(b"new").map_err(|_| error("test write")))
            .map_err(|failure| failure.error)
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(quota.usage().unwrap(), baseline);
        std::fs::remove_file(&path).unwrap();
        let bytes = ready
            .with_file(|file| {
                file.seek(SeekFrom::Start(0)).unwrap();
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).unwrap();
                Ok(bytes)
            })
            .unwrap();
        assert_eq!(bytes, b"new");
        assert!(publish(&quota, &table, &operation, &path, 3, true, &ready, |file| file.write_all(b"bad").map_err(|_| error("test write"))).is_err());
        assert!(!path.exists());
        drop(ready);
        assert_eq!(quota.backing_count().unwrap(), 0);
    }
    #[test]
    fn failed_write_sync_or_rename_preserves_destination_and_reclaims_exact_stage() {
        for mode in 0..4 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("destination");
            std::fs::write(&path, b"old").unwrap();
            let quota = DiskQuota::default();
            let table = CleanupTable::new(&quota).unwrap();
            let baseline = quota.usage().unwrap();
            let ready = AtomicFile::prepare(&quota).unwrap();
            let operation = quota.stage_operation().unwrap();
            let mut fs = backend();
            fs.fail_sync = mode == 0;
            fs.fail_rename = mode == 1;
            let result = publish_with(&quota, &table, &operation, &path, 3, true, &ready, &fs, |file| {
                let bytes = if mode == 2 {
                    b"too long".as_slice()
                } else if mode == 3 {
                    b"n".as_slice()
                } else {
                    b"new".as_slice()
                };
                file.write_all(bytes).map_err(|_| error("test write"))
            });
            let failure = result.err().unwrap();
            assert!(failure.cleanup_complete);
            assert_eq!(std::fs::read(&path).unwrap(), b"old");
            assert_eq!(quota.usage().unwrap(), baseline);
            assert!(!ready.published.load(Ordering::Acquire));
            assert!(ready.file.lock().unwrap().is_none());
            // A fully cleaned failed attempt can reuse the pre-reserved readable backing.
            publish_with(&quota, &table, &operation, &path, 3, true, &ready, &backend(), |file| file.write_all(b"new").map_err(|_| error("test write")))
                .map_err(|failure| failure.error)
                .unwrap();
            assert_eq!(std::fs::read(path).unwrap(), b"new");
        }
    }
    #[test]
    fn failed_temp_removal_is_not_a_reusable_clean_claim_and_debt_repairs_only_owned_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("destination");
        let unrelated = dir.path().join("unrelated");
        std::fs::write(&path, b"old").unwrap();
        std::fs::write(&unrelated, b"keep").unwrap();
        let quota = DiskQuota::default();
        let table = CleanupTable::with_remover(&quota, Arc::new(RemoveFault(AtomicU64::new(1)))).unwrap();
        let baseline = quota.usage().unwrap();
        let ready = AtomicFile::prepare(&quota).unwrap();
        let operation = quota.stage_operation().unwrap();
        let mut fs = backend();
        fs.fail_rename = true;
        let failure = publish_with(&quota, &table, &operation, &path, 3, true, &ready, &fs, |file| file.write_all(b"new").map_err(|_| error("test write")))
            .err()
            .unwrap();
        assert!(!failure.cleanup_complete);
        assert_eq!(quota.usage().unwrap().staging, 3);
        assert_eq!(quota.cleanup_retained_bytes().unwrap(), 3);
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert!(ready.file.lock().unwrap().is_none());
        table.cleanup().unwrap();
        assert_eq!(quota.usage().unwrap(), baseline);
        assert_eq!(std::fs::read(unrelated).unwrap(), b"keep");
    }
    #[test]
    fn exclusive_create_collisions_restore_the_same_reservation_without_spending_another_slot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("destination");
        let quota = DiskQuota::default();
        let table = CleanupTable::new(&quota).unwrap();
        let baseline = quota.usage().unwrap();
        let ready = AtomicFile::prepare(&quota).unwrap();
        let operation = quota.stage_operation().unwrap();
        let fs = backend();
        fs.collisions.store(7, Ordering::Relaxed);
        publish_with(&quota, &table, &operation, &path, 3, true, &ready, &fs, |file| file.write_all(b"new").map_err(|_| error("test write")))
            .map_err(|failure| failure.error)
            .unwrap();
        assert_eq!(quota.usage().unwrap(), baseline);
        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }
}

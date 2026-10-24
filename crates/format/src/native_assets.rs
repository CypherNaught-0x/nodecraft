//! Private immutable disk backings. No archive-provided filename is ever opened or removed here.
use crate::asset_source::{
    AssetRange, AssetSource, MAX_LARGE_ARCHIVE_BYTES, MAX_LARGE_ASSET_BYTES, MAX_LARGE_TOTAL_ASSET_BYTES, StreamDigest, StreamMetrics, TransferCancel,
    copy_and_hash,
};
use crate::disk_quota::{BackingReservation, DiskPool, DiskQuota, DiskReservation, StageOperation};
use crate::native_cleanup::{CleanupPermit, CleanupTable};
use nodecraft_types::{Error, ErrorKind};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn next_id() -> Result<u64, Error> {
    NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1)).map_err(|_| error("private asset identifier limit reached"))
}
struct Workspace {
    root: PathBuf,
    lifetime: crate::native_workspace::LifetimeLock,
    maintenance: crate::native_workspace::SweepReport,
    quota: DiskQuota,
    cleanup: Arc<CleanupTable>,
    journal: Mutex<Option<Arc<crate::storage::native::durability::SharedJournal>>>,
    #[cfg(test)]
    write_fault: Mutex<Option<u64>>,
    #[cfg(test)]
    remove_fault: Arc<AtomicU64>,
    #[cfg(test)]
    capture_sync_fault: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    atomic_fault: Mutex<Option<u64>>,
    #[cfg(test)]
    atomic_written: AtomicU64,
}
impl Drop for Workspace {
    fn drop(&mut self) {
        self.lifetime.retire_empty(&self.root);
    }
}
/// Owns a private directory and its exact disk reservations. Does not own source/user paths.
#[derive(Clone)]
pub struct NativeAssetWorkspace(Arc<Workspace>);
impl NativeAssetWorkspace {
    pub fn new(parent: &Path, quota: DiskQuota) -> Result<Self, Error> {
        #[cfg(test)]
        let remove_fault = Arc::new(AtomicU64::new(0));
        #[cfg(test)]
        let cleanup = CleanupTable::with_remover(&quota, Arc::new(RemoveFault(remove_fault.clone())))?;
        #[cfg(not(test))]
        let cleanup = CleanupTable::new(&quota)?;
        let created = crate::native_workspace::create(parent, next_id)?;
        Ok(Self(Arc::new(Workspace {
            root: created.root,
            lifetime: created.lock,
            maintenance: created.report,
            quota,
            cleanup,
            journal: Mutex::new(None),
            #[cfg(test)]
            write_fault: Mutex::new(None),
            #[cfg(test)]
            remove_fault,
            #[cfg(test)]
            capture_sync_fault: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            atomic_fault: Mutex::new(None),
            #[cfg(test)]
            atomic_written: AtomicU64::new(0),
        })))
    }
    /// Startup maintenance is bounded; this report never implies all parent entries were scanned.
    pub fn maintenance_report(&self) -> crate::native_workspace::SweepReport {
        self.0.maintenance
    }
    pub fn quota(&self) -> &DiskQuota {
        &self.0.quota
    }
    pub(crate) fn install_journal(&self, owner: &Arc<crate::storage::native::durability::SharedJournal>) -> Result<(), Error> {
        let mut slot = self.0.journal.lock().map_err(|_| error("recovery owner slot is unavailable"))?;
        if slot.is_some() {
            return Err(error("asset workspace already belongs to a recovery owner"));
        }
        *slot = Some(owner.clone());
        Ok(())
    }
    pub(crate) fn journal(&self) -> Result<Arc<crate::storage::native::durability::SharedJournal>, Error> {
        self.0.journal.lock().map_err(|_| error("recovery owner slot is unavailable"))?.clone().ok_or_else(|| error("native workspace has no recovery host"))
    }
    pub(crate) fn remove_journal(&self, owner: &Arc<crate::storage::native::durability::SharedJournal>) {
        let mut slot = self.0.journal.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, owner)) {
            *slot = None;
        }
    }
    #[cfg(test)]
    pub(crate) fn fail_next_write_after(&self, bytes: u64) -> Result<(), Error> {
        *self.0.write_fault.lock().map_err(|_| error("test write fault lock is unavailable"))? = Some(bytes);
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn fail_removals(&self, count: u64) {
        self.0.remove_fault.store(count, Ordering::Relaxed);
    }
    #[cfg(test)]
    pub(crate) fn fail_next_capture_sync(&self) {
        self.0.capture_sync_fault.store(true, Ordering::Relaxed);
    }
    #[cfg(test)]
    pub(crate) fn test_cleanup_capacity(&mut self, count: usize) -> Result<(), Error> {
        let workspace = Arc::get_mut(&mut self.0).ok_or_else(|| error("test cleanup capacity requires an unshared workspace"))?;
        let cleanup = CleanupTable::with_capacity(&workspace.quota, count, Arc::new(RemoveFault(workspace.remove_fault.clone())))?;
        workspace.cleanup = cleanup;
        Ok(())
    }
    /// Test-only fault at the actual atomic writer, separate from capture/copy faults.
    #[cfg(test)]
    pub(crate) fn fail_next_atomic_after(&self, bytes: u64) -> Result<(), Error> {
        self.0.atomic_written.store(0, Ordering::Relaxed);
        *self.0.atomic_fault.lock().map_err(|_| error("test atomic fault lock is unavailable"))? = Some(bytes);
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn atomic_written(&self) -> u64 {
        self.0.atomic_written.load(Ordering::Relaxed)
    }
    fn copy_stage(&self, source: &AssetRange, file: &mut File, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<StreamDigest, Error> {
        #[cfg(test)]
        {
            if let Some(left) = self.0.write_fault.lock().map_err(|_| error("test write fault lock is unavailable"))?.take() {
                return copy_and_hash(source, &mut WriteFault { file, left }, cancel, metrics);
            }
        }
        copy_and_hash(source, file, cancel, metrics)
    }
    /// Retry only removals of exact files this instance exclusively created.
    pub fn cleanup(&self) -> Result<(), Error> {
        self.0.cleanup.cleanup()?;
        if self.0.quota.cleanup_retained_bytes()? > 0 {
            return Err(error("owned storage cleanup debt remains; retry its owner cleanup"));
        }
        Ok(())
    }
    /// Serialize output under this workspace's one staging allowance. The readable
    /// owner and all cleanup bookkeeping exist before craft-atomic can rename.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn publish_file(
        &self,
        operation: &StageOperation<'_>,
        path: &Path,
        length: u64,
        private: bool,
        ready: &Arc<crate::native_atomic::AtomicFile>,
        write: impl FnOnce(&mut dyn std::io::Write) -> Result<(), Error>,
    ) -> Result<(), crate::native_atomic::AtomicFailure> {
        operation.check(&self.0.quota).and_then(|()| self.cleanup()).map_err(|error| crate::native_atomic::AtomicFailure { error, cleanup_complete: true })?;
        #[cfg(test)]
        {
            // Consume configuration before I/O. Never hold the fault mutex across a write.
            let fault = self
                .0
                .atomic_fault
                .lock()
                .map_err(|_| crate::native_atomic::AtomicFailure { error: error("test atomic fault lock is unavailable"), cleanup_complete: true })?
                .take();
            if let Some(limit) = fault {
                return crate::native_atomic::publish(&self.0.quota, &self.0.cleanup, operation, path, length, private, ready, |writer| {
                    write(&mut AtomicWriteFault { writer, limit, written: &self.0.atomic_written })
                });
            }
        }
        crate::native_atomic::publish(&self.0.quota, &self.0.cleanup, operation, path, length, private, ready, write)
    }
    /// Freeze a source into owned bytes before granting an immutable lease. Hashes describe
    /// the exact staged content; optional expected digest/CRC are checked before publication.
    pub fn seal_copy(
        &self,
        source: &AssetRange,
        pool: DiskPool,
        expected: Option<StreamDigest>,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
    ) -> Result<SealedBytes, Error> {
        let cap = match pool {
            DiskPool::Bases => MAX_LARGE_ARCHIVE_BYTES.checked_add(80).ok_or_else(|| error("checkpoint extent overflow"))?,
            DiskPool::Verified => MAX_LARGE_ASSET_BYTES,
            _ => return Err(error("private immutable bytes require an asset or checkpoint pool")),
        };
        self.seal_bounded(source, CopySpec { pool, cap, expected, mismatch: CopyMismatch::CallerExpectation }, cancel, metrics)
    }
    // Recovery supplies stored descriptors under its operation token. A completed proof
    // mismatch is corrupt input; interrupted transfers and association errors are not.
    pub(crate) fn seal_copy_in(
        &self,
        operation: &StageOperation<'_>,
        source: &AssetRange,
        pool: DiskPool,
        expected: Option<StreamDigest>,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
    ) -> Result<SealedBytes, Error> {
        let cap = match pool {
            DiskPool::Bases => MAX_LARGE_ARCHIVE_BYTES.checked_add(80).ok_or_else(|| error("checkpoint extent overflow"))?,
            DiskPool::Verified => MAX_LARGE_ASSET_BYTES,
            _ => return Err(error("invalid immutable copy pool")),
        };
        self.seal_bounded_in(operation, source, CopySpec { pool, cap, expected, mismatch: CopyMismatch::StoredContent }, cancel, metrics)
    }
    /// Compact verified logical blobs without keeping one open file per asset. The store
    /// independently verifies every resulting range before publishing cache relocation.
    pub(crate) fn seal_pack(&self, ranges: &[AssetRange], cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<SealedBytes, Error> {
        let operation = self.0.quota.stage_operation()?;
        self.seal_pack_in(&operation, ranges, cancel, metrics)
    }
    pub(crate) fn seal_pack_in(
        &self,
        operation: &StageOperation<'_>,
        ranges: &[AssetRange],
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
    ) -> Result<SealedBytes, Error> {
        if ranges.is_empty() || ranges.len() > crate::MAX_ASSETS {
            return Err(error("invalid asset pack count"));
        }
        let mut parts = Vec::new();
        parts.try_reserve_exact(ranges.len()).map_err(|_| error("cannot allocate bounded pack index"))?;
        let mut length = 0u64;
        for range in ranges {
            parts.push((length, range.clone()));
            length = length
                .checked_add(range.byte_len())
                .filter(|n| *n <= MAX_LARGE_TOTAL_ASSET_BYTES)
                .ok_or_else(|| error("asset pack exceeds logical size limit"))?;
        }
        let source = AssetRange::new(Arc::new(ConcatSource { parts, length, inner_metrics: StreamMetrics::default() }), 0, length)?;
        self.seal_bounded_in(
            operation,
            &source,
            CopySpec { pool: DiskPool::Verified, cap: MAX_LARGE_TOTAL_ASSET_BYTES, expected: None, mismatch: CopyMismatch::CallerExpectation },
            cancel,
            metrics,
        )
    }
    /// Capture only newly introduced frame bodies into one unpublished backing. Both
    /// stage and future verified ownership grow before their corresponding writes.
    pub(crate) fn capture(&self, operation: &StageOperation<'_>) -> Result<AssetCapture, Error> {
        operation.check(&self.0.quota)?;
        self.cleanup()?;
        let backing = self.0.quota.reserve_backing()?;
        let reservation = self.0.quota.reserve(DiskPool::Staging, 0)?;
        let verified = self.0.quota.reserve(DiskPool::Verified, 0)?;
        let path = self.0.root.join(format!("capture-{}", next_id()?));
        let mut ownership = self.0.cleanup.reserve(&self.0.quota, path, reservation)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(ownership.path()?).map_err(|_| error("cannot create recovery body capture"))?;
        ownership.mark_created();
        Ok(AssetCapture {
            workspace: self.clone(),
            pending: Pending { file: Some(file), ownership: Some(ownership), backing: Some(backing) },
            verified,
            reserved: 0,
            written: 0,
            hash: blake3::Hasher::new(),
            crc: crc32fast::Hasher::new(),
            #[cfg(test)]
            write_left: self.0.write_fault.lock().map_err(|_| error("test write fault lock is unavailable"))?.take(),
        })
    }
    pub(crate) fn input_range(file: File, length: u64) -> Result<AssetRange, Error> {
        AssetRange::new(Arc::new(InputFile { file: Mutex::new(file), length }), 0, length)
    }
    fn seal_bounded(&self, source: &AssetRange, spec: CopySpec, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<SealedBytes, Error> {
        let operation = self.0.quota.stage_operation()?;
        self.seal_bounded_in(&operation, source, spec, cancel, metrics)
    }
    fn seal_bounded_in(
        &self,
        operation: &StageOperation<'_>,
        source: &AssetRange,
        spec: CopySpec,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
    ) -> Result<SealedBytes, Error> {
        operation.check(&self.0.quota)?;
        let CopySpec { pool, cap, expected, mismatch } = spec;
        self.cleanup()?;
        cancel.check()?;
        if source.byte_len() > cap {
            return Err(error("private asset exceeds its source limit"));
        }
        if expected.is_some_and(|d| d.bytes != source.byte_len()) {
            return Err(mismatch.error("asset length does not match its expected identity"));
        }
        let backing = self.0.quota.reserve_backing_for(pool)?;
        let reservation = self.0.quota.reserve(DiskPool::Staging, source.byte_len())?;
        let destination = self.0.quota.reserve(pool, source.byte_len())?;
        // Reserve both in-progress stage and destination capacity before I/O. The future
        // destination reservation remains held, so concurrent work cannot steal it.
        let path = self.0.root.join(format!("stage-{}", next_id()?));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut ownership = self.0.cleanup.reserve(&self.0.quota, path, reservation)?;
        let file = options.open(ownership.path()?).map_err(|_| error("cannot create private asset stage"))?;
        ownership.mark_created();
        let mut pending = Pending { file: Some(file), ownership: Some(ownership), backing: Some(backing) };
        let digest = self.copy_stage(source, pending.file.as_mut().ok_or_else(|| error("private asset stage is unavailable"))?, cancel, metrics)?;
        if expected.is_some_and(|e| e != digest) {
            return Err(mismatch.error("asset content does not match its expected hash or checksum"));
        }
        pending.file.as_ref().ok_or_else(|| error("private asset stage is unavailable"))?.sync_all().map_err(|_| error("cannot sync private asset stage"))?;
        cancel.check()?;
        pending.ownership.as_mut().ok_or_else(|| error("private asset cleanup ownership is unavailable"))?.replace_reservation(destination);
        let file = pending.file.take().ok_or_else(|| error("private asset stage is unavailable"))?;
        let source = Arc::new(OwnedSource {
            file: Mutex::new(Some(file)),
            length: digest.bytes,
            _workspace: self.0.clone(),
            ownership: pending.ownership.take(),
            _backing: pending.backing.take(),
        });
        Ok(SealedBytes { source, digest })
    }
    /// Prepare one immutable archive before destination/recovery publication. Actual stage
    /// and future base ownership are both reserved before I/O; promotion releases staging.
    pub(crate) fn generate_archive(
        &self,
        length: u64,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
        write: impl FnOnce(&mut dyn crate::asset_source::WriteSeek) -> Result<(), Error>,
    ) -> Result<SealedBytes, Error> {
        let _stage = self.0.quota.try_stage()?;
        self.cleanup()?;
        cancel.check()?;
        if length > MAX_LARGE_ARCHIVE_BYTES {
            return Err(error("prepared archive exceeds 65 GiB limit"));
        }
        let backing = self.0.quota.reserve_backing_for(DiskPool::Bases)?;
        let reservation = self.0.quota.reserve(DiskPool::Staging, length)?;
        let destination = self.0.quota.reserve(DiskPool::Bases, length)?;
        let path = self.0.root.join(format!("archive-{}", next_id()?));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut ownership = self.0.cleanup.reserve(&self.0.quota, path, reservation)?;
        let file = options.open(ownership.path()?).map_err(|_| error("cannot create private archive stage"))?;
        ownership.mark_created();
        let mut pending = Pending { file: Some(file), ownership: Some(ownership), backing: Some(backing) };
        let file = pending.file.as_mut().ok_or_else(|| error("private archive stage is unavailable"))?;
        write(&mut BoundedFile { file, position: 0, limit: length })?;
        if file.metadata().map_err(|_| error("cannot inspect prepared archive"))?.len() != length {
            return Err(error("prepared archive length disagrees with its plan"));
        }
        // A sequential hash pass is the sole cursor user after the writer returns. This
        // clone does NOT provide independent cursors: the original handle is not read or
        // written concurrently, and all later OwnedSource reads explicitly seek under lock.
        let hash_handle = file.try_clone().map_err(|_| error("cannot prepare readable archive lease"))?;
        let range = AssetRange::new(Arc::new(InputFile { file: Mutex::new(hash_handle), length }), 0, length)?;
        let digest = copy_and_hash(&range, &mut std::io::sink(), cancel, metrics)?;
        drop(range);
        file.sync_all().map_err(|_| error("cannot sync prepared archive"))?;
        cancel.check()?;
        pending.ownership.as_mut().ok_or_else(|| error("private archive cleanup ownership is unavailable"))?.replace_reservation(destination);
        let source = Arc::new(OwnedSource {
            file: Mutex::new(Some(pending.file.take().ok_or_else(|| error("private archive ownership is unavailable"))?)),
            length,
            _workspace: self.0.clone(),
            ownership: pending.ownership.take(),
            _backing: pending.backing.take(),
        });
        Ok(SealedBytes { source, digest })
    }
    /// Admit metadata before transfer, then independently admit the retained private copy.
    /// Only the latter index escapes; mutable original ranges are never installed in a store.
    pub(crate) fn freeze_archive(
        &self,
        file: File,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
    ) -> Result<(SealedBytes, crate::zip_index::ArchiveIndex), Error> {
        let meta = file.metadata().map_err(|_| error("cannot inspect project source"))?;
        if !meta.is_file() || meta.len() > MAX_LARGE_ARCHIVE_BYTES {
            return Err(error("project source must be a bounded regular file"));
        }
        let input = Arc::new(InputFile { file: Mutex::new(file), length: meta.len() });
        let range = AssetRange::new(input.clone(), 0, meta.len())?;
        let admitted = crate::zip_index::ArchiveIndex::read(&range, cancel, metrics)?;
        let sealed = self.seal_copy(&range, DiskPool::Bases, None, cancel, metrics)?;
        if input.file.lock().map_err(|_| error("project source lock is unavailable"))?.metadata().map_err(|_| error("cannot reinspect project source"))?.len()
            != meta.len()
        {
            return Err(error("project source extent changed during snapshot transfer; retry Open"));
        }
        let retained = crate::zip_index::ArchiveIndex::read(&sealed.range()?, cancel, metrics)?;
        if admitted.identity() != retained.identity() {
            return Err(error("project metadata changed during snapshot transfer; retry Open"));
        }
        Ok((sealed, retained))
    }
    /// File handles are not immutable merely because they remain open. Copy into this
    /// private workspace before releasing the original input or installing snapshot state.
    pub fn snapshot_file(&self, file: File, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<SealedBytes, Error> {
        let meta = file.metadata().map_err(|_| error("cannot inspect project source"))?;
        if !meta.is_file() || meta.len() > MAX_LARGE_ARCHIVE_BYTES {
            return Err(error("project source must be a bounded regular file"));
        }
        let range = AssetRange::new(Arc::new(InputFile { file: Mutex::new(file), length: meta.len() }), 0, meta.len())?;
        self.seal_copy(&range, DiskPool::Bases, None, cancel, metrics)
    }
}
pub(crate) struct AssetCapture {
    // Declaration-order destruction must close payload/permit BEFORE the final lifetime Arc.
    pending: Pending,
    workspace: NativeAssetWorkspace,
    verified: DiskReservation,
    reserved: u64,
    written: u64,
    hash: blake3::Hasher,
    crc: crc32fast::Hasher,
    #[cfg(test)]
    write_left: Option<u64>,
}
impl AssetCapture {
    pub(crate) fn reserve_asset(&mut self, length: u64) -> Result<u64, Error> {
        if length > MAX_LARGE_ASSET_BYTES || self.written != self.reserved {
            return Err(error("invalid recovery capture extent"));
        }
        let total =
            self.reserved.checked_add(length).filter(|n| *n <= MAX_LARGE_TOTAL_ASSET_BYTES).ok_or_else(|| error("recovery captured assets exceed 64 GiB"))?;
        // A second reservation may fail; keep the larger first reservation until
        // this unpublished capture drops, rather than pretending any file shrank.
        self.verified.resize(total)?;
        self.pending.ownership.as_mut().ok_or_else(|| error("capture ownership is unavailable"))?.resize(total)?;
        let start = self.reserved;
        self.reserved = total;
        Ok(start)
    }
    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let end = self.written.checked_add(bytes.len() as u64).filter(|n| *n <= self.reserved).ok_or_else(|| error("capture exceeds admitted extent"))?;
        use std::io::Write;
        let file = self.pending.file.as_mut().ok_or_else(|| error("capture file is unavailable"))?;
        #[cfg(test)]
        let result = if let Some(left) = &mut self.write_left {
            let mut fault = WriteFault { file, left: *left };
            let result = fault.write_all(bytes);
            *left = fault.left;
            result
        } else {
            file.write_all(bytes)
        };
        #[cfg(not(test))]
        let result = file.write_all(bytes);
        result.map_err(|_| error("cannot write recovery asset capture"))?;
        self.hash.update(bytes);
        self.crc.update(bytes);
        self.written = end;
        Ok(())
    }
    pub(crate) fn seal(mut self, cancel: &TransferCancel) -> Result<SealedBytes, Error> {
        cancel.check()?;
        if self.written != self.reserved {
            return Err(error("recovery capture is incomplete"));
        }
        let file = self.pending.file.as_ref().ok_or_else(|| error("capture file is unavailable"))?;
        #[cfg(test)]
        if self.workspace.0.capture_sync_fault.swap(false, Ordering::Relaxed) {
            return Err(error("injected private capture sync failure"));
        }
        file.sync_all().map_err(|_| error("cannot sync recovery asset capture"))?;
        if file.metadata().map_err(|_| error("cannot inspect recovery asset capture"))?.len() != self.written {
            return Err(error("recovery capture extent changed"));
        }
        cancel.check()?;
        self.pending.ownership.as_mut().ok_or_else(|| error("capture ownership is unavailable"))?.replace_reservation(self.verified);
        let digest =
            StreamDigest { hash: nodecraft_types::Blake3::from_bytes(*self.hash.finalize().as_bytes()), crc32: self.crc.finalize(), bytes: self.written };
        let source = Arc::new(OwnedSource {
            file: Mutex::new(self.pending.file.take()),
            length: self.written,
            _workspace: self.workspace.0.clone(),
            ownership: self.pending.ownership.take(),
            _backing: self.pending.backing.take(),
        });
        Ok(SealedBytes { source, digest })
    }
}
struct BoundedFile<'a> {
    file: &'a mut File,
    position: u64,
    limit: u64,
}
impl std::io::Write for BoundedFile<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.position.checked_add(bytes.len() as u64).is_none_or(|n| n > self.limit) {
            return Err(std::io::Error::other("archive writer exceeds its reserved length"));
        }
        let n = std::io::Write::write(self.file, bytes)?;
        self.position = self.position.checked_add(n as u64).ok_or_else(|| std::io::Error::other("archive output extent overflow"))?;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(self.file)
    }
}
impl Seek for BoundedFile<'_> {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(n) => self.position.checked_add_signed(n),
            SeekFrom::End(n) => self.file.metadata()?.len().checked_add_signed(n),
        };
        let target = target.filter(|n| *n <= self.limit).ok_or_else(|| std::io::Error::other("archive seek exceeds its reserved length"))?;
        let actual = self.file.seek(SeekFrom::Start(target))?;
        if actual != target {
            return Err(std::io::Error::other("archive seek position mismatch"));
        }
        self.position = actual;
        Ok(actual)
    }
}
/// Counts only successful writes forwarded to native_atomic's real File-backed writer.
#[cfg(test)]
struct AtomicWriteFault<'a> {
    writer: &'a mut dyn std::io::Write,
    limit: u64,
    written: &'a AtomicU64,
}
#[cfg(test)]
impl std::io::Write for AtomicWriteFault<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let total = self.written.load(Ordering::Relaxed);
        let left = self.limit.checked_sub(total).ok_or_else(|| std::io::Error::other("atomic test counter overflow"))?;
        if left == 0 {
            return Err(std::io::Error::other("injected actual atomic write failure"));
        }
        let n = bytes.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let written = self.writer.write(&bytes[..n])?;
        let next = total.checked_add(written as u64).filter(|n| *n <= self.limit).ok_or_else(|| std::io::Error::other("atomic test counter overflow"))?;
        self.written.store(next, Ordering::Relaxed);
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}
struct CopySpec {
    pool: DiskPool,
    cap: u64,
    expected: Option<StreamDigest>,
    mismatch: CopyMismatch,
}
enum CopyMismatch {
    CallerExpectation,
    StoredContent,
}
impl CopyMismatch {
    fn error(&self, message: &'static str) -> Error {
        let kind = match self {
            Self::CallerExpectation => ErrorKind::Conflict,
            Self::StoredContent => ErrorKind::InvalidInput,
        };
        Error::new(kind, message)
    }
}
struct ConcatSource {
    parts: Vec<(u64, AssetRange)>,
    length: u64,
    inner_metrics: StreamMetrics,
}
impl AssetSource for ConcatSource {
    fn byte_len(&self) -> u64 {
        self.length
    }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        if offset == self.length || out.is_empty() {
            return Ok(0);
        }
        let index = self.parts.partition_point(|(start, _)| *start <= offset).checked_sub(1).ok_or_else(|| error("invalid asset pack offset"))?;
        let (start, range) = self.parts.get(index).ok_or_else(|| error("invalid asset pack part"))?;
        let at = offset.checked_sub(*start).filter(|n| *n <= range.byte_len()).ok_or_else(|| error("asset pack offset exceeds part"))?;
        let n = usize::try_from(range.byte_len().saturating_sub(at).min(out.len() as u64)).map_err(|_| error("asset pack extent overflow"))?;
        range.read_exact_at(at, out.get_mut(..n).ok_or_else(|| error("invalid asset pack read"))?, &self.inner_metrics)?;
        Ok(n)
    }
}
struct Pending {
    backing: Option<BackingReservation>,
    file: Option<File>,
    ownership: Option<CleanupPermit>,
}
#[cfg(test)]
struct RemoveFault(Arc<AtomicU64>);
#[cfg(test)]
impl crate::native_cleanup::RemoveOwned for RemoveFault {
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        if self.0.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)).is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "injected owned-file cleanup failure"));
        }
        std::fs::remove_file(path)
    }
}
#[cfg(test)]
struct WriteFault<'a> {
    file: &'a mut File,
    left: u64,
}
#[cfg(test)]
impl std::io::Write for WriteFault<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Err(std::io::Error::other("injected private stage write failure"));
        }
        let cap = usize::try_from(self.left.min(bytes.len() as u64)).map_err(|_| std::io::Error::other("test write extent overflow"))?;
        let n = std::io::Write::write(self.file, bytes.get(..cap).ok_or_else(|| std::io::Error::other("test write extent mismatch"))?)?;
        self.left = self.left.saturating_sub(n as u64);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(self.file)
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        drop(self.file.take());
        drop(self.ownership.take());
    }
}
struct InputFile {
    file: Mutex<File>,
    length: u64,
}
impl AssetSource for InputFile {
    fn byte_len(&self) -> u64 {
        self.length
    }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        let mut file = self.file.lock().map_err(|_| error("project source lock is unavailable"))?;
        file.seek(SeekFrom::Start(offset)).map_err(|_| error("cannot seek project source"))?;
        file.read(out).map_err(|_| error("cannot read project source"))
    }
}
struct OwnedSource {
    _backing: Option<BackingReservation>,
    file: Mutex<Option<File>>,
    length: u64,
    _workspace: Arc<Workspace>,
    ownership: Option<CleanupPermit>,
}
impl AssetSource for OwnedSource {
    fn byte_len(&self) -> u64 {
        self.length
    }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        let mut guard = self.file.lock().map_err(|_| error("private asset source lock is unavailable"))?;
        let file = guard.as_mut().ok_or_else(|| error("private asset source was closed"))?;
        file.seek(SeekFrom::Start(offset)).map_err(|_| error("cannot seek private asset source"))?;
        file.read(out).map_err(|_| error("cannot read private asset source"))
    }
}
impl Drop for OwnedSource {
    fn drop(&mut self) {
        // A poisoned mutex still owns its file. Close it before removal, including on Windows.
        let slot = match self.file.get_mut() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        drop(slot.take());
        drop(self.ownership.take());
    }
}
/// Immutable owned bytes with an exact computed digest. This is not MIME/graph admission.
#[derive(Clone)]
pub struct SealedBytes {
    source: Arc<OwnedSource>,
    digest: StreamDigest,
}
impl SealedBytes {
    pub fn digest(&self) -> StreamDigest {
        self.digest
    }
    pub fn range(&self) -> Result<AssetRange, Error> {
        AssetRange::new(self.source.clone(), 0, self.digest.bytes)
    }
}
fn error(message: &'static str) -> Error {
    Error::new(ErrorKind::Conflict, message)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_source::MemorySource;
    #[test]
    fn aggregate_capture_bounds_reservations_and_cancel_short_or_failed_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let mut limits = crate::disk_quota::DiskUsage::HARD;
        limits.verified = 8;
        limits.staging = 8;
        let workspace = NativeAssetWorkspace::new(dir.path(), DiskQuota::lower(limits, 1024 * 1024).unwrap()).unwrap();
        let baseline = workspace.quota().usage().unwrap();
        let operation = workspace.quota().stage_operation().unwrap();
        let mut capture = workspace.capture(&operation).unwrap();
        assert_eq!(capture.reserve_asset(8).unwrap(), 0);
        capture.write_bytes(b"complete").unwrap();
        assert!(capture.reserve_asset(1).is_err());
        assert_eq!(workspace.quota().usage().unwrap().staging, 8);
        let sealed = capture.seal(&TransferCancel::default()).unwrap();
        assert_eq!(sealed.range().unwrap().read_small(8, &StreamMetrics::default()).unwrap(), b"complete");
        assert_eq!(workspace.quota().usage().unwrap().verified, 8);
        assert_eq!(workspace.quota().usage().unwrap().staging, 0);
        drop(sealed);
        assert_eq!(workspace.quota().usage().unwrap(), baseline);
        let mut short = workspace.capture(&operation).unwrap();
        short.reserve_asset(8).unwrap();
        short.write_bytes(b"short").unwrap();
        assert!(short.seal(&TransferCancel::default()).is_err());
        assert_eq!(workspace.quota().usage().unwrap(), baseline);
        let mut cancelled = workspace.capture(&operation).unwrap();
        cancelled.reserve_asset(3).unwrap();
        cancelled.write_bytes(b"abc").unwrap();
        let cancel = TransferCancel::default();
        cancel.cancel();
        assert!(cancelled.seal(&cancel).is_err());
        assert_eq!(workspace.quota().usage().unwrap(), baseline);
        let mut debt = workspace.capture(&operation).unwrap();
        debt.reserve_asset(3).unwrap();
        debt.write_bytes(b"abc").unwrap();
        workspace.fail_removals(1);
        drop(debt);
        assert_eq!(workspace.quota().cleanup_retained_bytes().unwrap(), 3);
        assert_eq!(workspace.quota().usage().unwrap().staging, 3);
        assert_eq!(workspace.quota().usage().unwrap().verified, 0);
        workspace.cleanup().unwrap();
        assert_eq!(workspace.quota().usage().unwrap(), baseline);
    }
    #[test]
    fn sealed_snapshot_survives_mutation_and_deletion_of_its_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input");
        std::fs::write(&path, b"original").unwrap();
        let quota = DiskQuota::default();
        let workspace = NativeAssetWorkspace::new(dir.path(), quota.clone()).unwrap();
        let baseline = quota.usage().unwrap();
        let sealed = workspace.snapshot_file(File::open(&path).unwrap(), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        std::fs::write(&path, b"modified").unwrap();
        std::fs::remove_file(path).unwrap();
        let lease = sealed.range().unwrap();
        drop(sealed);
        assert_eq!(lease.read_small(100, &StreamMetrics::default()).unwrap(), b"original");
        assert_eq!(quota.usage().unwrap().bases, 8);
        drop(lease);
        assert_eq!(quota.usage().unwrap(), baseline);
    }
    #[test]
    fn borrowed_stage_operation_supports_nested_transfer_without_a_second_pool() {
        let dir = tempfile::tempdir().unwrap();
        let limits = crate::disk_quota::DiskUsage { verified: 8192, bases: 8192, journals: 8192, staging: 4096, metadata: 4096 };
        let quota = DiskQuota::lower(limits, 32768).unwrap();
        let workspace = NativeAssetWorkspace::new(dir.path(), quota.clone()).unwrap();
        let baseline = quota.usage().unwrap();
        let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(vec![b'A'; 4096]))), 0, 4096).unwrap();
        let operation = quota.stage_operation().unwrap();
        let sealed = workspace.seal_copy_in(&operation, &range, DiskPool::Verified, None, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        assert_eq!(sealed.digest().bytes, 4096);
        assert!(workspace.seal_copy(&range, DiskPool::Verified, None, &TransferCancel::default(), &StreamMetrics::default()).is_err());
        let aggregate_stage = quota.reserve(DiskPool::Staging, 4096).unwrap();
        let metrics = StreamMetrics::default();
        assert!(workspace.seal_copy_in(&operation, &range, DiskPool::Verified, None, &TransferCancel::default(), &metrics).is_err());
        assert_eq!(metrics.stats().source_bytes, 0);
        let other = DiskQuota::default();
        let wrong = other.stage_operation().unwrap();
        assert!(workspace.seal_copy_in(&wrong, &range, DiskPool::Verified, None, &TransferCancel::default(), &metrics).is_err());
        assert_eq!(metrics.stats().source_bytes, 0);
        drop((aggregate_stage, sealed, operation));
        assert_eq!(quota.usage().unwrap(), baseline);
    }
    #[test]
    fn wrong_expected_hash_leaves_no_admitted_bytes_or_charge() {
        let dir = tempfile::tempdir().unwrap();
        let quota = DiskQuota::default();
        let workspace = NativeAssetWorkspace::new(dir.path(), quota.clone()).unwrap();
        let baseline = quota.usage().unwrap();
        let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"abc".as_slice()))), 0, 3).unwrap();
        let expected = StreamDigest { hash: nodecraft_types::Blake3::from_bytes([0; 32]), crc32: 0, bytes: 3 };
        assert!(workspace.seal_copy(&range, DiskPool::Verified, Some(expected), &TransferCancel::default(), &StreamMetrics::default()).is_err());
        assert_eq!(quota.usage().unwrap(), baseline);
        workspace.cleanup().unwrap();
    }
    #[test]
    fn stored_copy_mismatch_is_corruption_but_caller_expectations_remain_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let quota = DiskQuota::default();
        let workspace = NativeAssetWorkspace::new(dir.path(), quota.clone()).unwrap();
        let baseline = quota.usage().unwrap();
        let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"abc".as_slice()))), 0, 3).unwrap();
        let actual = StreamDigest { hash: nodecraft_types::Blake3::from_bytes(*blake3::hash(b"abc").as_bytes()), crc32: crc32fast::hash(b"abc"), bytes: 3 };
        let mismatches = [
            StreamDigest { bytes: 4, ..actual },
            StreamDigest { hash: nodecraft_types::Blake3::from_bytes([0; 32]), ..actual },
            StreamDigest { crc32: actual.crc32 ^ 1, ..actual },
        ];
        for expected in mismatches {
            let error = workspace.seal_copy(&range, DiskPool::Verified, Some(expected), &TransferCancel::default(), &StreamMetrics::default()).err().unwrap();
            assert_eq!(error.kind, ErrorKind::Conflict);
            let operation = quota.stage_operation().unwrap();
            let error = workspace
                .seal_copy_in(&operation, &range, DiskPool::Verified, Some(expected), &TransferCancel::default(), &StreamMetrics::default())
                .err()
                .unwrap();
            assert_eq!(error.kind, ErrorKind::InvalidInput);
            assert_eq!(quota.usage().unwrap(), baseline);
        }
    }
    #[test]
    fn stored_copy_operational_failures_do_not_become_content_corruption() {
        struct Unreadable;
        impl AssetSource for Unreadable {
            fn byte_len(&self) -> u64 {
                3
            }
            fn read_at(&self, _: u64, _: &mut [u8]) -> Result<usize, Error> {
                Err(Error::new(ErrorKind::Conflict, "injected source read failure"))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let quota = DiskQuota::default();
        let workspace = NativeAssetWorkspace::new(dir.path(), quota.clone()).unwrap();
        let baseline = quota.usage().unwrap();
        let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"abc".as_slice()))), 0, 3).unwrap();
        let expected = StreamDigest { hash: nodecraft_types::Blake3::from_bytes(*blake3::hash(b"abc").as_bytes()), crc32: crc32fast::hash(b"abc"), bytes: 3 };
        let operation = quota.stage_operation().unwrap();
        let cancelled = TransferCancel::default();
        cancelled.cancel();
        let error = workspace.seal_copy_in(&operation, &range, DiskPool::Verified, Some(expected), &cancelled, &StreamMetrics::default()).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Conflict);
        let unreadable = AssetRange::new(Arc::new(Unreadable), 0, 3).unwrap();
        let error = workspace
            .seal_copy_in(&operation, &unreadable, DiskPool::Verified, Some(expected), &TransferCancel::default(), &StreamMetrics::default())
            .err()
            .unwrap();
        assert_eq!(error.kind, ErrorKind::Conflict);
        workspace.fail_next_write_after(1).unwrap();
        let error = workspace
            .seal_copy_in(&operation, &range, DiskPool::Verified, Some(expected), &TransferCancel::default(), &StreamMetrics::default())
            .err()
            .unwrap();
        assert_eq!(error.kind, ErrorKind::Conflict);
        let other = DiskQuota::default();
        let foreign = other.stage_operation().unwrap();
        let metrics = StreamMetrics::default();
        let error = workspace.seal_copy_in(&foreign, &range, DiskPool::Verified, Some(expected), &TransferCancel::default(), &metrics).err().unwrap();
        assert_ne!(error.kind, ErrorKind::InvalidInput);
        assert_eq!(metrics.stats().source_bytes, 0);
        assert_eq!(quota.usage().unwrap(), baseline);
        workspace.seal_copy_in(&operation, &range, DiskPool::Verified, Some(expected), &TransferCancel::default(), &metrics).unwrap();
        assert_eq!(quota.usage().unwrap(), baseline);
    }
    #[test]
    fn a_real_large_private_file_uses_one_buffer_and_no_body_vec() {
        struct Fill;
        impl AssetSource for Fill {
            fn byte_len(&self) -> u64 {
                20 * 1024 * 1024 + 1
            }
            fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
                let n = out.len().min(usize::try_from(self.byte_len().saturating_sub(at)).unwrap_or(usize::MAX));
                out[..n].fill(77);
                Ok(n)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let quota = DiskQuota::default();
        let workspace = NativeAssetWorkspace::new(dir.path(), quota.clone()).unwrap();
        let baseline = quota.usage().unwrap();
        let range = AssetRange::new(Arc::new(Fill), 0, Fill.byte_len()).unwrap();
        let metrics = StreamMetrics::default();
        let sealed = workspace.seal_copy(&range, DiskPool::Verified, None, &TransferCancel::default(), &metrics).unwrap();
        assert_eq!(sealed.digest.bytes, Fill.byte_len());
        assert_eq!(metrics.stats().peak_transfer_bytes, 64 * 1024);
        assert_eq!(metrics.stats().live_transfer_bytes, 0);
        let range = sealed.range().unwrap();
        assert!(range.read_small(crate::asset_source::MAX_SMALL_READ_BYTES, &metrics).is_err());
        let mut last = [0];
        range.read_exact_at(range.byte_len() - 1, &mut last, &metrics).unwrap();
        assert_eq!(last, [77]);
        drop((range, sealed));
        assert_eq!(quota.usage().unwrap(), baseline);
    }
}

#[cfg(test)]
#[path = "native_workspace/orphan_tests.rs"]
mod orphan_regressions;

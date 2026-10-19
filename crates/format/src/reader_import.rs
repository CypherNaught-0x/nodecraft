//! Unknown-length input is a bounded observation, sealed before any logical publication.
use crate::{
    asset_source::{MAX_LARGE_ASSET_BYTES, StreamMetrics, TransferBuffer, TransferCancel},
    disk_quota::StageOperation,
    native_assets::{NativeAssetWorkspace, SealedBytes},
};
use nodecraft_types::{Error, ErrorKind};
use std::io::Read;

pub(crate) fn capture(
    workspace: &NativeAssetWorkspace,
    operation: &StageOperation<'_>,
    reader: &mut dyn Read,
    cancel: &TransferCancel,
    metrics: &StreamMetrics,
) -> Result<SealedBytes, Error> {
    capture_with_cap(workspace, operation, reader, cancel, metrics, MAX_LARGE_ASSET_BYTES)
}

fn capture_with_cap(
    workspace: &NativeAssetWorkspace,
    operation: &StageOperation<'_>,
    reader: &mut dyn Read,
    cancel: &TransferCancel,
    metrics: &StreamMetrics,
    cap: u64,
) -> Result<SealedBytes, Error> {
    cancel.check()?;
    let mut capture = workspace.capture(operation)?;
    let mut buffer = TransferBuffer::new(metrics)?;
    let mut written = 0u64;
    let mut interrupted = 0u32;
    loop {
        cancel.check()?;
        let remaining = cap.checked_sub(written).ok_or_else(|| crate::invalid("reader asset extent overflow"))?;
        // A nonempty detection read is necessary even for an exact-cap stream.
        let request = if remaining == 0 {
            1
        } else {
            usize::try_from(remaining.min(buffer.bytes_mut().len() as u64)).map_err(|_| crate::invalid("reader extent exceeds host"))?
        };
        let bytes = buffer.bytes_mut().get_mut(..request).ok_or_else(|| crate::invalid("reader buffer extent is invalid"))?;
        let result = reader.read(bytes);
        if let Ok(count) = &result
            && *count <= request
        {
            metrics.read(*count);
        }
        cancel.check()?;
        let count = match result {
            Ok(count) => count,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                if interrupted >= 1024 {
                    return Err(operational("asset reader interrupted too many times"));
                }
                interrupted += 1;
                continue;
            }
            Err(_) => return Err(operational("cannot read asset input; the reader may have consumed a prefix")),
        };
        if count > request {
            return Err(crate::invalid("asset reader returned an invalid extent"));
        }
        if count == 0 {
            break;
        }
        if remaining == 0 {
            return Err(crate::invalid("asset reader exceeds 8 GiB limit"));
        }
        written = written.checked_add(count as u64).filter(|n| *n <= cap).ok_or_else(|| crate::invalid("asset reader exceeds its byte limit"))?;
        // The just-read RAM chunk is bounded; both future Verified and physical Staging
        // ownership must grow before any part of it reaches disk.
        capture.reserve_asset(count as u64)?;
        cancel.check()?;
        capture.write_bytes(bytes.get(..count).ok_or_else(|| crate::invalid("reader buffer extent is invalid"))?)?;
        cancel.check()?;
    }
    drop(buffer);
    let sealed = capture.seal(cancel)?;
    if sealed.digest().bytes != written || written > cap {
        return Err(crate::invalid("sealed reader asset extent disagrees"));
    }
    Ok(sealed)
}
fn operational(message: &'static str) -> Error {
    Error::new(ErrorKind::Conflict, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetContext, AssetStore,
        asset_source::TRANSFER_BYTES,
        disk_quota::{DiskQuota, DiskUsage},
    };
    use std::io::{self, Cursor};
    fn workspace(quota: DiskQuota) -> (tempfile::TempDir, NativeAssetWorkspace) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = NativeAssetWorkspace::new(dir.path(), quota).unwrap();
        (dir, workspace)
    }
    struct Short {
        left: usize,
        step: usize,
        observed: usize,
        requests: Vec<usize>,
        quota: Option<DiskQuota>,
    }
    impl Read for Short {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            assert!(!out.is_empty());
            assert!(out.len() <= TRANSFER_BYTES);
            if let Some(quota) = &self.quota {
                let usage = quota.usage().unwrap();
                assert!(usage.verified >= self.observed as u64);
                assert!(usage.staging >= self.observed as u64);
            }
            self.requests.push(out.len());
            let n = self.left.min(self.step).min(out.len());
            out[..n].fill(b'x');
            self.left -= n;
            self.observed += n;
            Ok(n)
        }
    }
    fn short(bytes: usize, step: usize) -> Short {
        Short { left: bytes, step, observed: 0, requests: Vec::new(), quota: None }
    }
    #[test]
    fn unknown_reader_short_eof_and_exact_cap_have_complete_computed_identity() {
        for length in [0, 1, 33] {
            let (_dir, workspace) = workspace(DiskQuota::default());
            let operation = workspace.quota().stage_operation().unwrap();
            let metrics = StreamMetrics::default();
            let mut reader = short(length, 2);
            let sealed = capture_with_cap(&workspace, &operation, &mut reader, &TransferCancel::default(), &metrics, 33).unwrap();
            let expected = vec![b'x'; length];
            let digest = sealed.digest();
            assert_eq!(digest.bytes, length as u64);
            assert_eq!(digest.hash, crate::hash(&expected));
            assert_eq!(digest.crc32, crc32fast::hash(&expected));
            assert_eq!(sealed.range().unwrap().read_small(64, &metrics).unwrap(), expected);
            if length == 33 {
                assert_eq!(reader.requests.last(), Some(&1));
            }
            assert_eq!(metrics.stats().live_transfer_bytes, 0);
            assert_eq!(metrics.stats().peak_transfer_bytes, TRANSFER_BYTES as u64);
        }
    }
    #[test]
    fn unknown_reader_cap_plus_one_and_invalid_extent_never_publish_or_overwrite_cap() {
        let (_dir, workspace) = workspace(DiskQuota::default());
        let operation = workspace.quota().stage_operation().unwrap();
        let metrics = StreamMetrics::default();
        let mut reader = short(34, 32);
        reader.quota = Some(workspace.quota().clone());
        let error = capture_with_cap(&workspace, &operation, &mut reader, &TransferCancel::default(), &metrics, 33).err().unwrap();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert_eq!(reader.observed, 34);
        assert_eq!(reader.requests.last(), Some(&1));
        assert_eq!(metrics.stats().source_bytes, 34);
        assert_eq!(workspace.quota().usage().unwrap().staging, 0);
        struct Invalid;
        impl Read for Invalid {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                Ok(out.len() + 1)
            }
        }
        assert_eq!(
            capture_with_cap(&workspace, &operation, &mut Invalid, &TransferCancel::default(), &metrics, 33).err().unwrap().kind,
            ErrorKind::InvalidInput
        );
        assert_eq!(metrics.stats().live_transfer_bytes, 0);
    }
    struct Interrupted {
        left: usize,
        progress_between: bool,
        emit: bool,
        calls: usize,
    }
    impl Read for Interrupted {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.emit {
                self.emit = false;
                out[0] = b'x';
                return Ok(1);
            }
            if self.left > 0 {
                self.left -= 1;
                self.emit = self.progress_between;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "private reader detail"));
            }
            Ok(0)
        }
    }
    #[test]
    fn unknown_reader_interrupt_budget_is_total_and_positive_progress_does_not_reset_it() {
        let (_dir, workspace) = workspace(DiskQuota::default());
        let operation = workspace.quota().stage_operation().unwrap();
        let metrics = StreamMetrics::default();
        let mut okay = Interrupted { left: 1024, progress_between: false, emit: false, calls: 0 };
        assert_eq!(capture_with_cap(&workspace, &operation, &mut okay, &TransferCancel::default(), &metrics, 4096).unwrap().digest().bytes, 0);
        assert_eq!(okay.calls, 1025);
        let mut exhausted = Interrupted { left: 1025, progress_between: true, emit: false, calls: 0 };
        let error = capture_with_cap(&workspace, &operation, &mut exhausted, &TransferCancel::default(), &metrics, 4096).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Conflict);
        assert!(!error.message.contains("private reader detail"));
        assert_eq!(exhausted.calls, 2049);
        assert_eq!(metrics.stats().live_transfer_bytes, 0);
    }
    #[test]
    fn unknown_reader_operational_errors_and_post_read_cancel_release_unpublished_owners() {
        struct Failed {
            first: bool,
            kind: io::ErrorKind,
        }
        impl Read for Failed {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if self.first {
                    self.first = false;
                    out[0] = b'x';
                    Ok(1)
                } else {
                    Err(io::Error::new(self.kind, "secret path /private/key"))
                }
            }
        }
        let (_dir, workspace) = workspace(DiskQuota::default());
        let mut store = AssetStore::with_context(AssetContext::native(workspace.clone()));
        let metrics = StreamMetrics::default();
        for kind in [io::ErrorKind::WouldBlock, io::ErrorKind::TimedOut, io::ErrorKind::Other] {
            let error = store.import_reader(&mut Failed { first: true, kind }, &TransferCancel::default(), &metrics).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Conflict);
            assert!(!error.message.contains("secret"));
            assert!(store.is_empty());
        }
        struct Cancel(TransferCancel);
        impl Read for Cancel {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                out[0] = b'x';
                self.0.cancel();
                Ok(1)
            }
        }
        let cancel = TransferCancel::default();
        let before = metrics.stats().source_bytes;
        assert!(store.import_reader(&mut Cancel(cancel.clone()), &cancel, &metrics).is_err());
        assert_eq!(metrics.stats().source_bytes, before + 1);
        assert!(store.is_empty());
        assert_eq!(metrics.stats().live_transfer_bytes, 0);
        assert_eq!(workspace.quota().usage().unwrap().verified, 0);
        assert_eq!(workspace.quota().usage().unwrap().staging, 0);
        store.import_reader(&mut Cursor::new(b"retry"), &TransferCancel::default(), &metrics).unwrap();
    }
    #[test]
    fn unknown_reader_incremental_dual_quota_refusals_precede_each_write() {
        for (verified, staging) in [(5, 100), (100, 5)] {
            let quota = DiskQuota::lower(DiskUsage { verified, staging, ..DiskUsage::HARD }, 640 * 1024 * 1024 * 1024).unwrap();
            let (_dir, workspace) = workspace(quota.clone());
            let mut store = AssetStore::with_context(AssetContext::native(workspace));
            let mut reader = short(6, 2);
            reader.quota = Some(quota.clone());
            assert!(store.import_reader(&mut reader, &TransferCancel::default(), &StreamMetrics::default()).is_err());
            assert_eq!(reader.observed, 6);
            assert!(store.is_empty());
            assert_eq!(quota.usage().unwrap().staging, 0);
            assert_eq!(quota.usage().unwrap().verified, 0);
        }
    }
    #[test]
    fn unknown_reader_setup_refuses_before_reader_and_holds_one_staging_gate_without_accounting_lock() {
        let (_dir, workspace) = workspace(DiskQuota::default());
        let mut store = AssetStore::with_context(AssetContext::native(workspace.clone()));
        let mut reader = short(4, 4);
        let held = workspace.quota().try_stage().unwrap();
        assert!(store.import_reader(&mut reader, &TransferCancel::default(), &StreamMetrics::default()).is_err());
        assert!(reader.requests.is_empty());
        drop(held);
        let permits = (0..32).map(|_| workspace.quota().reserve_backing().unwrap()).collect::<Vec<_>>();
        assert!(store.import_reader(&mut reader, &TransferCancel::default(), &StreamMetrics::default()).is_err());
        assert!(reader.requests.is_empty());
        drop(permits);
        let held = workspace
            .quota()
            .reserve(crate::disk_quota::DiskPool::Metadata, workspace.quota().remaining(crate::disk_quota::DiskPool::Metadata).unwrap())
            .unwrap();
        assert!(store.import_reader(&mut reader, &TransferCancel::default(), &StreamMetrics::default()).is_err());
        assert!(reader.requests.is_empty());
        drop(held);
        struct Inspect {
            workspace: NativeAssetWorkspace,
            done: bool,
        }
        impl Read for Inspect {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                assert!(self.workspace.quota().try_stage().is_err());
                self.workspace.quota().usage().unwrap();
                if self.done {
                    return Ok(0);
                }
                self.done = true;
                out[0] = b'x';
                Ok(1)
            }
        }
        store.import_reader(&mut Inspect { workspace: workspace.clone(), done: false }, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    }
    #[test]
    fn unknown_reader_partial_write_sync_failure_and_delete_debt_preserve_exact_charges() {
        let (dir, workspace) = workspace(DiskQuota::default());
        let unrelated = dir.path().join("keep");
        std::fs::write(&unrelated, b"safe").unwrap();
        let mut store = AssetStore::with_context(AssetContext::native(workspace.clone()));
        let metrics = StreamMetrics::default();
        workspace.fail_next_write_after(2).unwrap();
        workspace.fail_removals(2);
        assert!(store.import_reader(&mut Cursor::new(b"abcd"), &TransferCancel::default(), &metrics).is_err());
        assert!(store.is_empty());
        assert_eq!(workspace.quota().backing_count().unwrap(), 0);
        assert_eq!(workspace.quota().usage().unwrap().staging, 4);
        assert_eq!(workspace.quota().cleanup_retained_bytes().unwrap(), 4);
        let mut reader = short(4, 4);
        assert!(store.import_reader(&mut reader, &TransferCancel::default(), &metrics).is_err());
        assert!(reader.requests.is_empty());
        assert_eq!(workspace.quota().cleanup_retained_bytes().unwrap(), 4);
        workspace.cleanup().unwrap();
        assert_eq!(workspace.quota().cleanup_retained_bytes().unwrap(), 0);
        workspace.fail_next_capture_sync();
        assert!(store.import_reader(&mut Cursor::new(b"abcd"), &TransferCancel::default(), &metrics).is_err());
        assert!(store.is_empty());
        assert_eq!(workspace.quota().usage().unwrap().verified, 0);
        assert_eq!(workspace.quota().usage().unwrap().staging, 0);
        assert_eq!(metrics.stats().live_transfer_bytes, 0);
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"safe");
        store.import_reader(&mut Cursor::new(b"abcd"), &TransferCancel::default(), &metrics).unwrap();
    }
    #[test]
    fn unknown_reader_resident_host_refuses_before_any_reader_call() {
        let mut store = AssetStore::default();
        let mut reader = short(1, 1);
        assert_eq!(store.import_reader(&mut reader, &TransferCancel::default(), &StreamMetrics::default()).unwrap_err().kind, ErrorKind::Unsupported);
        assert!(reader.requests.is_empty());
        assert!(store.is_empty());
        store.insert(b"small still works").unwrap();
    }
    #[test]
    fn unknown_reader_blocking_call_keeps_gates_but_not_accounting_mutex_and_cancel_is_cooperative() {
        use std::{sync::mpsc, thread, time::Duration};
        let (_dir, workspace) = workspace(DiskQuota::default());
        let context = AssetContext::native(workspace.clone());
        let mut store = AssetStore::with_context(context.clone());
        let cancel = TransferCancel::default();
        let metrics = StreamMetrics::default();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        struct Blocking {
            entered: mpsc::SyncSender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Read for Blocking {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                self.entered.send(()).map_err(|_| io::Error::other("fixture receiver closed"))?;
                self.release.recv_timeout(Duration::from_secs(2)).map_err(|_| io::Error::other("fixture release timed out"))?;
                out[0] = b'x';
                Ok(1)
            }
        }
        thread::scope(|scope| {
            let cancel_worker = cancel.clone();
            let metrics_worker = metrics.clone();
            let store_worker = &mut store;
            let worker =
                scope.spawn(move || store_worker.import_reader(&mut Blocking { entered: entered_tx, release: release_rx }, &cancel_worker, &metrics_worker));
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(context.operation().is_err());
            assert!(workspace.quota().try_stage().is_err());
            workspace.quota().usage().unwrap();
            cancel.cancel();
            release_tx.send(()).unwrap();
            assert_eq!(worker.join().unwrap().unwrap_err().kind, ErrorKind::Conflict);
        });
        assert!(store.is_empty());
        assert_eq!(metrics.stats().live_transfer_bytes, 0);
        assert!(context.operation().is_ok());
        assert!(workspace.quota().try_stage().is_ok());
    }
    #[test]
    fn unknown_reader_total_quota_and_cleanup_slots_are_not_hidden_by_spooling() {
        let quota = DiskQuota::lower(DiskUsage { verified: 64 * 1024, staging: 64 * 1024, metadata: 4096, bases: 0, journals: 0 }, 4096).unwrap();
        let (_dir, workspace) = workspace(quota.clone());
        let mut store = AssetStore::with_context(AssetContext::native(workspace));
        let mut reader = short(8192, 128);
        reader.quota = Some(quota.clone());
        assert!(store.import_reader(&mut reader, &TransferCancel::default(), &StreamMetrics::default()).is_err());
        assert!(reader.observed > 128 && reader.observed < 8192);
        assert!(quota.usage().unwrap().total().unwrap() <= 4096);
        assert!(store.is_empty());
        assert_eq!(quota.usage().unwrap().verified, 0);
        assert_eq!(quota.usage().unwrap().staging, 0);
        let (_dir, mut workspace) = self::workspace(DiskQuota::default());
        workspace.test_cleanup_capacity(2).unwrap();
        let stage = workspace.quota().stage_operation().unwrap();
        let mut held = Vec::new();
        let error = loop {
            match workspace.capture(&stage) {
                Ok(capture) => held.push(capture),
                Err(error) => break error,
            }
        };
        assert!(error.message.contains("slots are full"), "{error:?}");
        assert!(held.len() < 30);
        drop(stage);
        let mut store = AssetStore::with_context(AssetContext::native(workspace.clone()));
        let mut input = short(4, 4);
        assert!(store.import_reader(&mut input, &TransferCancel::default(), &StreamMetrics::default()).is_err());
        assert!(input.requests.is_empty());
        drop(held);
        store.import_reader(&mut input, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    }
}

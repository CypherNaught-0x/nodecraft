use super::*;
use crate::asset_source::AssetSource;
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(not(target_arch = "wasm32"))]
struct Counting {
    bytes: Vec<u8>,
    calls: Arc<AtomicUsize>,
    largest: Arc<AtomicUsize>,
    cancel: Option<TransferCancel>,
    fail: bool,
}
#[cfg(not(target_arch = "wasm32"))]
impl AssetSource for Counting {
    fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.largest.fetch_max(out.len(), Ordering::SeqCst);
        if self.fail {
            return Err(Error::new(ErrorKind::Internal, "injected Interrupted source error"));
        }
        let at = at as usize;
        let n = out.len().min(self.bytes.len().saturating_sub(at));
        out[..n].copy_from_slice(&self.bytes[at..at + n]);
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        Ok(n)
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn fixture(bytes: Vec<u8>, fail: bool, cancel: Option<TransferCancel>) -> (tempfile::TempDir, AssetStore, AssetRef, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = crate::native_assets::NativeAssetWorkspace::new(temp.path(), Default::default()).unwrap();
    let context = AssetContext::native(workspace);
    let mut store = AssetStore::with_context(context.clone());
    let hash = crate::hash(&bytes);
    let length = bytes.len() as u64;
    let crc = crc32fast::hash(&bytes);
    let calls = Arc::new(AtomicUsize::new(0));
    let largest = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(Counting { bytes, calls: calls.clone(), largest: largest.clone(), cancel, fail });
    let range = AssetRange::new(source, 0, length).unwrap();
    let identity = context.claim(hash, length).unwrap();
    store.assets.insert(hash, Arc::new(Entry { identity, memory: None, _origin: Mutex::new(Some(range)), expected_crc: Some(crc) }));
    store.bytes = length;
    (temp, store, AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() }, calls, largest)
}
fn png() -> Vec<u8> {
    nodecraft_media::encode_png(8, 6, &[90; 8 * 6 * 4], &Default::default()).unwrap()
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn preview_capture_does_no_io_and_pins_one_identity_without_publishing_cache() {
    let (_temp, store, asset, calls, _) = fixture(png(), false, None);
    let context = store.context.clone();
    let request = store.preview_read(&asset).unwrap();
    assert!(request.resident().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let usage = context.storage_usage().unwrap();
    drop(store);
    assert_eq!(context.retained_usage().unwrap().0, 1);
    let body = request.read_verified(&Default::default(), &Default::default()).unwrap();
    assert_eq!(body, png());
    assert!(context.0.cache.lock().unwrap().is_empty());
    assert_eq!(context.storage_usage().unwrap(), usage);
    drop(request);
    assert_eq!(context.retained_usage().unwrap().0, 0);
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn preview_busy_is_distinct_and_recoverable_after_the_real_operation_guard() {
    let (_temp, store, asset, calls, _) = fixture(png(), false, None);
    let request = store.preview_read(&asset).unwrap();
    let operation = store.context.operation().unwrap();
    assert_eq!(request.read_verified(&Default::default(), &Default::default()), Err(PreviewReadError::Busy));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(operation);
    assert_eq!(request.read_verified(&Default::default(), &Default::default()).unwrap(), png());
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn preview_reads_chunks_cancels_and_never_retries_a_source_error() {
    let mut body = png();
    body.resize(200_000, 0);
    let (_temp, store, asset, calls, largest) = fixture(body.clone(), false, None);
    let request = store.preview_read(&asset).unwrap();
    assert_eq!(request.read_verified(&Default::default(), &Default::default()).unwrap(), body);
    assert!(calls.load(Ordering::SeqCst) >= 4);
    assert_eq!(largest.load(Ordering::SeqCst), 64 * 1024);
    let cancel = TransferCancel::default();
    let (_temp, store, asset, calls, _) = fixture(body, false, Some(cancel.clone()));
    assert_eq!(store.preview_read(&asset).unwrap().read_verified(&cancel, &Default::default()), Err(PreviewReadError::Cancelled));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let (_temp, store, asset, calls, _) = fixture(png(), true, None);
    assert_eq!(store.preview_read(&asset).unwrap().read_verified(&Default::default(), &Default::default()), Err(PreviewReadError::Unreadable));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn preview_crc_metadata_hash_and_cache_alias_refuse() {
    let (_temp, mut store, mut asset, calls, _) = fixture(png(), false, None);
    asset.meta.width = Some(9);
    assert_eq!(store.preview_read(&asset).unwrap().read_verified(&Default::default(), &Default::default()), Err(PreviewReadError::Corrupt));
    asset.meta.width = None;
    Arc::get_mut(store.assets.get_mut(&asset.hash).unwrap()).unwrap().expected_crc = Some(0);
    assert_eq!(store.preview_read(&asset).unwrap().read_verified(&Default::default(), &Default::default()), Err(PreviewReadError::Corrupt));
    let entry = store.assets.get(&asset.hash).unwrap();
    let other = AssetContext::default().claim(asset.hash, entry.identity.bytes).unwrap();
    let range = entry._origin.lock().unwrap().as_ref().unwrap().clone();
    store
        .context
        .0
        .cache
        .lock()
        .unwrap()
        .insert(asset.hash, CacheRecord { identity: Arc::downgrade(&other), data: VerifiedData { range, crc32: 0, sniffed: None, image: None } });
    let before = calls.load(Ordering::SeqCst);
    assert_eq!(store.preview_read(&asset).unwrap().read_verified(&Default::default(), &Default::default()), Err(PreviewReadError::Corrupt));
    assert_eq!(calls.load(Ordering::SeqCst), before);
    store.context.0.cache.lock().unwrap().clear();
    let wrong = Blake3::from_bytes([7; 32]);
    let mut entry = store.assets.remove(&asset.hash).unwrap();
    let owned = Arc::get_mut(&mut entry).unwrap();
    owned.identity = store.context.claim(wrong, owned.identity.bytes).unwrap();
    owned.expected_crc = None;
    store.assets.insert(wrong, entry);
    asset.hash = wrong;
    assert_eq!(store.preview_read(&asset).unwrap().read_verified(&Default::default(), &Default::default()), Err(PreviewReadError::Corrupt));
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn preview_oversize_descriptor_refuses_before_any_body_read() {
    struct Oversize(Arc<AtomicUsize>);
    impl AssetSource for Oversize {
        fn byte_len(&self) -> u64 {
            nodecraft_media::Limits::default().max_input_bytes + 1
        }
        fn read_at(&self, _: u64, _: &mut [u8]) -> Result<usize, Error> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(Error::new(ErrorKind::Internal, "must not read"))
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let context = AssetContext::native(crate::native_assets::NativeAssetWorkspace::new(temp.path(), Default::default()).unwrap());
    let mut store = AssetStore::with_context(context.clone());
    let hash = Blake3::from_bytes([9; 32]);
    let calls = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(Oversize(calls.clone()));
    let length = source.byte_len();
    store.assets.insert(
        hash,
        Arc::new(Entry {
            identity: context.claim(hash, length).unwrap(),
            memory: None,
            _origin: Mutex::new(Some(AssetRange::new(source, 0, length).unwrap())),
            expected_crc: Some(0),
        }),
    );
    let asset = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    assert!(matches!(store.preview_read(&asset), Err(PreviewReadError::TooLarge)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn preview_short_reads_complete_but_early_eof_refuses() {
    struct Pieces {
        bytes: Vec<u8>,
        end: usize,
    }
    impl AssetSource for Pieces {
        fn byte_len(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
            let at = at as usize;
            let n = out.len().min(7).min(self.end.saturating_sub(at));
            out[..n].copy_from_slice(&self.bytes[at..at + n]);
            Ok(n)
        }
    }
    let body = png();
    let (_temp, store, asset, _, _) = fixture(body.clone(), false, None);
    let entry = store.assets.get(&asset.hash).unwrap();
    for end in [body.len(), body.len() / 2] {
        *entry._origin.lock().unwrap() = Some(AssetRange::new(Arc::new(Pieces { bytes: body.clone(), end }), 0, body.len() as u64).unwrap());
        let result = store.preview_read(&asset).unwrap().read_verified(&Default::default(), &Default::default());
        if end == body.len() {
            assert_eq!(result.unwrap(), body)
        } else {
            assert_eq!(result, Err(PreviewReadError::Unreadable));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn supplement_preview_pins_old_range_during_actual_private_pack() {
    use std::sync::mpsc::sync_channel;
    struct GateSource {
        range: AssetRange,
        entered: std::sync::mpsc::SyncSender<()>,
        wait: Mutex<std::sync::mpsc::Receiver<()>>,
        calls: AtomicUsize,
    }
    impl AssetSource for GateSource {
        fn byte_len(&self) -> u64 {
            self.range.byte_len()
        }
        fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.send(()).unwrap();
                self.wait.lock().unwrap().recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            }
            let remaining = self.range.byte_len().saturating_sub(at);
            self.range.slice(at, remaining)?.reader(Default::default()).read(out).map_err(|_| Error::new(ErrorKind::Internal, "controlled source read failed"))
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let workspace = crate::native_assets::NativeAssetWorkspace::new(temp.path(), Default::default()).unwrap();
    let context = AssetContext::native(workspace);
    let mut store = AssetStore::with_context(context.clone());
    let bytes = png();
    let hash = store.insert(&bytes).unwrap();
    let asset = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    let (entered, ready) = sync_channel(1);
    let (go, wait) = sync_channel(1);
    let weak;
    {
        let _operation = context.operation().unwrap();
        let mut cache = context.0.cache.lock().unwrap();
        let record = cache.get_mut(&hash).unwrap();
        let source = Arc::new(GateSource { range: record.data.range.clone(), entered, wait: Mutex::new(wait), calls: AtomicUsize::new(0) });
        weak = Arc::downgrade(&source);
        record.data.range = AssetRange::new(source, 0, bytes.len() as u64).unwrap();
    }
    let request = store.preview_read(&asset).unwrap();
    let job = std::thread::spawn(move || request.read_verified(&Default::default(), &Default::default()));
    struct Owner {
        go: std::sync::mpsc::SyncSender<()>,
        job: Option<std::thread::JoinHandle<Result<Vec<u8>, PreviewReadError>>>,
    }
    impl Drop for Owner {
        fn drop(&mut self) {
            let _ = self.go.try_send(());
            if let Some(job) = self.job.take() {
                let _ = job.join();
            }
        }
    }
    let mut owner = Owner { go, job: Some(job) };
    ready.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    // The worker has selected/pinned the old range and is in source I/O. Its selection
    // authority must already be released, allowing the actual pack publication below.
    {
        let _operation = context.operation().unwrap();
        assert_eq!(store.preview_read(&asset).unwrap().read_verified(&Default::default(), &Default::default()), Err(PreviewReadError::Busy));
        store.pack_if_needed(&Default::default(), &Default::default(), 0).unwrap();
    }
    assert!(weak.upgrade().is_some(), "active preview pins old native backing after pack publication");
    let old_calls = weak.upgrade().unwrap().calls.load(Ordering::SeqCst);
    assert!(old_calls >= 2, "actual pack read the old backing while the preview was parked");
    assert_eq!(store.preview_read(&asset).unwrap().read_verified(&Default::default(), &Default::default()).unwrap(), bytes);
    assert_eq!(weak.upgrade().unwrap().calls.load(Ordering::SeqCst), old_calls, "new preview uses relocated cache range");
    owner.go.send(()).unwrap();
    assert_eq!(owner.job.take().unwrap().join().unwrap().unwrap(), bytes);
    assert!(weak.upgrade().is_none(), "last old preview range releases its source after completion");
    assert_eq!(context.retained_usage().unwrap(), (1, bytes.len() as u64));
}

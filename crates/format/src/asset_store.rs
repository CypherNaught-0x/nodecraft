//! Bounded logical asset identities. Indexed ranges do not become trusted by registration.
#[cfg(not(target_arch = "wasm32"))]
use crate::asset_context::{CacheRecord, unavailable};
use crate::{
    asset_context::{AssetContext, AssetIdentity, VerifiedData},
    asset_source::{AssetRange, MAX_LARGE_ASSET_BYTES, MemorySource, StreamMetrics, TransferCancel},
    invalid,
};
use nodecraft_types::{AssetRef, Blake3, Error, ErrorKind};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

mod preview_read;
mod sealed;
pub use preview_read::{PreviewAssetRead, PreviewReadError};
pub use sealed::SealedAsset;

struct Entry {
    identity: Arc<AssetIdentity>,
    memory: Option<Arc<[u8]>>,
    // Retain unverified source ownership on every platform; only native may consume it.
    _origin: Mutex<Option<AssetRange>>,
    expected_crc: Option<u32>,
}
/// Logical identities and explicitly unverified indexed ranges, with private verified caches.
/// Clone/history share leases; descriptors never read media. Use open_verified for trusted access.
#[derive(Clone, Default)]
pub struct AssetStore {
    assets: BTreeMap<Blake3, Arc<Entry>>,
    bytes: u64,
    context: AssetContext,
}
impl std::fmt::Debug for AssetStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetStore").field("assets", &self.assets.len()).field("logical_bytes", &self.bytes).finish_non_exhaustive()
    }
}
impl PartialEq for AssetStore {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
            && self.assets.len() == other.assets.len()
            && self.assets.iter().all(|(hash, entry)| other.assets.get(hash).is_some_and(|e| e.identity.bytes == entry.identity.bytes))
    }
}
impl Eq for AssetStore {}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetDescriptor {
    pub hash: Blake3,
    pub bytes: u64,
}
/// A private store verifier minted this immutable content lease after expected-reference checks.
/// Each clone pins both its canonical logical identity and the exact old backing it was granted.
#[derive(Clone)]
pub struct VerifiedAsset {
    identity: Arc<AssetIdentity>,
    data: VerifiedData,
}
pub struct VerifiedReader {
    _identity: Arc<AssetIdentity>,
    reader: crate::asset_source::AssetReader,
}
impl std::io::Read for VerifiedReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.reader, out)
    }
}
impl std::io::Seek for VerifiedReader {
    fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(&mut self.reader, from)
    }
}
impl VerifiedAsset {
    pub fn hash(&self) -> Blake3 {
        self.identity.hash
    }
    pub fn byte_len(&self) -> u64 {
        self.identity.bytes
    }
    pub fn is_empty(&self) -> bool {
        self.identity.bytes == 0
    }
    pub fn reader(&self, metrics: StreamMetrics) -> VerifiedReader {
        VerifiedReader { _identity: self.identity.clone(), reader: self.data.range.reader(metrics) }
    }
    pub fn read_small(&self, cap: usize, metrics: &StreamMetrics) -> Result<Vec<u8>, Error> {
        self.data.range.read_small(cap, metrics)
    }
    pub(crate) fn range(&self) -> &AssetRange {
        &self.data.range
    }
}
impl AssetStore {
    pub fn with_context(context: AssetContext) -> Self {
        Self { context, ..Self::default() }
    }
    pub fn context(&self) -> &AssetContext {
        &self.context
    }
    pub fn descriptor(&self, hash: &Blake3) -> Option<AssetDescriptor> {
        self.assets.get(hash).map(|e| AssetDescriptor { hash: *hash, bytes: e.identity.bytes })
    }
    pub fn contains(&self, hash: &Blake3) -> bool {
        self.assets.contains_key(hash)
    }
    pub fn len(&self) -> usize {
        self.assets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }
    pub fn total_logical_bytes(&self) -> u64 {
        self.bytes
    }
    pub fn byte_len(&self) -> u64 {
        self.bytes
    }
    pub fn hashes(&self) -> impl Iterator<Item = &Blake3> {
        self.assets.keys()
    }
    /// The frozen bounded resident convenience view. Indexed/imported disk bytes are NEVER
    /// implicitly materialized by this method; use open_verified and an explicit reader/cap.
    pub fn get(&self, hash: &Blake3) -> Option<&[u8]> {
        self.assets.get(hash).and_then(|e| e.memory.as_deref())
    }
    /// Legacy resident entries only. New archive/journal paths iterate identities/leases instead.
    pub fn iter(&self) -> impl Iterator<Item = (&Blake3, &[u8])> {
        self.assets.iter().filter_map(|(hash, e)| e.memory.as_deref().map(|bytes| (hash, bytes)))
    }
    pub fn is_resident_small(&self) -> bool {
        self.bytes <= crate::MAX_TOTAL_ASSET_BYTES as u64
            && self.assets.values().all(|e| e.memory.is_some() && e.identity.bytes <= crate::MAX_ASSET_BYTES as u64)
    }
    pub fn insert(&mut self, bytes: &[u8]) -> Result<Blake3, Error> {
        if bytes.len() > crate::MAX_ASSET_BYTES {
            return Err(invalid("asset exceeds 16 MiB convenience limit; use stream import"));
        }
        let hash = crate::hash(bytes);
        if self.context.supports_large_assets() {
            if self.assets.contains_key(&hash) {
                return Ok(hash);
            }
            let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 0, bytes.len() as u64)?;
            return match self.import_stream(&range, &TransferCancel::default(), &StreamMetrics::default()) {
                // Storage busy with a long copy, such as a shared asset being sealed (#855):
                // a small asset waits in memory instead, while there's room.
                Err(error) if error.kind == ErrorKind::Busy => self.insert_resident(hash, bytes).map_err(|_| error),
                imported => imported,
            };
        }
        let _operation = self.context.operation()?;
        self.context.prune_cache()?;
        if self.assets.contains_key(&hash) {
            return Ok(hash);
        }
        let total = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|n| *n <= crate::MAX_TOTAL_ASSET_BYTES as u64)
            .ok_or_else(|| invalid("asset store exceeds 48 MiB resident convenience limit"))?;
        let identity = self.context.claim(hash, bytes.len() as u64)?;
        let memory = identity.resident_bytes(bytes)?;
        self.assets.insert(hash, Arc::new(Entry { identity, memory: Some(memory), _origin: Mutex::new(None), expected_crc: None }));
        self.bytes = total;
        Ok(hash)
    }
    /// Keeps `bytes`, which hash to `hash`, in memory on a host with private storage, without
    /// waiting for that storage (#855). Resident bytes stay within the resident convenience
    /// limit, so a long copy can't make the store grow in memory without bound.
    fn insert_resident(&mut self, hash: Blake3, bytes: &[u8]) -> Result<Blake3, Error> {
        let resident = self.assets.values().filter_map(|e| e.memory.as_ref()).fold(0u64, |n, m| n.saturating_add(m.len() as u64));
        if resident.checked_add(bytes.len() as u64).is_none_or(|n| n > crate::MAX_TOTAL_ASSET_BYTES as u64) {
            return Err(invalid("too many assets wait in memory for busy storage"));
        }
        let total = self.bytes.checked_add(bytes.len() as u64).ok_or_else(|| invalid("asset extent overflow"))?;
        let identity = self.context.claim(hash, bytes.len() as u64)?;
        let memory = identity.resident_bytes(bytes)?;
        self.assets.insert(hash, Arc::new(Entry { identity, memory: Some(memory), _origin: Mutex::new(None), expected_crc: None }));
        self.bytes = total;
        Ok(hash)
    }
    pub fn retain(&mut self, roots: &BTreeSet<Blake3>) {
        self.assets.retain(|hash, _| roots.contains(hash));
        self.bytes = self.assets.values().fold(0u64, |n, e| n.saturating_add(e.identity.bytes));
    }
    /// Reclaim dead weak cache records and surface private cleanup debt. Visible-store removal
    /// alone cannot release bytes or identities pinned by prepared payloads or reader leases.
    pub fn collect_garbage(&self) -> Result<(), Error> {
        let _operation = self.context.operation()?;
        self.context.prune_cache()?;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(workspace) = &self.context.0.workspace {
            workspace.cleanup()?;
        }
        Ok(())
    }
    pub(crate) fn referenced_snapshot(&self, hashes: &BTreeSet<Blake3>) -> Result<Self, Error> {
        let mut snapshot = Self::with_context(self.context.clone());
        for hash in hashes {
            let entry = self.assets.get(hash).ok_or_else(|| invalid("journal replay references a missing retained asset"))?;
            snapshot.bytes = snapshot.bytes.checked_add(entry.identity.bytes).ok_or_else(|| invalid("asset extent overflow"))?;
            snapshot.assets.insert(*hash, entry.clone());
        }
        Ok(snapshot)
    }
    pub(crate) fn from_index(index: &crate::zip_index::ArchiveIndex, context: AssetContext) -> Result<Self, Error> {
        let _operation = context.operation()?;
        context.prune_cache()?;
        let mut store = Self::with_context(context.clone());
        for entry in index.entries() {
            if let crate::zip_index::EntryName::Asset(hash) = entry.name {
                let identity = store.context.claim(hash, entry.data.byte_len())?;
                store.bytes = store.bytes.checked_add(identity.bytes).ok_or_else(|| invalid("asset extent overflow"))?;
                store
                    .assets
                    .insert(hash, Arc::new(Entry { identity, memory: None, _origin: Mutex::new(Some(entry.data.clone())), expected_crc: Some(entry.crc32) }));
            }
        }
        Ok(store)
    }
    pub fn import_stream(&mut self, source: &AssetRange, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<Blake3, Error> {
        if source.byte_len() > MAX_LARGE_ASSET_BYTES {
            return Err(invalid("asset exceeds 8 GiB limit"));
        }
        // Capability is checked before any source read, eager body construction or legacy call.
        if !self.context.supports_large_assets() {
            return Err(Error::new(ErrorKind::Unsupported, "this host requires bounded resident asset insertion; large stream import is unavailable"));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _operation = self.context.operation()?;
            self.context.prune_cache()?;
            self.pack_if_needed(cancel, metrics, 30)?;
            let workspace = self.context.0.workspace.as_ref().ok_or_else(unavailable)?;
            // Preflight count/aggregate before transfer without trusting an asserted digest.
            let (count, retained) = self.context.retained_usage()?;
            if count >= crate::MAX_ASSETS || retained.checked_add(source.byte_len()).is_none_or(|n| n > crate::asset_source::MAX_LARGE_TOTAL_ASSET_BYTES) {
                return Err(invalid("retained asset budget is full; release unused assets/readers before importing"));
            }
            let sealed = workspace.seal_copy(source, crate::disk_quota::DiskPool::Verified, None, cancel, metrics)?;
            let digest = sealed.digest();
            let hash = digest.hash;
            if self.assets.contains_key(&hash) {
                return Ok(hash);
            }
            let identity = self.context.claim(hash, digest.bytes)?;
            let range = sealed.range()?;
            let data = facts(range, digest.crc32, metrics)?;
            let entry = Arc::new(Entry { identity: identity.clone(), memory: None, _origin: Mutex::new(None), expected_crc: Some(digest.crc32) });
            let total = self.bytes.checked_add(digest.bytes).ok_or_else(|| invalid("asset extent overflow"))?;
            self.context.cache_verified(&identity, data)?;
            self.assets.insert(hash, entry);
            self.bytes = total;
            Ok(hash)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (cancel, metrics);
            Err(Error::new(ErrorKind::Unsupported, "large asset storage is unavailable on this host"))
        }
    }
    /// Consume an unknown-length native reader into private immutable storage. Any error may
    /// consume a prefix. Cancellation cannot interrupt an arbitrary blocking Read or sync.
    /// The hash is content identity only; references still require MIME/metadata validation.
    pub fn import_reader(&mut self, reader: &mut dyn std::io::Read, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<Blake3, Error> {
        if !self.context.supports_large_assets() {
            return Err(Error::new(ErrorKind::Unsupported, "unknown-length asset intake requires native private storage"));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let context = self.context.clone();
            let _operation = context.operation()?;
            cancel.check()?;
            context.prune_cache()?;
            let workspace = context.0.workspace.as_ref().ok_or_else(unavailable)?;
            let stage = workspace.quota().stage_operation()?;
            workspace.cleanup()?;
            self.pack_if_needed_with(cancel, metrics, 30, Some(&stage))?;
            let sealed = crate::reader_import::capture(workspace, &stage, reader, cancel, metrics)?;
            self.adopt_reader(sealed, cancel, metrics)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (reader, cancel, metrics);
            Err(Error::new(ErrorKind::Unsupported, "unknown-length asset intake is unavailable on this host"))
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn adopt_reader(&mut self, sealed: crate::native_assets::SealedBytes, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<Blake3, Error> {
        cancel.check()?;
        let digest = sealed.digest();
        if let Some(existing) = self.descriptor(&digest.hash) {
            if existing.bytes != digest.bytes {
                return Err(invalid("duplicate reader asset has inconsistent length"));
            }
            return Ok(digest.hash);
        }
        let (count, retained) = self.context.retained_usage()?;
        if count >= crate::MAX_ASSETS || retained.checked_add(digest.bytes).is_none_or(|n| n > crate::asset_source::MAX_LARGE_TOTAL_ASSET_BYTES) {
            return Err(invalid("retained asset budget is full; release unused assets/readers before importing"));
        }
        let identity = self.context.claim(digest.hash, digest.bytes)?;
        let total = self.bytes.checked_add(digest.bytes).ok_or_else(|| invalid("reader asset extent overflow"))?;
        let data = facts(sealed.range()?, digest.crc32, metrics)?;
        let entry = Arc::new(Entry { identity: identity.clone(), memory: None, _origin: Mutex::new(None), expected_crc: Some(digest.crc32) });
        cancel.check()?;
        self.context.cache_verified(&identity, data)?;
        self.assets.insert(digest.hash, entry);
        self.bytes = total;
        Ok(digest.hash)
    }
    /// Verify retained content before retiring a source. This proves content ownership,
    /// not caller-supplied MIME/metadata; actual references are validated separately.
    pub fn verify_retained(&self, hashes: &BTreeSet<Blake3>, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<(), Error> {
        if hashes.len() > crate::MAX_ASSETS {
            return Err(invalid("retained asset set exceeds its count limit"));
        }
        for hash in hashes {
            self.verified_content(hash, cancel, metrics)?;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.context.supports_large_assets() {
            // Packing is housekeeping: while a long copy holds the storage (#855) it waits for
            // the next import rather than holding this one up.
            match self.compact_before_output(cancel, metrics) {
                Err(error) if error.kind == ErrorKind::Busy && hashes.is_empty() => {}
                compacted => compacted?,
            }
        }
        Ok(())
    }
    pub fn in_context(&self, context: AssetContext, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<Self, Error> {
        if self.context.same(&context) {
            return Ok(self.clone());
        }
        if !context.supports_large_assets() && !self.is_resident_small() {
            return Err(Error::new(ErrorKind::Unsupported, "this host cannot adopt indexed or large assets"));
        }
        let mut out = Self::with_context(context);
        for (hash, entry) in &self.assets {
            if let Some(bytes) = &entry.memory {
                out.insert(bytes)?;
            } else {
                let data = self.verified_content(hash, cancel, metrics)?;
                let inserted = out.import_stream(data.range(), cancel, metrics)?;
                if inserted != *hash {
                    return Err(invalid("asset identity changed while adopting host storage"));
                }
            }
        }
        Ok(out)
    }
    /// Authenticate before emitting the first byte, then stream from the immutable lease.
    /// Output I/O/cancellation may leave a partial destination; hosts publish atomically.
    pub fn copy_verified_to(&self, asset: &AssetRef, output: &mut dyn std::io::Write, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<u64, Error> {
        let lease = self.open_verified(asset, cancel, metrics)?;
        let digest = crate::asset_source::copy_and_hash(lease.range(), output, cancel, metrics)?;
        if digest.hash != lease.hash() || digest.bytes != lease.byte_len() {
            return Err(invalid("verified asset changed during output"));
        }
        Ok(digest.bytes)
    }
    pub fn open_verified(&self, asset: &AssetRef, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<VerifiedAsset, Error> {
        let _operation = self.context.operation()?;
        self.open_verified_locked(
            asset,
            cancel,
            metrics,
            #[cfg(not(target_arch = "wasm32"))]
            None,
        )
    }
    fn open_verified_locked(
        &self,
        asset: &AssetRef,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
        #[cfg(not(target_arch = "wasm32"))] stage: Option<&crate::disk_quota::StageOperation<'_>>,
    ) -> Result<VerifiedAsset, Error> {
        let entry = self.assets.get(&asset.hash).ok_or_else(|| invalid("project references a missing asset"))?;
        let mut data = self.content_locked_with(
            entry,
            cancel,
            metrics,
            #[cfg(not(target_arch = "wasm32"))]
            stage,
        )?;
        if let Some(sniffed) = data.sniffed {
            let actual = sniffed.mime.split(';').next().unwrap_or_default();
            let expected = asset.mime.essence();
            // A style descriptor is text too (#167), so a text asset saved before styles were
            // sniffed apart still opens.
            let generic_text = (actual == "text/plain" || actual == nodecraft_media::style::MIME)
                && (expected.starts_with("text/") || expected == "application/json" || expected.ends_with("+json") || expected.ends_with("+xml"));
            if actual != expected && !generic_text {
                return Err(invalid("asset MIME does not match its stored bytes"));
            }
            if matches!(actual, "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp" | "image/tiff") && asset.meta.claims_image_header() {
                let meta = match data.image {
                    Some(meta) => meta,
                    None => {
                        #[cfg(any(test, feature = "validation-metrics"))]
                        crate::validation_metrics::record(|m| m.image_probes = m.image_probes.saturating_add(1));
                        let limits = nodecraft_media::Limits { max_input_bytes: MAX_LARGE_ASSET_BYTES, ..Default::default() };
                        nodecraft_media::probe_image_reader(data.range.reader(metrics.clone()), data.range.byte_len(), &limits)
                            .map_err(|_| invalid("cannot verify bounded asset image dimensions"))?
                    }
                };
                crate::assets::check_image_header(&asset.meta, &meta)?;
                data.image = Some(meta);
            }
        }
        self.context.cache_verified(&entry.identity, data.clone())?;
        Ok(VerifiedAsset { identity: entry.identity.clone(), data })
    }
    /// The bytes stored under `hash`, checked against it, without the MIME and header checks of
    /// [`Self::open_verified`]: for passing an asset on as it is (#700), not for reading it.
    pub fn verified_content(&self, hash: &Blake3, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<VerifiedAsset, Error> {
        let _operation = self.context.operation()?;
        let entry = self.assets.get(hash).ok_or_else(|| invalid("missing retained asset"))?;
        let data = self.content_locked(entry, cancel, metrics)?;
        Ok(VerifiedAsset { identity: entry.identity.clone(), data })
    }
    fn content_locked(&self, entry: &Entry, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<VerifiedData, Error> {
        self.content_locked_with(
            entry,
            cancel,
            metrics,
            #[cfg(not(target_arch = "wasm32"))]
            None,
        )
    }
    fn content_locked_with(
        &self,
        entry: &Entry,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
        #[cfg(not(target_arch = "wasm32"))] stage: Option<&crate::disk_quota::StageOperation<'_>>,
    ) -> Result<VerifiedData, Error> {
        cancel.check()?;
        if let Some(data) = self.context.cached(&entry.identity)? {
            if entry.expected_crc.is_some_and(|crc| crc != data.crc32) {
                return Err(invalid("asset CRC does not match its indexed descriptor"));
            }
            return Ok(data);
        }
        let data = if let Some(bytes) = &entry.memory {
            facts(AssetRange::new(Arc::new(MemorySource::new(bytes.clone())), 0, bytes.len() as u64)?, crc32fast::hash(bytes), metrics)?
        } else {
            if !self.context.supports_large_assets() {
                return Err(Error::new(ErrorKind::Unsupported, "this host cannot privately verify indexed assets"));
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.context.prune_cache()?;
                self.pack_if_needed_with(cancel, metrics, 30, stage)?;
                let mut origin = entry._origin.lock().map_err(|_| unavailable())?;
                let source = origin.as_ref().ok_or_else(|| invalid("indexed asset has no retained source"))?;
                let expected = crate::asset_source::StreamDigest {
                    hash: entry.identity.hash,
                    crc32: entry.expected_crc.ok_or_else(|| invalid("indexed asset is missing its CRC"))?,
                    bytes: entry.identity.bytes,
                };
                let workspace = self.context.0.workspace.as_ref().ok_or_else(unavailable)?;
                let sealed = match stage {
                    Some(stage) => workspace.seal_copy_in(stage, source, crate::disk_quota::DiskPool::Verified, Some(expected), cancel, metrics)?,
                    None => workspace.seal_copy(source, crate::disk_quota::DiskPool::Verified, Some(expected), cancel, metrics)?,
                };
                let data = facts(sealed.range()?, expected.crc32, metrics)?;
                // The complete verified data is prepared before relinquishing this origin.
                self.context.cache_verified(&entry.identity, data.clone())?;
                *origin = None;
                data
            }
            #[cfg(target_arch = "wasm32")]
            {
                return Err(Error::new(ErrorKind::Unsupported, "indexed asset verification is unavailable on this host"));
            }
        };
        self.context.cache_verified(&entry.identity, data.clone())?;
        Ok(data)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn compact_in_stage(
        &self,
        stage: &crate::disk_quota::StageOperation<'_>,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
    ) -> Result<(), Error> {
        let _operation = self.context.operation()?;
        stage.check(self.context.0.workspace.as_ref().ok_or_else(unavailable)?.quota())?;
        self.context.prune_cache()?;
        self.pack_if_needed_with(cancel, metrics, 24, Some(stage))
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn recovery_ranges(&self, ranges: &[(Blake3, AssetRange, u32)], cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<Self, Error> {
        let _operation = self.context.operation()?;
        self.context.prune_cache()?;
        let mut next = self.clone();
        for (hash, range, crc) in ranges {
            if next.contains(hash) || range.byte_len() > MAX_LARGE_ASSET_BYTES {
                return Err(invalid("duplicate or oversized recovery asset"));
            }
            // Even this private path cannot turn an asserted source/hash into proof.
            let actual = crate::asset_source::copy_and_hash(range, &mut std::io::sink(), cancel, metrics)?;
            if actual.hash != *hash || actual.crc32 != *crc || actual.bytes != range.byte_len() {
                return Err(invalid("captured recovery asset identity disagrees"));
            }
            let identity = self.context.claim(*hash, range.byte_len())?;
            let data = facts(range.clone(), *crc, metrics)?;
            let total = next.bytes.checked_add(range.byte_len()).ok_or_else(|| invalid("recovery asset extent overflow"))?;
            self.context.cache_verified(&identity, data)?;
            next.assets.insert(*hash, Arc::new(Entry { identity, memory: None, _origin: Mutex::new(None), expected_crc: Some(*crc) }));
            next.bytes = total;
        }
        Ok(next)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn verify_reference_in_stage(
        &self,
        stage: &crate::disk_quota::StageOperation<'_>,
        asset: &AssetRef,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
    ) -> Result<(), Error> {
        let _operation = self.context.operation()?;
        stage.check(self.context.0.workspace.as_ref().ok_or_else(unavailable)?.quota())?;
        self.open_verified_locked(asset, cancel, metrics, Some(stage)).map(|_| ())
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn verify_all_content_in_stage(
        &self,
        stage: &crate::disk_quota::StageOperation<'_>,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
    ) -> Result<(), Error> {
        let _operation = self.context.operation()?;
        stage.check(self.context.0.workspace.as_ref().ok_or_else(unavailable)?.quota())?;
        for entry in self.assets.values() {
            self.content_locked_with(entry, cancel, metrics, Some(stage))?;
        }
        Ok(())
    }
    /// Reserve headroom before freezing an output plan's verified reader leases. Once the
    /// plan pins them, those exact sources must remain alive through publication.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn compact_before_output(&self, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<(), Error> {
        let _operation = self.context.operation()?;
        self.context.prune_cache()?;
        self.pack_if_needed(cancel, metrics, 24)
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn pack_if_needed(&self, cancel: &TransferCancel, metrics: &StreamMetrics, threshold: u64) -> Result<(), Error> {
        self.pack_if_needed_with(cancel, metrics, threshold, None)
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn pack_if_needed_with(
        &self,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
        threshold: u64,
        stage: Option<&crate::disk_quota::StageOperation<'_>>,
    ) -> Result<(), Error> {
        let workspace = self.context.0.workspace.as_ref().ok_or_else(unavailable)?;
        if workspace.quota().backing_count()? < threshold {
            return Ok(());
        }
        let mut cache = self.context.0.cache.lock().map_err(|_| unavailable())?;
        // Only cache-exclusive source groups can be reclaimed by this relocation.
        // Existing leases may clone/drop concurrently, but cannot appear from a source
        // with no external owner while verification/cache access is serialized here.
        let mut groups: Vec<(&AssetRange, usize)> = Vec::new();
        groups.try_reserve_exact(cache.len()).map_err(|_| invalid("cannot allocate bounded backing inventory"))?;
        for record in cache.values() {
            if let Some((_, count)) = groups.iter_mut().find(|(range, _)| range.same_backing(&record.data.range)) {
                *count = count.saturating_add(1);
            } else {
                groups.push((&record.data.range, 1));
            }
        }
        let reclaimable = groups.iter().filter(|(range, count)| range.backing_references() == *count).count() as u64;
        let projected = workspace
            .quota()
            .backing_count()?
            .checked_sub(reclaimable)
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| invalid("asset backing inventory mismatch"))?;
        if projected >= 31 {
            return Err(Error::new(ErrorKind::Busy, "retired readers retain backing capacity; release them before importing"));
        }
        drop(groups);
        let mut records = Vec::new();
        records.try_reserve_exact(cache.len()).map_err(|_| invalid("cannot allocate bounded asset relocation plan"))?;
        // Identity pins are retained through cache replacement and lock release.
        for (hash, record) in cache.iter() {
            if let Some(identity) = record.identity.upgrade() {
                records.push((*hash, identity, record.data.clone()));
            }
        }
        if records.is_empty() {
            return Err(Error::new(ErrorKind::Busy, "asset backings are pinned; release readers before importing"));
        }
        let ranges = records.iter().map(|(_, _, data)| data.range.clone()).collect::<Vec<_>>();
        let sealed = match stage {
            Some(stage) => workspace.seal_pack_in(stage, &ranges, cancel, metrics)?,
            None => workspace.seal_pack(&ranges, cancel, metrics)?,
        };
        let packed = sealed.range()?;
        let mut next = BTreeMap::new();
        let mut offset = 0u64;
        for (hash, identity, data) in &records {
            let range = packed.slice(offset, identity.bytes)?;
            offset = offset.checked_add(identity.bytes).ok_or_else(|| invalid("asset pack extent overflow"))?;
            let actual = crate::asset_source::copy_and_hash(&range, &mut std::io::sink(), cancel, metrics)?;
            if actual.hash != *hash || actual.crc32 != data.crc32 || actual.bytes != identity.bytes {
                return Err(invalid("asset pack does not match verified identities"));
            }
            let mut data = data.clone();
            data.range = range;
            next.insert(*hash, CacheRecord { identity: Arc::downgrade(identity), data });
        }
        cancel.check()?;
        *cache = next;
        drop(cache);
        drop(records);
        drop(ranges);
        // Publication is complete. Concurrent prepared generations may consume the
        // released headroom; their own permits enforce the cap. Never return a new
        // admission error after swapping verified cache bindings.
        Ok(())
    }
}
fn facts(range: AssetRange, crc32: u32, metrics: &StreamMetrics) -> Result<VerifiedData, Error> {
    let prefix = range.slice(0, range.byte_len().min(64 * 1024))?.read_small(64 * 1024, metrics)?;
    #[cfg(any(test, feature = "validation-metrics"))]
    crate::validation_metrics::record(|m| m.sniffs = m.sniffs.saturating_add(1));
    Ok(VerifiedData { range, crc32, sniffed: nodecraft_media::sniff(&prefix), image: None })
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use crate::{disk_quota::DiskQuota, native_assets::NativeAssetWorkspace};
    fn setup() -> (tempfile::TempDir, NativeAssetWorkspace, AssetStore) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = NativeAssetWorkspace::new(dir.path(), DiskQuota::default()).unwrap();
        let store = AssetStore::with_context(AssetContext::native(workspace.clone()));
        (dir, workspace, store)
    }
    fn input(value: u8) -> AssetRange {
        AssetRange::new(Arc::new(MemorySource::new(Arc::from(vec![value; 4096]))), 0, 4096).unwrap()
    }
    #[test]
    fn a_small_insert_waits_in_memory_while_storage_is_busy_within_the_resident_limit() {
        let (_dir, workspace, mut store) = setup();
        let busy = store.clone();
        // A long copy holds the store's operation lock (#855).
        let held = busy.context().operation().unwrap();
        let hash = store.insert(b"a model's result").unwrap();
        assert_eq!(store.get(&hash), Some(&b"a model's result"[..]));
        assert_eq!(store.insert(b"a model's result").unwrap(), hash, "the same bytes once");
        drop(held);
        // Or the staging pool, held by a copy into another project's store.
        let stage = workspace.quota().stage_operation().unwrap();
        let staging = store.insert(b"while staging").unwrap();
        assert!(store.get(&staging).is_some());
        drop(stage);
        let held = busy.context().operation().unwrap();
        let full = vec![7u8; crate::MAX_ASSET_BYTES - 64];
        for value in 1..4u8 {
            let mut bytes = full.clone();
            bytes[0] = value;
            store.insert(&bytes).unwrap();
        }
        let mut last = full.clone();
        last[0] = 4;
        assert_eq!(store.insert(&last).unwrap_err().kind, ErrorKind::Busy, "past the resident limit it waits for storage");
        drop(held);
        // Free again: to disk, and what waited reads back whole.
        let later = store.insert(b"later").unwrap();
        assert!(store.contains(&later) && store.get(&later).is_none());
        let waited = store.verified_content(&hash, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        assert_eq!(waited.read_small(64, &StreamMetrics::default()).unwrap(), b"a model's result");
    }
    #[test]
    fn failed_private_pack_write_keeps_all_old_bindings_and_quota() {
        let (_dir, workspace, mut store) = setup();
        let mut hashes = Vec::new();
        for value in 64..94 {
            hashes.push(store.import_stream(&input(value), &TransferCancel::default(), &StreamMetrics::default()).unwrap());
        }
        let first = store.verified_content(&hashes[0], &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        let before = workspace.quota().usage().unwrap();
        let count = workspace.quota().backing_count().unwrap();
        workspace.fail_next_write_after(1).unwrap();
        assert!(store.import_stream(&input(120), &TransferCancel::default(), &StreamMetrics::default()).is_err());
        assert_eq!(workspace.quota().usage().unwrap(), before);
        assert_eq!(workspace.quota().backing_count().unwrap(), count);
        assert_eq!(store.len(), 30);
        let still = store.verified_content(&hashes[0], &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        assert!(first.range().same_backing(still.range()));
        assert_eq!(first.read_small(4096, &StreamMetrics::default()).unwrap(), vec![64; 4096]);
        drop(still);
        store.import_stream(&input(120), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        let relocated = store.verified_content(&hashes[0], &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        assert!(!first.range().same_backing(relocated.range()));
        assert_eq!(first.read_small(4096, &StreamMetrics::default()).unwrap(), relocated.read_small(4096, &StreamMetrics::default()).unwrap());
    }
    #[test]
    fn owned_cleanup_fault_retains_real_file_charge_until_exact_path_repair() {
        let (_dir, workspace, mut store) = setup();
        let baseline = workspace.quota().usage().unwrap();
        store.import_stream(&input(77), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        store.retain(&BTreeSet::new());
        workspace.fail_removals(2);
        assert!(store.collect_garbage().is_err());
        assert_eq!(store.context().retained_usage().unwrap(), (0, 0));
        assert_eq!(workspace.quota().cleanup_retained_bytes().unwrap(), 4096);
        assert_eq!(workspace.quota().usage().unwrap().verified, 4096);
        workspace.cleanup().unwrap();
        assert_eq!(workspace.quota().cleanup_retained_bytes().unwrap(), 0);
        assert_eq!(workspace.quota().usage().unwrap(), baseline);
    }
    #[test]
    fn short_and_mid_copy_cancelled_sources_leave_no_store_publication_or_charge() {
        struct Broken {
            cancel: Option<TransferCancel>,
        }
        impl crate::asset_source::AssetSource for Broken {
            fn byte_len(&self) -> u64 {
                1024 * 1024
            }
            fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
                if at > 0 {
                    return Ok(0);
                }
                out.fill(77);
                if let Some(cancel) = &self.cancel {
                    cancel.cancel();
                }
                Ok(out.len())
            }
        }
        for cancelled in [false, true] {
            let (_dir, workspace, mut store) = setup();
            let baseline = workspace.quota().usage().unwrap();
            let cancel = TransferCancel::default();
            let range = AssetRange::new(Arc::new(Broken { cancel: cancelled.then(|| cancel.clone()) }), 0, 1024 * 1024).unwrap();
            let metrics = StreamMetrics::default();
            assert!(store.import_stream(&range, &cancel, &metrics).is_err());
            assert_eq!(store.len(), 0);
            assert_eq!(store.context().retained_usage().unwrap(), (0, 0));
            assert_eq!(workspace.quota().usage().unwrap(), baseline);
            assert_eq!(metrics.stats().live_transfer_bytes, 0);
        }
    }
}

#[cfg(test)]
mod cache_lifetime147 {
    use super::*;
    fn reference(hash: Blake3) -> AssetRef {
        AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }
    }
    fn cache_size(context: &AssetContext) -> (usize, u64) {
        let cache = context.0.cache.lock().unwrap();
        (cache.len(), cache.values().map(|record| record.data.range.byte_len()).sum())
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn retired_cache_cannot_authorize_public_indexed_descriptor_with_forged_crc_and_new_length() {
        use std::io::{Cursor, Write};
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::NativeAssetWorkspace::new(dir.path(), crate::disk_quota::DiskQuota::default()).unwrap();
        let context = AssetContext::native(workspace.clone());
        let mut old = AssetStore::with_context(context.clone());
        let hash = old.insert(b"abc").unwrap();
        drop(old);
        assert_eq!(context.retained_usage().unwrap(), (0, 0));
        assert_eq!(cache_size(&context), (1, 3));
        let project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([42; 16]), "Public fixture").unwrap();
        let json = serde_json::to_vec(&serde_json::json!({"version":4,"epoch":0,"project":project})).unwrap();
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("project.json", options).unwrap();
        writer.write_all(&json).unwrap();
        writer.start_file(format!("assets/{hash}"), options).unwrap();
        writer.write_all(b"evil").unwrap();
        let mut bytes = writer.finish().unwrap().into_inner();
        let range = |bytes: Vec<u8>| {
            let len = bytes.len() as u64;
            AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 0, len).unwrap()
        };
        let index = crate::zip_index::ArchiveIndex::read(&range(bytes.clone()), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        let entry = index.entries().iter().find(|entry| matches!(entry.name, crate::zip_index::EntryName::Asset(_))).unwrap();
        let crc = crc32fast::hash(b"abc").to_le_bytes();
        bytes[entry.local_offset as usize + 14..entry.local_offset as usize + 18].copy_from_slice(&crc);
        let central = index.identity().directory_offset as usize + 46 + "project.json".len();
        bytes[central + 16..central + 20].copy_from_slice(&crc);
        let index = crate::zip_index::ArchiveIndex::read(&range(bytes), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        let candidate = crate::decode_indexed(&index, context.clone()).unwrap().assets;
        assert_eq!(candidate.descriptor(&hash).unwrap().bytes, 4);
        assert_eq!(cache_size(&context), (0, 0), "indexed admission reclaims retired proof before a new identity is admitted");
        assert!(candidate.open_verified(&reference(hash), &TransferCancel::default(), &StreamMetrics::default()).is_err());
        assert_eq!(cache_size(&context), (0, 0));
        assert_eq!(workspace.quota().usage().unwrap().verified, 0);
    }
    #[test]
    fn ordinary_resident_churn_prunes_every_admission_and_cache_producer_without_manual_gc() {
        let context = AssetContext::default();
        let mut store = AssetStore::with_context(context.clone());
        for n in 0..crate::MAX_ASSETS * 2 + 1 {
            let bytes = format!("bounded public fixture {n}");
            let hash = store.insert(bytes.as_bytes()).unwrap();
            assert_eq!(cache_size(&context), (0, 0), "ordinary insertion prunes the previous retired body");
            let lease = if n % 2 == 0 {
                store.open_verified(&reference(hash), &TransferCancel::default(), &StreamMetrics::default()).unwrap()
            } else {
                store.verified_content(&hash, &TransferCancel::default(), &StreamMetrics::default()).unwrap()
            };
            assert_eq!(lease.byte_len(), bytes.len() as u64);
            drop(lease);
            store.retain(&BTreeSet::new());
            assert_eq!(context.retained_usage().unwrap(), (0, 0));
            assert_eq!(cache_size(&context), (1, bytes.len() as u64));
        }
    }
    #[test]
    fn active_resident_readers_keep_the_aggregate_budget_and_bytes_until_final_close() {
        use std::io::Read;
        let context = AssetContext::default();
        let mut store = AssetStore::with_context(context.clone());
        let mut readers = Vec::new();
        let length = 8 * 1024 * 1024;
        for value in b'A'..=b'F' {
            let hash = store.insert(&vec![value; length]).unwrap();
            let lease = store.open_verified(&reference(hash), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
            readers.push(lease.reader(StreamMetrics::default()));
            drop(lease);
            store.retain(&BTreeSet::new());
        }
        assert_eq!(context.retained_usage().unwrap(), (6, crate::MAX_TOTAL_ASSET_BYTES as u64));
        assert!(store.insert(&vec![b'G'; length]).is_err());
        assert!(store.is_empty());
        for (n, reader) in readers.iter_mut().enumerate() {
            let mut head = [0];
            reader.read_exact(&mut head).unwrap();
            assert_eq!(head, [b'A' + n as u8]);
        }
        drop(readers.remove(0));
        let hash = store.insert(&vec![b'G'; length]).unwrap();
        let lease = store.open_verified(&reference(hash), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        assert_eq!(lease.byte_len(), length as u64);
        assert_eq!(context.retained_usage().unwrap(), (6, crate::MAX_TOTAL_ASSET_BYTES as u64));
        assert_eq!(cache_size(&context), (6, crate::MAX_TOTAL_ASSET_BYTES as u64));
        drop((lease, readers));
        store.retain(&BTreeSet::new());
        store.insert(b"after readers").unwrap();
        assert_eq!(cache_size(&context), (0, 0));
    }
    #[test]
    fn identical_resident_admissions_share_one_canonical_body_and_one_charge() {
        let context = AssetContext::default();
        let mut a = AssetStore::with_context(context.clone());
        let mut b = AssetStore::with_context(context.clone());
        let body = vec![b'A'; 4096];
        let h = a.insert(&body).unwrap();
        assert_eq!(b.insert(&body).unwrap(), h);
        let left = a.assets.get(&h).unwrap().memory.as_ref().unwrap();
        let right = b.assets.get(&h).unwrap().memory.as_ref().unwrap();
        assert!(Arc::ptr_eq(left, right));
        assert_eq!(context.retained_usage().unwrap(), (1, 4096));
        let pinned = b.open_verified(&reference(h), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        drop((a, b));
        assert_eq!(pinned.read_small(4096, &StreamMetrics::default()).unwrap(), body);
        assert_eq!(context.retained_usage().unwrap(), (1, 4096));
        drop(pinned);
        assert_eq!(context.retained_usage().unwrap(), (0, 0));
    }
    #[test]
    fn cache_hit_checks_live_exact_identity_and_range_length_even_if_internal_record_is_inconsistent() {
        let mut store = AssetStore::default();
        let hash = store.insert(b"abc").unwrap();
        store.open_verified(&reference(hash), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        {
            let mut cache = store.context.0.cache.lock().unwrap();
            let record = cache.get_mut(&hash).unwrap();
            record.data.range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"ab".as_slice()))), 0, 2).unwrap();
        }
        assert!(store.open_verified(&reference(hash), &TransferCancel::default(), &StreamMetrics::default()).is_err());
    }
}

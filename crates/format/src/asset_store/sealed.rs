//! An asset sealed into a host's private storage away from the store that takes it (#842): the
//! slow copy and hash run on another thread, on a clone of the store, and the store adopts the
//! result in a moment.
use super::{AssetStore, Entry};
use crate::{
    asset_context::{AssetContext, AssetIdentity},
    asset_source::{AssetRange, MAX_LARGE_ASSET_BYTES, StreamMetrics, TransferCancel},
    invalid,
};
use nodecraft_types::{Blake3, Error, ErrorKind};
use std::sync::{Arc, Mutex};

/// Bytes copied into private storage and checked, charged to their context, not yet in a store.
/// Dropped unadopted, they are let go like any asset no store holds.
pub struct SealedAsset {
    identity: Arc<AssetIdentity>,
    crc32: u32,
    context: AssetContext,
}

impl std::fmt::Debug for SealedAsset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedAsset").field("bytes", &self.identity.bytes).finish_non_exhaustive()
    }
}

impl SealedAsset {
    /// What the bytes hash to.
    pub fn hash(&self) -> Blake3 {
        self.identity.hash
    }

    pub fn byte_len(&self) -> u64 {
        self.identity.bytes
    }
}

impl AssetStore {
    /// Copies `source` into this store's private storage and checks it, as
    /// [`Self::import_stream`] does, without adding it: [`Self::adopt_sealed`] does, on this
    /// store or any other of its context. It takes as long as an import; run it on a clone.
    /// Another verification in the context meanwhile is told it is busy, as during an import.
    pub fn seal(&self, source: &AssetRange, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<SealedAsset, Error> {
        if source.byte_len() > MAX_LARGE_ASSET_BYTES {
            return Err(invalid("asset exceeds 8 GiB limit"));
        }
        if !self.context.supports_large_assets() {
            return Err(Error::new(ErrorKind::Unsupported, "large asset storage is unavailable on this host"));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _operation = self.context.operation()?;
            self.context.prune_cache()?;
            self.pack_if_needed(cancel, metrics, 30)?;
            let workspace = self.context.0.workspace.as_ref().ok_or_else(crate::asset_context::unavailable)?;
            let (count, retained) = self.context.retained_usage()?;
            if count >= crate::MAX_ASSETS || retained.checked_add(source.byte_len()).is_none_or(|n| n > crate::asset_source::MAX_LARGE_TOTAL_ASSET_BYTES) {
                return Err(invalid("retained asset budget is full; release unused assets/readers before importing"));
            }
            let sealed = workspace.seal_copy(source, crate::disk_quota::DiskPool::Verified, None, cancel, metrics)?;
            let digest = sealed.digest();
            let identity = self.context.claim(digest.hash, digest.bytes)?;
            // Already held, the asset keeps the copy it has.
            if self.context.cached(&identity)?.is_none() {
                let data = super::facts(sealed.range()?, digest.crc32, metrics)?;
                self.context.cache_verified(&identity, data)?;
            }
            Ok(SealedAsset { identity, crc32: digest.crc32, context: self.context.clone() })
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (cancel, metrics);
            Err(Error::new(ErrorKind::Unsupported, "large asset storage is unavailable on this host"))
        }
    }

    /// Adds an asset [`Self::seal`] made in this store's context; one it holds already stays
    /// as it is.
    pub fn adopt_sealed(&mut self, sealed: SealedAsset) -> Result<Blake3, Error> {
        if !self.context.same(&sealed.context) {
            return Err(Error::new(ErrorKind::Conflict, "the asset was sealed for another project's storage"));
        }
        let hash = sealed.identity.hash;
        if let Some(entry) = self.assets.get(&hash) {
            if entry.identity.bytes != sealed.identity.bytes {
                return Err(invalid("one asset identity has inconsistent lengths"));
            }
            return Ok(hash);
        }
        let total = self.bytes.checked_add(sealed.identity.bytes).ok_or_else(|| invalid("asset extent overflow"))?;
        let entry = Entry { identity: sealed.identity, memory: None, _origin: Mutex::new(None), expected_crc: Some(sealed.crc32) };
        self.assets.insert(hash, Arc::new(entry));
        self.bytes = total;
        Ok(hash)
    }
}

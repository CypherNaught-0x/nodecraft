//! One host-owned retention and verification context shared by stores and reader leases.
use crate::{
    asset_source::{AssetRange, MAX_LARGE_TOTAL_ASSET_BYTES},
    invalid,
};
use nodecraft_types::{Blake3, Error, ErrorKind};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, Weak},
};

struct Retained {
    bytes: u64,
    identity: Weak<AssetIdentity>,
}
#[derive(Default)]
struct Ledger {
    retained: BTreeMap<Blake3, Retained>,
    bytes: u64,
}
pub(crate) struct AssetIdentity {
    pub hash: Blake3,
    pub bytes: u64,
    ledger: Weak<Mutex<Ledger>>,
    resident: Mutex<Option<Arc<[u8]>>>,
}
impl AssetIdentity {
    /// One resident body per charged identity, shared by stores/readers in this context.
    pub(crate) fn resident_bytes(&self, bytes: &[u8]) -> Result<Arc<[u8]>, Error> {
        if bytes.len() as u64 != self.bytes || crate::hash(bytes) != self.hash {
            return Err(invalid("resident bytes disagree with their canonical identity"));
        }
        let mut resident = self.resident.lock().map_err(|_| unavailable())?;
        if let Some(existing) = &*resident {
            return Ok(existing.clone());
        }
        let owned: Arc<[u8]> = Arc::from(bytes);
        *resident = Some(owned.clone());
        Ok(owned)
    }
}
impl Drop for AssetIdentity {
    fn drop(&mut self) {
        if let Some(ledger) = self.ledger.upgrade()
            && let Ok(mut ledger) = ledger.lock()
            && ledger.retained.get(&self.hash).is_some_and(|entry| entry.identity.strong_count() == 0)
            && let Some(entry) = ledger.retained.remove(&self.hash)
        {
            // A poisoned/mismatched ledger never admits further work or invents capacity.
            ledger.bytes = ledger.bytes.saturating_sub(entry.bytes);
        }
    }
}
#[derive(Clone)]
pub(crate) struct VerifiedData {
    pub range: AssetRange,
    pub crc32: u32,
    pub sniffed: Option<nodecraft_media::Sniffed>,
    pub image: Option<nodecraft_media::ImageMeta>,
}
pub(crate) struct CacheRecord {
    pub identity: Weak<AssetIdentity>,
    pub data: VerifiedData,
}
pub(crate) struct ContextState {
    ledger: Arc<Mutex<Ledger>>,
    pub operation: Mutex<()>,
    pub cache: Mutex<BTreeMap<Blake3, CacheRecord>>,
    #[cfg(not(target_arch = "wasm32"))]
    pub workspace: Option<crate::native_assets::NativeAssetWorkspace>,
}
/// Platform capability and unique pinned-identity ownership, not content verification authority.
/// Memory/default hosts retain their small byte path. Native hosts explicitly inject an owned
/// private workspace; browser large-asset absence is a fallible capability boundary.
#[derive(Clone)]
pub struct AssetContext(pub(crate) Arc<ContextState>);
impl Default for AssetContext {
    fn default() -> Self {
        Self(Arc::new(ContextState {
            ledger: Arc::new(Mutex::new(Ledger::default())),
            operation: Mutex::new(()),
            cache: Mutex::new(BTreeMap::new()),
            #[cfg(not(target_arch = "wasm32"))]
            workspace: None,
        }))
    }
}
impl std::fmt::Debug for AssetContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetContext").field("streaming", &self.supports_large_assets()).finish_non_exhaustive()
    }
}
impl AssetContext {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn native(workspace: crate::native_assets::NativeAssetWorkspace) -> Self {
        Self(Arc::new(ContextState {
            ledger: Arc::new(Mutex::new(Ledger::default())),
            operation: Mutex::new(()),
            cache: Mutex::new(BTreeMap::new()),
            workspace: Some(workspace),
        }))
    }
    pub fn supports_large_assets(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.0.workspace.is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }
    pub fn storage_usage(&self) -> Result<crate::disk_quota::DiskUsage, Error> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(workspace) = &self.0.workspace {
            return workspace.quota().usage();
        }
        Ok(crate::disk_quota::DiskUsage::default())
    }
    pub fn retained_usage(&self) -> Result<(usize, u64), Error> {
        let ledger = self.0.ledger.lock().map_err(|_| unavailable())?;
        Ok((ledger.retained.len(), ledger.bytes))
    }
    fn retained_limit(&self) -> u64 {
        if self.supports_large_assets() { MAX_LARGE_TOTAL_ASSET_BYTES } else { crate::MAX_TOTAL_ASSET_BYTES as u64 }
    }
    pub(crate) fn claim(&self, hash: Blake3, bytes: u64) -> Result<Arc<AssetIdentity>, Error> {
        let mut ledger = self.0.ledger.lock().map_err(|_| unavailable())?;
        if let Some(entry) = ledger.retained.get(&hash) {
            if entry.bytes != bytes {
                return Err(invalid("one asset identity has inconsistent lengths"));
            }
            return entry.identity.upgrade().ok_or_else(|| Error::new(ErrorKind::Busy, "asset retirement is in progress; retry"));
        }
        let total = ledger
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= self.retained_limit())
            .ok_or_else(|| invalid("retained assets exceed this host's logical byte limit"))?;
        if bytes > crate::asset_source::MAX_LARGE_ASSET_BYTES || ledger.retained.len() >= crate::MAX_ASSETS {
            return Err(invalid("retained asset size or count exceeds limit"));
        }
        let identity = Arc::new(AssetIdentity { hash, bytes, ledger: Arc::downgrade(&self.0.ledger), resident: Mutex::new(None) });
        ledger.retained.insert(hash, Retained { bytes, identity: Arc::downgrade(&identity) });
        ledger.bytes = total;
        Ok(identity)
    }
    pub(crate) fn operation(&self) -> Result<MutexGuard<'_, ()>, Error> {
        self.0.operation.try_lock().map_err(|_| Error::new(ErrorKind::Busy, "asset verification or relocation is in progress; retry"))
    }
    /// Called under operation ownership. Weak dead cache entries never own logical leases;
    /// their backing bytes remain charged until dropping the actual private source succeeds.
    pub(crate) fn prune_cache(&self) -> Result<(), Error> {
        self.0.cache.lock().map_err(|_| unavailable())?.retain(|_, record| record.identity.strong_count() != 0);
        Ok(())
    }
    /// Called under operation ownership. Proof belongs to one live logical identity,
    /// never a hash-key cache slot left behind by an earlier retired allocation.
    pub(crate) fn cached(&self, identity: &Arc<AssetIdentity>) -> Result<Option<VerifiedData>, Error> {
        self.prune_cache()?;
        let cache = self.0.cache.lock().map_err(|_| unavailable())?;
        let Some(record) = cache.get(&identity.hash) else {
            return Ok(None);
        };
        let owner = record.identity.upgrade().ok_or_else(|| invalid("cached asset owner retired during verification"))?;
        if !Arc::ptr_eq(&owner, identity) || owner.hash != identity.hash || owner.bytes != identity.bytes || record.data.range.byte_len() != identity.bytes {
            return Err(invalid("cached asset proof does not match its live exact-length identity"));
        }
        Ok(Some(record.data.clone()))
    }
    /// All cache producers share this bounded publication path under the operation gate.
    /// Dead bytes cannot accumulate across resident churn or indexed owner retirement.
    pub(crate) fn cache_verified(&self, identity: &Arc<AssetIdentity>, data: VerifiedData) -> Result<(), Error> {
        if data.range.byte_len() != identity.bytes {
            return Err(invalid("verified asset extent does not match its identity"));
        }
        let mut cache = self.0.cache.lock().map_err(|_| unavailable())?;
        cache.retain(|_, record| record.identity.strong_count() != 0);
        if let Some(old) = cache.get(&identity.hash) {
            let owner = old.identity.upgrade().ok_or_else(|| invalid("cached asset owner retired during publication"))?;
            if !Arc::ptr_eq(&owner, identity) || owner.bytes != identity.bytes || old.data.range.byte_len() != identity.bytes {
                return Err(invalid("cached asset proof belongs to another identity or extent"));
            }
        } else if cache.len() >= crate::MAX_ASSETS {
            return Err(invalid("verified asset cache count limit reached"));
        }
        let bytes = cache.iter().filter(|(hash, _)| **hash != identity.hash).try_fold(identity.bytes, |bytes, (_, record)| {
            bytes
                .checked_add(record.data.range.byte_len())
                .filter(|n| *n <= self.retained_limit())
                .ok_or_else(|| invalid("verified asset cache exceeds this host's byte limit"))
        })?;
        if bytes > self.retained_limit() {
            return Err(invalid("verified asset cache exceeds this host's byte limit"));
        }
        cache.insert(identity.hash, CacheRecord { identity: Arc::downgrade(identity), data });
        Ok(())
    }
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
pub(crate) fn unavailable() -> Error {
    Error::new(ErrorKind::Conflict, "asset ownership or verification lock is unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hash(n: u64) -> Blake3 {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        Blake3::from_bytes(bytes)
    }
    #[test]
    fn unique_pinned_identities_share_one_charge_through_last_reader() {
        let context = AssetContext::default();
        let identity = context.claim(hash(1), 100).unwrap();
        let same = context.claim(hash(1), 100).unwrap();
        assert!(Arc::ptr_eq(&identity, &same));
        assert_eq!(context.retained_usage().unwrap(), (1, 100));
        let reader = identity.clone();
        drop((identity, same));
        assert_eq!(context.retained_usage().unwrap(), (1, 100));
        drop(reader);
        assert_eq!(context.retained_usage().unwrap(), (0, 0));
    }
    #[test]
    fn clone_count_and_byte_admission_cannot_hide_retired_pins() {
        let context = AssetContext::default();
        let clone = context.clone();
        let mut identities = Vec::new();
        for n in 0..crate::MAX_ASSETS {
            identities.push(context.claim(hash(n as u64), 1).unwrap());
        }
        assert!(clone.claim(hash(9999), 0).is_err());
        assert!(clone.claim(hash(1), 2).is_err());
        drop(identities);
        assert_eq!(clone.retained_usage().unwrap(), (0, 0));
        assert!(context.claim(hash(99), crate::MAX_TOTAL_ASSET_BYTES as u64 + 1).is_err());
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dir = tempfile::tempdir().unwrap();
            let native = AssetContext::native(crate::NativeAssetWorkspace::new(dir.path(), crate::disk_quota::DiskQuota::default()).unwrap());
            let mut large = Vec::new();
            for n in 0..8 {
                large.push(native.claim(hash(n), crate::asset_source::MAX_LARGE_ASSET_BYTES).unwrap());
            }
            assert!(native.claim(hash(99), 1).is_err());
            drop(large);
        }
        assert_eq!(context.retained_usage().unwrap(), (0, 0));
    }
}

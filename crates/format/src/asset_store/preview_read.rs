//! One-identity preview input. Capture does not touch a source; consumption is bounded and
//! deliberately does not seal/pack/publish the store's global verified cache.
use super::*;
use std::{io::Read, sync::TryLockError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewReadError {
    Missing,
    TooLarge,
    Busy,
    Cancelled,
    Unavailable,
    Corrupt,
    Unreadable,
    Capacity,
}

/// Pins one logical identity and its source context, not a clone of the complete store.
/// The source range is selected atomically with relocation when the worker consumes it.
pub struct PreviewAssetRead {
    entry: Arc<Entry>,
    context: AssetContext,
    mime: String,
    width: Option<u32>,
    height: Option<u32>,
}
impl AssetStore {
    /// Constant-size ownership capture after a descriptor lookup. No locks, reads, hashing,
    /// media probing or body allocation occur here. The existing image decoder cap applies.
    pub fn preview_read(&self, asset: &AssetRef) -> Result<PreviewAssetRead, PreviewReadError> {
        let entry = self.assets.get(&asset.hash).ok_or(PreviewReadError::Missing)?;
        if entry.identity.bytes > nodecraft_media::Limits::default().max_input_bytes {
            return Err(PreviewReadError::TooLarge);
        }
        let essence = asset.mime.essence();
        if essence.len() > 1024 {
            return Err(PreviewReadError::Corrupt);
        }
        let mut mime = String::new();
        mime.try_reserve_exact(essence.len()).map_err(|_| PreviewReadError::Capacity)?;
        if mime.capacity() > 1024 {
            return Err(PreviewReadError::Capacity);
        }
        mime.push_str(essence);
        Ok(PreviewAssetRead { entry: entry.clone(), context: self.context.clone(), mime, width: asset.meta.width, height: asset.meta.height })
    }
}
impl PreviewAssetRead {
    pub fn hash(&self) -> Blake3 {
        self.entry.identity.hash
    }
    pub fn byte_len(&self) -> u64 {
        self.entry.identity.bytes
    }
    pub fn is_empty(&self) -> bool {
        self.byte_len() == 0
    }
    /// Existing insertion-verified immutable resident bytes only. Never resolves a lazy source.
    pub fn resident(&self) -> Option<&[u8]> {
        self.entry.memory.as_deref()
    }

    fn range(&self) -> Result<(AssetRange, Option<u32>), PreviewReadError> {
        if let Some(bytes) = &self.entry.memory {
            let range = AssetRange::new(Arc::new(MemorySource::new(bytes.clone())), 0, self.byte_len()).map_err(|_| PreviewReadError::Corrupt)?;
            return Ok((range, self.entry.expected_crc));
        }
        if !self.context.supports_large_assets() {
            return Err(PreviewReadError::Unavailable);
        }
        let _operation = match self.context.0.operation.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Err(PreviewReadError::Busy),
            Err(TryLockError::Poisoned(_)) => return Err(PreviewReadError::Unavailable),
        };
        // No pruning/global inventory: select exactly this identity under relocation authority.
        let cache = match self.context.0.cache.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Err(PreviewReadError::Busy),
            Err(TryLockError::Poisoned(_)) => return Err(PreviewReadError::Unavailable),
        };
        if let Some(record) = cache.get(&self.hash()) {
            let owner = record.identity.upgrade().ok_or(PreviewReadError::Corrupt)?;
            if !Arc::ptr_eq(&owner, &self.entry.identity)
                || owner.bytes != self.byte_len()
                || record.data.range.byte_len() != self.byte_len()
                || self.entry.expected_crc.is_some_and(|crc| crc != record.data.crc32)
            {
                return Err(PreviewReadError::Corrupt);
            }
            return Ok((record.data.range.clone(), Some(record.data.crc32)));
        }
        drop(cache);
        let origin = match self.entry._origin.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Err(PreviewReadError::Busy),
            Err(TryLockError::Poisoned(_)) => return Err(PreviewReadError::Unavailable),
        };
        let range = origin.as_ref().ok_or(PreviewReadError::Unavailable)?;
        if range.byte_len() != self.byte_len() {
            return Err(PreviewReadError::Corrupt);
        }
        Ok((range.clone(), self.entry.expected_crc))
        // The range clone pins its old backing after these guards are released.
    }

    /// Checks the image's size like open_verified, without publishing cache proof. Its other
    /// header facts (#393) were checked when the store was opened or the reference saved.
    /// Other media (video, #105) are checked for their type only.
    /// Call under the consumer's panic guard, including in the resident inline fallback.
    pub fn validate_image(&self, bytes: &[u8]) -> Result<(), PreviewReadError> {
        if bytes.len() as u64 != self.byte_len() {
            return Err(PreviewReadError::Corrupt);
        }
        if let Some(sniffed) = nodecraft_media::sniff(bytes) {
            if sniffed.mime.split(';').next().unwrap_or_default() != self.mime {
                return Err(PreviewReadError::Corrupt);
            }
            // Only images are probed for their size; a video's is checked by its own service.
            if self.mime.starts_with("image/") && (self.width.is_some() || self.height.is_some()) {
                let meta = nodecraft_media::probe_image(bytes, &nodecraft_media::Limits::default()).map_err(|_| PreviewReadError::Corrupt)?;
                if self.width.is_some_and(|n| n != meta.width) || self.height.is_some_and(|n| n != meta.height) {
                    return Err(PreviewReadError::Corrupt);
                }
            }
        }
        Ok(())
    }

    /// Worker consumption: at most one decoder-capped Vec and <=64KiB read calls. No retries
    /// (AssetReader maps source errors to Other), global sealing, relocation or cache mutation.
    pub fn read_verified(&self, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<Vec<u8>, PreviewReadError> {
        cancel.check().map_err(|_| PreviewReadError::Cancelled)?;
        let cap = usize::try_from(nodecraft_media::Limits::default().max_input_bytes).map_err(|_| PreviewReadError::TooLarge)?;
        let length = usize::try_from(self.byte_len()).map_err(|_| PreviewReadError::TooLarge)?;
        if length > cap {
            return Err(PreviewReadError::TooLarge);
        }
        let (range, expected_crc) = self.range()?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|_| PreviewReadError::Capacity)?;
        if bytes.capacity() > cap {
            return Err(PreviewReadError::Capacity);
        }
        bytes.resize(length, 0);
        let mut reader = range.reader(metrics.clone());
        let mut hash = blake3::Hasher::new();
        let mut crc = crc32fast::Hasher::new();
        let mut at = 0usize;
        while at < length {
            cancel.check().map_err(|_| PreviewReadError::Cancelled)?;
            let end = at.checked_add((length - at).min(64 * 1024)).ok_or(PreviewReadError::Corrupt)?;
            let out = bytes.get_mut(at..end).ok_or(PreviewReadError::Corrupt)?;
            let n = reader.read(out).map_err(|_| PreviewReadError::Unreadable)?;
            if n == 0 || n > end - at {
                return Err(PreviewReadError::Unreadable);
            }
            let data = out.get(..n).ok_or(PreviewReadError::Corrupt)?;
            hash.update(data);
            crc.update(data);
            at = at.checked_add(n).ok_or(PreviewReadError::Corrupt)?;
        }
        cancel.check().map_err(|_| PreviewReadError::Cancelled)?;
        if Blake3::from_bytes(*hash.finalize().as_bytes()) != self.hash() || expected_crc.is_some_and(|n| n != crc.finalize()) {
            return Err(PreviewReadError::Corrupt);
        }
        self.validate_image(&bytes)?;
        cancel.check().map_err(|_| PreviewReadError::Cancelled)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests;

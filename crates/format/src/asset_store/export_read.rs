//! One export's private verifier. The live store's relocation authority is held only while
//! selecting a pinned source, never while reading, hashing, probing or staging its body.
use super::*;

impl AssetStore {
    /// Verify a reference for a background export using a separate, caller-owned scratch
    /// workspace. At most one asset (8 GiB) is staged. The canonical identity and old source
    /// remain pinned, but no proof or storage is published into the live session's cache.
    /// Busy source capture is retryable by a cancellable worker; it performs no source reads.
    pub fn open_export_verified(
        &self,
        asset: &AssetRef,
        scratch: &crate::NativeAssetWorkspace,
        cancel: &TransferCancel,
        metrics: &StreamMetrics,
    ) -> Result<VerifiedAsset, Error> {
        cancel.check()?;
        let entry = self.assets.get(&asset.hash).ok_or_else(|| invalid("project references a missing asset"))?;
        let (cached, origin) = {
            let _operation = self.context.operation()?;
            let cached = self.context.cached(&entry.identity)?;
            let origin = if cached.is_none() && entry.memory.is_none() {
                if !self.context.supports_large_assets() {
                    return Err(Error::new(ErrorKind::Unsupported, "this host cannot privately verify indexed assets"));
                }
                entry._origin.lock().map_err(|_| unavailable())?.clone()
            } else {
                None
            };
            (cached, origin)
        };
        let mut data = match cached {
            Some(data) => data,
            None => match &entry.memory {
                Some(bytes) => facts(AssetRange::new(Arc::new(MemorySource::new(bytes.clone())), 0, entry.identity.bytes)?, crc32fast::hash(bytes), metrics)?,
                None => {
                    let source = origin.ok_or_else(|| invalid("indexed asset has no retained source"))?;
                    let expected = crate::asset_source::StreamDigest {
                        hash: entry.identity.hash,
                        crc32: entry.expected_crc.ok_or_else(|| invalid("indexed asset is missing its CRC"))?,
                        bytes: entry.identity.bytes,
                    };
                    let sealed = scratch.seal_copy(&source, crate::disk_quota::DiskPool::Verified, Some(expected), cancel, metrics)?;
                    facts(sealed.range()?, expected.crc32, metrics)?
                }
            },
        };
        if entry.expected_crc.is_some_and(|crc| crc != data.crc32) {
            return Err(invalid("asset CRC does not match its indexed descriptor"));
        }
        self.check_reference(asset, &mut data, metrics)?;
        cancel.check()?;
        Ok(VerifiedAsset { identity: entry.identity.clone(), data })
    }
}

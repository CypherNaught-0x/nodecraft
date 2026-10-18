//! Immutable complete recovery checkpoint plans; no caller supplied digest is trusted.
use crate::asset_source::{StreamMetrics, TransferCancel};
use crate::{ArchivePlan, StorageCapability, invalid};
use nodecraft_types::{Blake3, Error};
use std::{collections::BTreeMap, io::Write};
const MAGIC: &[u8; 8] = b"NCRJ0004";
const HEADER_DOMAIN: &[u8] = b"nodecraft.recovery.header.v4";
const IDENTITY_DOMAIN: &[u8] = b"nodecraft.recovery.identity.v4";
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_header(header: &[u8; 80]) -> Result<(u64, Blake3, Blake3), Error> {
    if header.get(..8) != Some(MAGIC.as_slice()) {
        return Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Unsupported, "unsupported recovery checkpoint version"));
    }
    let length = u64::from_le_bytes(
        header.get(8..16).ok_or_else(|| invalid("truncated checkpoint header"))?.try_into().map_err(|_| invalid("invalid checkpoint extent"))?,
    );
    if length > crate::asset_source::MAX_LARGE_ARCHIVE_BYTES {
        return Err(invalid("checkpoint archive exceeds 65 GiB"));
    }
    let digest = Blake3::from_bytes(
        header.get(16..48).ok_or_else(|| invalid("truncated checkpoint digest"))?.try_into().map_err(|_| invalid("invalid checkpoint digest"))?,
    );
    let mut hash = blake3::Hasher::new();
    hash.update(HEADER_DOMAIN);
    hash.update(header.get(..48).ok_or_else(|| invalid("truncated checkpoint header"))?);
    if Some(hash.finalize().as_bytes().as_slice()) != header.get(48..80) {
        return Err(invalid("checkpoint header checksum mismatch"));
    }
    let mut hash = blake3::Hasher::new();
    hash.update(IDENTITY_DOMAIN);
    hash.update(&length.to_le_bytes());
    hash.update(digest.as_bytes());
    Ok((length, digest, Blake3::from_bytes(*hash.finalize().as_bytes())))
}
#[derive(Clone)]
pub struct CheckpointPlan {
    archive: ArchivePlan,
    header: [u8; 80],
    identity: Blake3,
    small: Option<Vec<u8>>,
    #[cfg(not(target_arch = "wasm32"))]
    native: Option<std::sync::Arc<crate::storage::native::durability::Publication>>,
}
impl CheckpointPlan {
    pub fn new(archive: ArchivePlan) -> Result<Self, Error> {
        let length = archive.byte_len().to_le_bytes();
        let digest = archive.digest().hash;
        let mut header = [0; 80];
        header.get_mut(..8).ok_or_else(|| invalid("invalid checkpoint header"))?.copy_from_slice(MAGIC);
        header.get_mut(8..16).ok_or_else(|| invalid("invalid checkpoint header"))?.copy_from_slice(&length);
        header.get_mut(16..48).ok_or_else(|| invalid("invalid checkpoint header"))?.copy_from_slice(digest.as_bytes());
        let mut hash = blake3::Hasher::new();
        hash.update(HEADER_DOMAIN);
        hash.update(header.get(..48).ok_or_else(|| invalid("invalid checkpoint header"))?);
        header.get_mut(48..80).ok_or_else(|| invalid("invalid checkpoint header"))?.copy_from_slice(hash.finalize().as_bytes());
        let mut hash = blake3::Hasher::new();
        hash.update(IDENTITY_DOMAIN);
        hash.update(&length);
        hash.update(digest.as_bytes());
        let mut identity = Blake3::from_bytes(*hash.finalize().as_bytes());
        let small = if archive.capability() == StorageCapability::SmallV1 {
            let bytes = archive.small_bytes()?;
            let size = bytes.len().checked_add(16).ok_or_else(|| invalid("legacy checkpoint extent overflow"))?;
            let mut small = Vec::new();
            small.try_reserve_exact(size).map_err(|_| invalid("cannot allocate bounded legacy checkpoint"))?;
            small.extend_from_slice(b"NCRJ0001");
            small.extend_from_slice(&length);
            small.extend_from_slice(bytes);
            identity = digest;
            Some(small)
        } else {
            None
        };
        archive.byte_len().checked_add(if small.is_some() { 16 } else { 80 }).ok_or_else(|| invalid("checkpoint extent overflow"))?;
        #[cfg(not(target_arch = "wasm32"))]
        let native = if let Some(context) = archive.native_context() {
            let workspace = context.0.workspace.as_ref().ok_or_else(|| invalid("native checkpoint context is unavailable"))?;
            let owner = workspace.journal()?;
            let bytes = archive.byte_len().checked_add(80).ok_or_else(|| invalid("checkpoint extent overflow"))?;
            let recorded = std::sync::Arc::new(crate::RecordedAssets::new(archive.asset_lengths())?);
            Some(owner.prepare(bytes, crate::storage::native::durability::Wire::Streaming { identity, checkpoint_bytes: bytes, recorded })?)
        } else {
            None
        };
        Ok(Self {
            archive,
            header,
            identity,
            small,
            #[cfg(not(target_arch = "wasm32"))]
            native,
        })
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn native_publication(&self) -> Result<&crate::storage::native::durability::Publication, Error> {
        self.native.as_deref().ok_or_else(|| invalid("native recovery requires an owner-bound streaming checkpoint plan"))
    }
    pub fn identity(&self) -> Blake3 {
        self.identity
    }
    pub fn archive(&self) -> &ArchivePlan {
        &self.archive
    }
    pub fn asset_lengths(&self) -> &BTreeMap<Blake3, u64> {
        self.archive.asset_lengths()
    }
    pub fn byte_len(&self) -> u64 {
        self.archive.byte_len().saturating_add(if self.small.is_some() { 16 } else { 80 })
    }
    pub fn is_empty(&self) -> bool {
        false
    }
    pub fn small_bytes(&self) -> Result<&[u8], Error> {
        self.small
            .as_deref()
            .ok_or_else(|| nodecraft_types::Error::new(nodecraft_types::ErrorKind::Unsupported, "legacy storage cannot publish a streaming checkpoint"))
    }
    pub fn write_to(&self, output: &mut dyn Write, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<(), Error> {
        cancel.check()?;
        if let Some(bytes) = &self.small {
            return output.write_all(bytes).map_err(|_| invalid("cannot write legacy checkpoint"));
        }
        output.write_all(&self.header).map_err(|_| invalid("cannot write checkpoint header"))?;
        self.archive.write_to(output, cancel, metrics)
    }
}

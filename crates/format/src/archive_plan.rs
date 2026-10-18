//! Private, capability-checked output plans. Construction validates content before publication.
use crate::asset_source::{StreamDigest, StreamMetrics, TransferCancel};
use crate::{Snapshot, invalid};
use nodecraft_types::{Blake3, Error, ErrorKind};
use std::{collections::BTreeMap, io::Write, sync::Arc};

/// Actual storage capability, not a request to silently materialize indexed content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageCapability {
    SmallV1,
    StreamingV2,
}

/// Owns exactly the immutable bytes that Save and its recovery checkpoint will publish.
/// Private constructors prevent arbitrary source/hash pairs from becoming output authority.
#[derive(Clone)]
pub struct ArchivePlan(Arc<PreparedArchive>);
struct PreparedArchive {
    content: Content,
    digest: StreamDigest,
    assets: BTreeMap<Blake3, u64>,
}
enum Content {
    Small(Vec<u8>),
    #[cfg(not(target_arch = "wasm32"))]
    Native {
        sealed: crate::native_assets::SealedBytes,
        _leases: Vec<crate::VerifiedAsset>,
        _store: crate::AssetStore,
    },
}
impl ArchivePlan {
    pub fn prepare(snapshot: &Snapshot, capability: StorageCapability, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<Self, Error> {
        cancel.check()?;
        if capability == StorageCapability::SmallV1 {
            // This check deliberately precedes validation, source reads, and allocation.
            if !snapshot.assets.is_resident_small() {
                return Err(unsupported());
            }
            let bytes = crate::encode(snapshot)?;
            let assets = lengths(&snapshot.assets, &crate::assets::project_hashes(&snapshot.project))?;
            let digest = StreamDigest { hash: crate::hash(&bytes), crc32: crc32fast::hash(&bytes), bytes: bytes.len() as u64 };
            return Ok(Self(Arc::new(PreparedArchive { content: Content::Small(bytes), digest, assets })));
        }
        if !snapshot.assets.context().supports_large_assets() {
            return Err(unsupported());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::validate_v1_project(&snapshot.project)?;
            let referenced = snapshot.assets.verify_project_streams(&snapshot.project, cancel, metrics)?;
            snapshot.assets.compact_before_output(cancel, metrics)?;
            let assets = lengths(&snapshot.assets, &referenced)?;
            let mut leases = Vec::new();
            leases.try_reserve_exact(assets.len()).map_err(|_| invalid("cannot allocate bounded archive asset plan"))?;
            for hash in assets.keys() {
                leases.push(snapshot.assets.verified_content(hash, cancel, metrics)?);
            }
            #[derive(serde::Serialize)]
            struct Manifest<'a> {
                version: u32,
                epoch: u64,
                project: &'a nodecraft_graph::Project,
            }
            let json = crate::archive::bounded_json(&Manifest { version: 2, epoch: snapshot.epoch, project: &snapshot.project }, crate::MAX_MANIFEST_BYTES)?;
            crate::compatibility::validate(&json)?;
            let layout = layout(json.len() as u64, assets.values().copied())?;
            let workspace = snapshot.assets.context().0.workspace.as_ref().ok_or_else(unsupported)?;
            let sealed = workspace.generate_archive(layout.bytes, cancel, metrics, |output| {
                use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
                let mut zip = ZipWriter::new(output);
                // The pinned writer otherwise emits a classic footer at the exact u32
                // sentinel. Force the canonical empty ZIP64 extension at that boundary.
                if layout.zip64_footer {
                    zip.set_raw_zip64_extensible_data_sector(Box::new([]));
                }
                let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
                zip.start_file("project.json", options).map_err(|_| invalid("cannot encode streaming manifest entry"))?;
                zip.write_all(&json).map_err(|_| invalid("cannot write streaming manifest"))?;
                for lease in &leases {
                    cancel.check()?;
                    zip.start_file(format!("assets/{}", lease.hash()), options.large_file(lease.byte_len() >= u32::MAX as u64))
                        .map_err(|_| invalid("cannot encode streaming asset entry"))?;
                    let copied = crate::asset_source::copy_and_hash(lease.range(), &mut zip, cancel, metrics)?;
                    if copied.hash != lease.hash() || copied.bytes != lease.byte_len() {
                        return Err(invalid("verified asset changed during archive output"));
                    }
                }
                zip.finish().map_err(|_| invalid("cannot finish streaming archive"))?;
                Ok(())
            })?;
            // Validate the actual finalized grammar and metadata before granting authority.
            let index = crate::zip_index::ArchiveIndex::read(&sealed.range()?, cancel, metrics)?;
            if index.version() != 2 || index.manifest() != json || index.identity().archive_bytes != layout.bytes {
                return Err(invalid("prepared archive disagrees with its admitted manifest"));
            }
            let indexed = index
                .entries()
                .iter()
                .filter_map(|entry| match entry.name {
                    crate::zip_index::EntryName::Asset(hash) => Some((hash, entry.data.byte_len())),
                    crate::zip_index::EntryName::Manifest => None,
                })
                .collect::<BTreeMap<_, _>>();
            if indexed != assets {
                return Err(invalid("prepared archive disagrees with its admitted assets"));
            }
            let digest = sealed.digest();
            Ok(Self(Arc::new(PreparedArchive { content: Content::Native { sealed, _leases: leases, _store: snapshot.assets.clone() }, digest, assets })))
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = metrics;
            Err(unsupported())
        }
    }
    pub(crate) fn from_small_open(bytes: Vec<u8>) -> Result<(Snapshot, Self), Error> {
        if bytes.len() > crate::MAX_ARCHIVE_BYTES {
            return Err(invalid("project exceeds legacy archive limit"));
        }
        let snapshot = crate::decode(&bytes)?;
        let hashes = snapshot.assets.hashes().copied().collect();
        let assets = lengths(&snapshot.assets, &hashes)?;
        let digest = StreamDigest { hash: crate::hash(&bytes), crc32: crc32fast::hash(&bytes), bytes: bytes.len() as u64 };
        Ok((snapshot, Self(Arc::new(PreparedArchive { content: Content::Small(bytes), digest, assets }))))
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn from_native_open(sealed: crate::native_assets::SealedBytes, snapshot: &Snapshot) -> Result<Self, Error> {
        let hashes = snapshot.assets.hashes().copied().collect();
        let assets = lengths(&snapshot.assets, &hashes)?;
        let digest = sealed.digest();
        Ok(Self(Arc::new(PreparedArchive { content: Content::Native { sealed, _leases: Vec::new(), _store: snapshot.assets.clone() }, digest, assets })))
    }
    pub fn capability(&self) -> StorageCapability {
        match &self.0.content {
            Content::Small(_) => StorageCapability::SmallV1,
            #[cfg(not(target_arch = "wasm32"))]
            Content::Native { .. } => StorageCapability::StreamingV2,
        }
    }
    pub fn byte_len(&self) -> u64 {
        self.0.digest.bytes
    }
    pub fn is_empty(&self) -> bool {
        self.0.digest.bytes == 0
    }
    pub fn digest(&self) -> StreamDigest {
        self.0.digest
    }
    pub fn asset_lengths(&self) -> &BTreeMap<Blake3, u64> {
        &self.0.assets
    }
    /// Legacy host defaults must call this before any read/write. It never materializes.
    pub fn small_bytes(&self) -> Result<&[u8], Error> {
        match &self.0.content {
            Content::Small(bytes) => Ok(bytes),
            #[cfg(not(target_arch = "wasm32"))]
            Content::Native { .. } => Err(unsupported()),
        }
    }
    pub fn write_to(&self, output: &mut dyn Write, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<(), Error> {
        cancel.check()?;
        #[cfg(target_arch = "wasm32")]
        let _ = metrics;
        match &self.0.content {
            Content::Small(bytes) => output.write_all(bytes).map_err(|_| invalid("cannot write bounded project archive")),
            #[cfg(not(target_arch = "wasm32"))]
            Content::Native { sealed, .. } => {
                let copied = crate::asset_source::copy_and_hash(&sealed.range()?, output, cancel, metrics)?;
                if copied != self.0.digest {
                    return Err(invalid("prepared archive no longer matches its identity"));
                }
                Ok(())
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn native_context(&self) -> Option<&crate::AssetContext> {
        match &self.0.content {
            Content::Native { _store: store, .. } => Some(store.context()),
            _ => None,
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn check_context(&self, context: &crate::AssetContext) -> Result<(), Error> {
        match &self.0.content {
            Content::Native { _store: store, .. } if !store.context().same(context) => {
                Err(Error::new(ErrorKind::Conflict, "archive plan belongs to another storage context"))
            }
            _ => Ok(()),
        }
    }
}
fn lengths(store: &crate::AssetStore, hashes: &std::collections::BTreeSet<Blake3>) -> Result<BTreeMap<Blake3, u64>, Error> {
    let mut out = BTreeMap::new();
    let mut total = 0u64;
    for hash in hashes {
        let bytes = store.descriptor(hash).ok_or_else(|| invalid("archive references a missing asset"))?.bytes;
        total = total
            .checked_add(bytes)
            .filter(|n| *n <= crate::asset_source::MAX_LARGE_TOTAL_ASSET_BYTES)
            .ok_or_else(|| invalid("archive assets exceed 64 GiB limit"))?;
        if out.len() >= crate::MAX_ASSETS {
            return Err(invalid("archive exceeds asset count limit"));
        }
        out.insert(*hash, bytes);
    }
    Ok(out)
}
#[cfg(not(target_arch = "wasm32"))]
struct Layout {
    bytes: u64,
    zip64_footer: bool,
}
/// Exact stored-ZIP overhead for the pinned writer's canonical ZIP64 size/offset fields.
#[cfg(not(target_arch = "wasm32"))]
fn layout(manifest: u64, assets: impl Iterator<Item = u64>) -> Result<Layout, Error> {
    let mut offset = 0u64;
    let mut directory = 0u64;
    for (name, bytes) in std::iter::once((12u64, manifest)).chain(assets.map(|n| (71u64, n))) {
        let sizes = bytes >= u32::MAX as u64;
        let wide_offset = offset >= u32::MAX as u64;
        let extra = match (sizes, wide_offset) {
            (false, false) => 0,
            (true, false) => 20,
            (false, true) => 12,
            (true, true) => 28,
        };
        directory = add(directory, add(46 + name, extra)?)?;
        offset = add(offset, add(30 + name + if sizes { 20 } else { 0 }, bytes)?)?;
    }
    let zip64_footer = offset >= u32::MAX as u64 || directory >= u32::MAX as u64;
    let bytes = add(add(offset, directory)?, if zip64_footer { 98 } else { 22 })?;
    if bytes > crate::asset_source::MAX_LARGE_ARCHIVE_BYTES {
        return Err(invalid("archive exceeds 65 GiB limit"));
    }
    Ok(Layout { bytes, zip64_footer })
}
#[cfg(not(target_arch = "wasm32"))]
fn add(a: u64, b: u64) -> Result<u64, Error> {
    a.checked_add(b).ok_or_else(|| invalid("archive extent overflow"))
}
fn unsupported() -> Error {
    Error::new(ErrorKind::Unsupported, "this host requires small resident v1 assets; streaming archive capability is unavailable")
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn exact_size_and_offset_sentinels_have_explicit_zip64_overhead() {
        let small = layout(2, std::iter::once(3)).unwrap();
        assert_eq!(small.bytes, (30 + 12 + 2) + (30 + 71 + 3) + (46 + 12) + (46 + 71) + 22);
        assert!(!small.zip64_footer);
        for n in [u32::MAX as u64 - 1, u32::MAX as u64, u32::MAX as u64 + 1] {
            let result = layout(2, std::iter::once(n)).unwrap();
            let sizes = n >= u32::MAX as u64;
            assert_eq!(result.bytes, (30 + 12 + 2) + (30 + 71 + n + if sizes { 20 } else { 0 }) + (46 + 12) + (46 + 71 + if sizes { 20 } else { 0 }) + 98);
            assert!(result.zip64_footer);
        }
        // The following small asset uses only an offset ZIP64 extra field.
        let n = u32::MAX as u64;
        let result = layout(2, [n, 1].into_iter()).unwrap();
        assert_eq!(result.bytes, (30 + 12 + 2) + (30 + 71 + n + 20) + (30 + 71 + 1) + (46 + 12) + (46 + 71 + 20) + (46 + 71 + 12) + 98);
        assert!(layout(2, std::iter::repeat_n(crate::asset_source::MAX_LARGE_ASSET_BYTES, 9)).is_err());
    }
}

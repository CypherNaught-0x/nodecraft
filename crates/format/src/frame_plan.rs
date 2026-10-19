//! Bound replay wire plans and checkpoint-plus-tail identity admission.
use crate::asset_source::{StreamMetrics, TransferCancel};
use crate::{AssetStore, StorageCapability, invalid};
use nodecraft_graph::Changeset;
use nodecraft_types::{Blake3, Error, ErrorKind};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
};
pub const MAX_STREAM_FRAME_BYTES: u64 = 65 * 1024 * 1024 * 1024;
pub const MAX_STREAM_JOURNAL_BYTES: u64 = 132 * 1024 * 1024 * 1024;
pub const MAX_STREAM_TAIL_BYTES: u64 = 65 * 1024 * 1024 * 1024;
/// Independent recovery lifetime budget: GC of the live graph cannot erase bytes that
/// a checkpoint or prior tail frame still records. A successful checkpoint replaces it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordedAssets {
    lengths: BTreeMap<Blake3, u64>,
    bytes: u64,
}
impl RecordedAssets {
    pub fn new(lengths: &BTreeMap<Blake3, u64>) -> Result<Self, Error> {
        Self::default().admit(lengths)
    }
    pub fn lengths(&self) -> &BTreeMap<Blake3, u64> {
        &self.lengths
    }
    pub fn byte_len(&self) -> u64 {
        self.bytes
    }
    pub fn len(&self) -> usize {
        self.lengths.len()
    }
    pub fn is_empty(&self) -> bool {
        self.lengths.is_empty()
    }
    pub fn admit(&self, additional: &BTreeMap<Blake3, u64>) -> Result<Self, Error> {
        let mut next = self.clone();
        for (hash, bytes) in additional {
            if let Some(old) = next.lengths.get(hash) {
                if old != bytes {
                    return Err(invalid("recorded asset identity has inconsistent lengths"));
                }
                continue;
            }
            if *bytes > crate::asset_source::MAX_LARGE_ASSET_BYTES || next.lengths.len() >= crate::MAX_ASSETS {
                return Err(invalid("recovery recorded asset limit reached; Save before editing"));
            }
            next.bytes = next
                .bytes
                .checked_add(*bytes)
                .filter(|n| *n <= crate::asset_source::MAX_LARGE_TOTAL_ASSET_BYTES)
                .ok_or_else(|| invalid("recovery recorded assets exceed 64 GiB; Save before editing"))?;
            next.lengths.insert(*hash, *bytes);
        }
        Ok(next)
    }
}
pub struct FramePlan {
    content: Content,
    bytes: u64,
}
#[cfg(not(target_arch = "wasm32"))]
struct JournalFrame {
    checkpoint: Blake3,
    sequence: u64,
    previous: RecordedAssets,
    next: std::sync::Arc<RecordedAssets>,
}
enum Content {
    Small(Vec<u8>),
    Streaming {
        json: Vec<u8>,
        assets: Vec<crate::VerifiedAsset>,
        hash: Blake3,
        #[cfg(not(target_arch = "wasm32"))]
        context: crate::AssetContext,
        #[cfg(not(target_arch = "wasm32"))]
        journal: JournalFrame,
    },
}
impl FramePlan {
    pub fn byte_len(&self) -> u64 {
        self.bytes
    }
    pub fn is_empty(&self) -> bool {
        false
    }
    pub fn small_bytes(&self) -> Result<&[u8], Error> {
        match &self.content {
            Content::Small(bytes) => Ok(bytes),
            _ => Err(unsupported()),
        }
    }
    pub fn write_to(&self, output: &mut dyn Write, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<(), Error> {
        cancel.check()?;
        match &self.content {
            Content::Small(bytes) => output.write_all(bytes).map_err(|_| invalid("cannot append bounded legacy frame")),
            Content::Streaming { json, assets, hash, .. } => {
                let length = self.bytes.checked_sub(40).ok_or_else(|| invalid("invalid frame extent"))?;
                output.write_all(&length.to_le_bytes()).and_then(|()| output.write_all(hash.as_bytes())).map_err(|_| invalid("cannot append frame header"))?;
                let mut writer = HashWriter { output, hash: blake3::Hasher::new(), bytes: 0 };
                payload(&mut writer, json, assets, cancel, metrics)?;
                if writer.bytes != length || writer.hash.finalize().as_bytes() != hash.as_bytes() {
                    return Err(invalid("frame bytes changed after admission"));
                }
                Ok(())
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn admit_journal(&self, checkpoint: Blake3, sequence: u64, recorded: &RecordedAssets) -> Result<std::sync::Arc<RecordedAssets>, Error> {
        match &self.content {
            Content::Streaming { journal, .. } if journal.checkpoint == checkpoint && journal.sequence == sequence && &journal.previous == recorded => {
                Ok(journal.next.clone())
            }
            _ => Err(invalid("frame does not match the active checkpoint, sequence and recorded assets")),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn check_context(&self, context: &crate::AssetContext) -> Result<(), Error> {
        if let Content::Streaming { context: owner, .. } = &self.content
            && !owner.same(context)
        {
            return Err(Error::new(ErrorKind::Conflict, "frame plan belongs to another storage context"));
        }
        Ok(())
    }
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare(
    identity: Blake3,
    sequence: u64,
    changes: &Changeset,
    assets: &AssetStore,
    referenced: &BTreeSet<Blake3>,
    recorded: &RecordedAssets,
    capability: StorageCapability,
    cancel: &TransferCancel,
    metrics: &StreamMetrics,
) -> Result<(FramePlan, RecordedAssets), Error> {
    cancel.check()?;
    let mut lengths = BTreeMap::new();
    let mut additional = BTreeSet::new();
    for hash in referenced {
        let bytes = assets.descriptor(hash).ok_or_else(|| invalid("replay references a missing retained asset"))?.bytes;
        if let Some(old) = recorded.lengths.get(hash) {
            if *old != bytes {
                return Err(invalid("recorded asset identity has inconsistent lengths"));
            }
        } else {
            lengths.insert(*hash, bytes);
            additional.insert(*hash);
        }
    }
    let next = recorded.admit(&lengths)?; // Complete count/byte admission before content I/O.
    if capability == StorageCapability::SmallV1 {
        if !assets.is_resident_small() {
            return Err(unsupported());
        }
        let bytes = crate::journal::frame_admitted(identity, sequence, changes, assets, &additional)?;
        let length = bytes.len() as u64;
        return Ok((FramePlan { content: Content::Small(bytes), bytes: length }, next));
    }
    if !assets.context().supports_large_assets() {
        return Err(unsupported());
    }
    #[derive(serde::Serialize)]
    struct Record<'a> {
        checkpoint: Blake3,
        sequence: u64,
        changes: &'a Changeset,
    }
    let json = crate::archive::bounded_json(&Record { checkpoint: identity, sequence, changes }, 16 * 1024 * 1024)?;
    let bytes = lengths.values().try_fold(56u64.checked_add(json.len() as u64).ok_or_else(|| invalid("frame extent overflow"))?, |n, bytes| {
        n.checked_add(40).and_then(|n| n.checked_add(*bytes)).filter(|n| *n <= MAX_STREAM_FRAME_BYTES).ok_or_else(|| invalid("frame exceeds 65 GiB limit"))
    })?;
    let mut leases = Vec::new();
    leases.try_reserve_exact(lengths.len()).map_err(|_| invalid("cannot allocate bounded frame asset plan"))?;
    for hash in lengths.keys() {
        assets.verified_content(hash, cancel, metrics)?;
    }
    #[cfg(not(target_arch = "wasm32"))]
    assets.compact_before_output(cancel, metrics)?;
    for hash in lengths.keys() {
        leases.push(assets.verified_content(hash, cancel, metrics)?);
    }
    let mut sink = std::io::sink();
    let mut writer = HashWriter { output: &mut sink, hash: blake3::Hasher::new(), bytes: 0 };
    payload(&mut writer, &json, &leases, cancel, metrics)?;
    if writer.bytes.checked_add(40) != Some(bytes) {
        return Err(invalid("prepared frame extent disagrees with admission"));
    }
    let hash = Blake3::from_bytes(*writer.hash.finalize().as_bytes());
    Ok((
        FramePlan {
            content: Content::Streaming {
                json,
                assets: leases,
                hash,
                #[cfg(not(target_arch = "wasm32"))]
                context: assets.context().clone(),
                #[cfg(not(target_arch = "wasm32"))]
                journal: JournalFrame { checkpoint: identity, sequence, previous: recorded.clone(), next: std::sync::Arc::new(next.clone()) },
            },
            bytes,
        },
        next,
    ))
}
fn payload(output: &mut dyn Write, json: &[u8], assets: &[crate::VerifiedAsset], cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<(), Error> {
    output
        .write_all(&(json.len() as u64).to_le_bytes())
        .and_then(|()| output.write_all(json))
        .and_then(|()| output.write_all(&(assets.len() as u64).to_le_bytes()))
        .map_err(|_| invalid("cannot write bounded frame metadata"))?;
    for asset in assets {
        cancel.check()?;
        output
            .write_all(asset.hash().as_bytes())
            .and_then(|()| output.write_all(&asset.byte_len().to_le_bytes()))
            .map_err(|_| invalid("cannot write frame asset header"))?;
        let actual = crate::asset_source::copy_and_hash(asset.range(), output, cancel, metrics)?;
        if actual.hash != asset.hash() || actual.bytes != asset.byte_len() {
            return Err(invalid("frame asset changed after verification"));
        }
    }
    Ok(())
}
struct HashWriter<'a> {
    output: &'a mut dyn Write,
    hash: blake3::Hasher,
    bytes: u64,
}
impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.output.write(bytes)?;
        self.bytes = self.bytes.checked_add(n as u64).ok_or_else(|| std::io::Error::other("frame extent overflow"))?;
        self.hash.update(bytes.get(..n).ok_or_else(|| std::io::Error::other("frame write length mismatch"))?);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}
fn unsupported() -> Error {
    Error::new(ErrorKind::Unsupported, "legacy storage requires bounded resident replay; streaming frame capability is unavailable")
}

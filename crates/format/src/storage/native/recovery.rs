//! NCRJ2 recovery: immutable checkpoint authority and one aggregate frame-body stage.
//! Outer physical damage wins over inner semantic damage; cancellation/I/O remain errors.
use super::*;
use crate::{
    RecordedAssets,
    asset_source::{MAX_LARGE_ASSET_BYTES, TRANSFER_BYTES, TransferBuffer},
    disk_quota::DiskPool,
    journal::{Recovered, RecoveryDamage, RecoveryDamageReason},
};
use nodecraft_graph::Changeset;
use nodecraft_types::Blake3;
use std::{
    collections::BTreeMap,
    io::{Seek, SeekFrom},
};

pub(super) struct NativeRecovered {
    pub recovered: Recovered,
    pub wire: durability::Wire,
}
#[derive(serde::Deserialize)]
struct Record {
    checkpoint: Blake3,
    sequence: u64,
    changes: Changeset,
}
enum FrameFailure {
    Physical,
    Semantic(Error),
    Operational(Error),
}
fn semantic(message: &'static str) -> FrameFailure {
    FrameFailure::Semantic(invalid(message))
}
struct Payload<'a> {
    file: &'a mut File,
    left: u64,
    hash: blake3::Hasher,
    cancel: &'a TransferCancel,
    metrics: &'a StreamMetrics,
}
impl Payload<'_> {
    fn read_exact(&mut self, mut bytes: &mut [u8]) -> Result<(), FrameFailure> {
        if bytes.len() as u64 > self.left {
            return Err(semantic("truncated recovery payload envelope"));
        }
        while !bytes.is_empty() {
            self.cancel.check().map_err(FrameFailure::Operational)?;
            let n = bytes.len().min(TRANSFER_BYTES);
            let chunk = bytes.get_mut(..n).ok_or_else(|| semantic("invalid payload buffer"))?;
            let n = self.file.read(chunk).map_err(|_| FrameFailure::Operational(io_error("cannot read recovery payload")))?;
            if n == 0 {
                return Err(FrameFailure::Physical);
            }
            let (read, rest) = bytes.split_at_mut_checked(n).ok_or_else(|| semantic("invalid recovery read extent"))?;
            self.hash.update(read);
            self.metrics.read(n);
            self.left = self.left.checked_sub(n as u64).ok_or_else(|| semantic("recovery payload extent overflow"))?;
            bytes = rest;
        }
        Ok(())
    }
    fn number(&mut self) -> Result<u64, FrameFailure> {
        let mut bytes = [0; 8];
        self.read_exact(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }
    fn drain(&mut self, buffer: &mut TransferBuffer) -> Result<(), FrameFailure> {
        while self.left > 0 {
            let n = usize::try_from(self.left.min(TRANSFER_BYTES as u64)).map_err(|_| semantic("recovery drain extent overflow"))?;
            self.read_exact(buffer.bytes_mut().get_mut(..n).ok_or_else(|| semantic("invalid recovery drain buffer"))?)?;
        }
        Ok(())
    }
}
struct Captured {
    hash: Blake3,
    start: u64,
    bytes: u64,
    crc: u32,
    _identity: Arc<crate::asset_context::AssetIdentity>,
}
struct PendingFrame {
    record: Record,
    capture: Option<crate::native_assets::AssetCapture>,
    captured: Vec<Captured>,
    recorded: RecordedAssets,
}
#[allow(clippy::too_many_arguments)]
fn parse_payload(
    payload: &mut Payload<'_>,
    snapshot: &Snapshot,
    recorded: &RecordedAssets,
    identity: Blake3,
    sequence: u64,
    workspace: &crate::NativeAssetWorkspace,
    stage: &crate::disk_quota::StageOperation<'_>,
    buffer: &mut TransferBuffer,
    cancel: &TransferCancel,
    metrics: &StreamMetrics,
) -> Result<PendingFrame, FrameFailure> {
    let json_len = payload.number()?;
    if json_len > 16 * 1024 * 1024 || json_len > payload.left {
        return Err(semantic("recovery changeset exceeds its bounded payload"));
    }
    let n = usize::try_from(json_len).map_err(|_| semantic("recovery changeset length cannot be represented"))?;
    let mut json = Vec::new();
    json.try_reserve_exact(n).map_err(|_| FrameFailure::Operational(io_error("cannot allocate bounded recovery changeset")))?;
    json.resize(n, 0);
    payload.read_exact(&mut json)?;
    crate::compatibility::validate(&json).map_err(FrameFailure::Semantic)?;
    let record: Record = serde_json::from_slice(&json).map_err(|_| semantic("invalid recovery changeset"))?;
    if record.checkpoint != identity || record.sequence != sequence {
        return Err(semantic("stale or out-of-order recovery changeset"));
    }
    let count = payload.number()?;
    if count > crate::MAX_ASSETS as u64 {
        return Err(semantic("recovery asset count exceeds 1024"));
    }
    let mut captured = Vec::new();
    captured.try_reserve_exact(count as usize).map_err(|_| FrameFailure::Operational(io_error("cannot allocate bounded recovery asset index")))?;
    let mut next = recorded.clone();
    let mut capture = None;
    for _ in 0..count {
        let mut digest = [0; 32];
        payload.read_exact(&mut digest)?;
        let hash = Blake3::from_bytes(digest);
        let bytes = payload.number()?;
        if bytes > MAX_LARGE_ASSET_BYTES || bytes > payload.left || next.lengths().contains_key(&hash) || snapshot.assets.contains(&hash) {
            return Err(semantic("duplicate, oversized or truncated recovery asset"));
        }
        next = next.admit(&BTreeMap::from([(hash, bytes)])).map_err(FrameFailure::Semantic)?;
        // Logical admission includes old reader/history pins and precedes body I/O.
        let pin = snapshot.assets.context().claim(hash, bytes).map_err(FrameFailure::Operational)?;
        if capture.is_none() {
            snapshot.assets.compact_in_stage(stage, cancel, metrics).map_err(FrameFailure::Operational)?;
            capture = Some(workspace.capture(stage).map_err(FrameFailure::Operational)?);
        }
        let output = capture.as_mut().ok_or_else(|| semantic("recovery body capture is unavailable"))?;
        let start = output.reserve_asset(bytes).map_err(FrameFailure::Operational)?;
        let mut left = bytes;
        let mut actual = blake3::Hasher::new();
        let mut crc = crc32fast::Hasher::new();
        while left > 0 {
            let n = usize::try_from(left.min(TRANSFER_BYTES as u64)).map_err(|_| semantic("recovery asset extent overflow"))?;
            let chunk = buffer.bytes_mut().get_mut(..n).ok_or_else(|| semantic("invalid recovery transfer buffer"))?;
            payload.read_exact(chunk)?;
            output.write_bytes(chunk).map_err(FrameFailure::Operational)?;
            actual.update(chunk);
            crc.update(chunk);
            left = left.checked_sub(n as u64).ok_or_else(|| semantic("recovery asset extent overflow"))?;
        }
        if actual.finalize().as_bytes() != hash.as_bytes() {
            return Err(semantic("recovery asset content hash mismatch"));
        }
        captured.push(Captured { hash, start, bytes, crc: crc.finalize(), _identity: pin });
    }
    if payload.left != 0 {
        return Err(semantic("unexpected recovery payload data"));
    }
    Ok(PendingFrame { record, capture, captured, recorded: next })
}
fn read_exact(file: &mut File, mut bytes: &mut [u8], cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<bool, Error> {
    while !bytes.is_empty() {
        cancel.check()?;
        let n = file.read(bytes).map_err(|_| io_error("cannot read recovery envelope"))?;
        if n == 0 {
            return Ok(false);
        }
        metrics.read(n);
        bytes = bytes.get_mut(n..).ok_or_else(|| invalid("invalid recovery read extent"))?;
    }
    Ok(true)
}
fn damage(offset: u64, reason: RecoveryDamageReason) -> Option<RecoveryDamage> {
    Some(RecoveryDamage { offset, reason })
}
#[allow(clippy::too_many_arguments)]
pub(super) fn recover(
    file: &mut File,
    length: u64,
    context: AssetContext,
    workspace: &crate::NativeAssetWorkspace,
    cancel: &TransferCancel,
    metrics: &StreamMetrics,
) -> Result<NativeRecovered, Error> {
    cancel.check()?;
    if length > crate::frame_plan::MAX_STREAM_JOURNAL_BYTES {
        return Err(invalid("native recovery exceeds size limit"));
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| io_error("cannot seek recovery header"))?;
    let mut magic = [0; 8];
    if !read_exact(file, &mut magic, cancel, metrics)? {
        return Err(invalid("truncated recovery checkpoint"));
    }
    if &magic == b"NCRJ0001" {
        if length > MAX_JOURNAL_BYTES as u64 {
            return Err(invalid("legacy recovery exceeds size limit"));
        }
        file.seek(SeekFrom::Start(0)).map_err(|_| io_error("cannot seek legacy recovery"))?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length as usize).map_err(|_| io_error("cannot allocate bounded legacy recovery"))?;
        file.take(MAX_JOURNAL_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| io_error("cannot read legacy recovery"))?;
        return Ok(NativeRecovered { recovered: crate::journal::recover(&bytes)?, wire: durability::Wire::Legacy });
    }
    if &magic != b"NCRJ0002" {
        return Err(invalid("unsupported recovery checkpoint version"));
    }
    let mut header = [0; 80];
    header.get_mut(..8).ok_or_else(|| invalid("invalid checkpoint header"))?.copy_from_slice(&magic);
    if !read_exact(file, header.get_mut(8..).ok_or_else(|| invalid("invalid checkpoint header"))?, cancel, metrics)? {
        return Err(invalid("truncated recovery checkpoint header"));
    }
    let (archive_len, digest, identity) = crate::checkpoint_plan::read_header(&header)?;
    let checkpoint_bytes = archive_len.checked_add(80).filter(|n| *n <= length).ok_or_else(|| invalid("truncated recovery checkpoint archive"))?;
    if length.checked_sub(checkpoint_bytes).is_none_or(|n| n > crate::frame_plan::MAX_STREAM_TAIL_BYTES) {
        return Err(invalid("recovery tail exceeds 65 GiB"));
    }
    let stage = workspace.quota().stage_operation()?;
    // This clone shares its seek cursor with file. Only the copy/index phase uses it;
    // file is explicitly repositioned afterwards, never read concurrently.
    let input = crate::NativeAssetWorkspace::input_range(file.try_clone().map_err(|_| io_error("cannot retain recovery input"))?, length)?;
    let archive = input.slice(80, archive_len)?;
    let admitted = crate::zip_index::ArchiveIndex::read(&archive, cancel, metrics)?;
    let sealed = workspace.seal_copy_in(&stage, &archive, DiskPool::Bases, None, cancel, metrics)?;
    if sealed.digest().hash != digest || sealed.digest().bytes != archive_len {
        return Err(invalid("recovery checkpoint content digest mismatch"));
    }
    let retained = crate::zip_index::ArchiveIndex::read(&sealed.range()?, cancel, metrics)?;
    if admitted.identity() != retained.identity() {
        return Err(invalid("recovery checkpoint metadata changed during private transfer"));
    }
    let mut snapshot = crate::archive::decode_indexed_presence(&retained, context)?;
    if retained.version() == 1 {
        snapshot.assets.verify_all_content_in_stage(&stage, cancel, metrics)?;
        snapshot.assets.verify_project_in_stage(&stage, &snapshot.project, cancel, metrics)?;
    }
    let lengths = snapshot
        .assets
        .hashes()
        .map(|hash| snapshot.assets.descriptor(hash).map(|d| (*hash, d.bytes)).ok_or_else(|| invalid("checkpoint asset is missing")))
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let mut recorded = RecordedAssets::new(&lengths)?;
    let mut applied = 0u64;
    let mut stopped = None;
    let mut offset = checkpoint_bytes;
    let mut buffer = TransferBuffer::new(metrics)?;
    while offset < length {
        cancel.check()?;
        file.seek(SeekFrom::Start(offset)).map_err(|_| io_error("cannot seek recovery frame"))?;
        let mut outer = [0; 40];
        if length.saturating_sub(offset) < 40 || !read_exact(file, &mut outer, cancel, metrics)? {
            stopped = damage(offset, RecoveryDamageReason::IncompleteHeader);
            break;
        }
        if outer.iter().all(|b| *b == 0) {
            stopped = damage(offset, RecoveryDamageReason::ZeroFilledHeader);
            break;
        }
        let payload_len =
            u64::from_le_bytes(outer.get(..8).ok_or_else(|| invalid("invalid frame header"))?.try_into().map_err(|_| invalid("invalid frame extent"))?);
        if !(16..=crate::frame_plan::MAX_STREAM_FRAME_BYTES - 40).contains(&payload_len) {
            stopped = damage(offset, RecoveryDamageReason::InvalidFrameLength);
            break;
        }
        let end = offset.checked_add(40).and_then(|n| n.checked_add(payload_len));
        if end.is_none_or(|n| n > length) {
            stopped = damage(offset, RecoveryDamageReason::IncompletePayload);
            break;
        }
        let sequence = applied.checked_add(1).ok_or_else(|| invalid("recovery sequence overflow"))?;
        let mut payload = Payload { file, left: payload_len, hash: blake3::Hasher::new(), cancel, metrics };
        let parsed = parse_payload(&mut payload, &snapshot, &recorded, identity, sequence, workspace, &stage, &mut buffer, cancel, metrics);
        let (pending, semantic_error) = match parsed {
            Ok(pending) => (Some(pending), None),
            Err(FrameFailure::Semantic(error)) => (None, Some(error)),
            Err(FrameFailure::Physical) => {
                stopped = damage(offset, RecoveryDamageReason::IncompletePayload);
                break;
            }
            Err(FrameFailure::Operational(error)) => return Err(Error::new(ErrorKind::Conflict, error.message)),
        };
        match payload.drain(&mut buffer) {
            Ok(()) => {}
            Err(FrameFailure::Physical) => {
                stopped = damage(offset, RecoveryDamageReason::IncompletePayload);
                break;
            }
            Err(FrameFailure::Operational(error)) => return Err(Error::new(ErrorKind::Conflict, error.message)),
            Err(FrameFailure::Semantic(error)) => return Err(error),
        }
        if Some(payload.hash.finalize().as_bytes().as_slice()) != outer.get(8..40) {
            stopped = damage(offset, RecoveryDamageReason::ChecksumMismatch);
            break;
        }
        if let Some(error) = semantic_error {
            return Err(error);
        }
        let pending = pending.ok_or_else(|| invalid("missing validated recovery frame"))?;
        let mut ranges = Vec::new();
        ranges.try_reserve_exact(pending.captured.len()).map_err(|_| io_error("cannot allocate bounded recovery ranges"))?;
        if let Some(capture) = pending.capture {
            let sealed = capture.seal(cancel)?;
            let range = sealed.range()?;
            for entry in &pending.captured {
                ranges.push((entry.hash, range.slice(entry.start, entry.bytes)?, entry.crc));
            }
        }
        let assets = snapshot.assets.recovery_ranges(&ranges, cancel, metrics)?;
        crate::journal::validate_replay(&pending.record.changes, &assets)?;
        let mut project = snapshot.project.clone();
        pending.record.changes.apply(&mut project)?;
        assets.verify_changed_in_stage(&stage, &project, &pending.record.changes, cancel, metrics)?;
        cancel.check()?;
        snapshot = Snapshot { project, assets, epoch: snapshot.epoch };
        recorded = pending.recorded;
        applied = sequence;
        offset = end.ok_or_else(|| invalid("recovery frame extent overflow"))?;
    }
    if file.metadata().map_err(|_| io_error("cannot reinspect recovery source"))?.len() != length {
        return Err(io_error("recovery source extent changed during replay"));
    }
    workspace.cleanup()?;
    Ok(NativeRecovered {
        recovered: Recovered { snapshot, applied, truncated_tail: stopped.is_some(), damage: stopped },
        wire: durability::Wire::Streaming { identity, checkpoint_bytes, recorded: Arc::new(recorded) },
    })
}

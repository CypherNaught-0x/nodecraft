//! Recovery files contain a complete checkpoint followed by checksummed, sequential changesets.
//! Physical frame damage stops at the validated prefix; later frames are never replayed.
//! Checksum-valid semantic/compatibility violations remain hard errors.
use crate::{AssetStore, MAX_ARCHIVE_BYTES, MAX_ASSET_BYTES, MAX_ASSETS, Snapshot, encode, hash, invalid};
use nodecraft_graph::Changeset;
use nodecraft_types::{Blake3, Error};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_JOURNAL_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_CHANGES_BYTES: usize = 16 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"NCRJ0002";
const LEGACY_MAGIC: &[u8; 8] = b"NCRJ0001";

pub struct Checkpoint {
    pub bytes: Vec<u8>,
    pub identity: Blake3,
    pub assets: BTreeSet<Blake3>,
}

pub fn checkpoint(snapshot: &Snapshot) -> Result<Checkpoint, Error> {
    let archive = encode(snapshot)?;
    from_validated_archive(archive, crate::assets::project_hashes(&snapshot.project))
}

/// Decode once and reuse the validated archive bytes as the recovery checkpoint.
pub fn checkpoint_from_archive(archive: Vec<u8>) -> Result<(Snapshot, Checkpoint), Error> {
    let (snapshot, version) = crate::archive::decode_versioned(&archive)?;
    // Never append v2 operations beneath a legacy header/identity. The caller atomically
    // checkpoints this converted snapshot before adopting it or publishing any new edit.
    if version == 1 {
        let checkpoint = checkpoint(&snapshot)?;
        return Ok((snapshot, checkpoint));
    }
    let assets = snapshot.assets.iter().map(|(hash, _)| *hash).collect();
    Ok((snapshot, from_validated_archive(archive, assets)?))
}
fn from_validated_archive(mut archive: Vec<u8>, assets: BTreeSet<Blake3>) -> Result<Checkpoint, Error> {
    let identity = hash(&archive);
    let len = archive.len();
    archive.try_reserve(16).map_err(|_| invalid("cannot allocate recovery checkpoint"))?;
    archive.resize(len.saturating_add(16), 0);
    archive.copy_within(..len, 16);
    archive.get_mut(..8).ok_or_else(|| invalid("invalid recovery checkpoint"))?.copy_from_slice(MAGIC);
    archive.get_mut(8..16).ok_or_else(|| invalid("invalid recovery checkpoint"))?.copy_from_slice(&(len as u64).to_le_bytes());
    Ok(Checkpoint { bytes: archive, identity, assets })
}
impl Checkpoint {
    pub fn archive(&self) -> Result<&[u8], Error> {
        self.bytes.get(16..).ok_or_else(|| invalid("invalid recovery checkpoint"))
    }
}

#[derive(Serialize, Deserialize)]
struct Record {
    checkpoint: Blake3,
    sequence: u64,
    changes: Changeset,
}

#[derive(Serialize)]
struct RecordRef<'a> {
    checkpoint: Blake3,
    sequence: u64,
    changes: &'a Changeset,
}

/// Reserve a canonical replay record and every referenced asset as if a future checkpoint
/// omitted them all. Sequence digits, the full record envelope and binary frame/asset headers
/// count too, so an accepted undo cannot become too large after saving a smaller document.
pub fn validate_replay(changes: &Changeset, assets: &AssetStore) -> Result<(), Error> {
    validate_replay_with_limits(changes, assets, MAX_CHANGES_BYTES, MAX_FRAME_BYTES)
}
fn validate_replay_with_limits(changes: &Changeset, assets: &AssetStore, record_cap: usize, frame_cap: usize) -> Result<(), Error> {
    replay_references(changes, assets, record_cap, frame_cap).map(|_| ())
}
fn replay_references(changes: &Changeset, assets: &AssetStore, record_cap: usize, frame_cap: usize) -> Result<BTreeSet<Blake3>, Error> {
    #[cfg(any(test, feature = "validation-metrics"))]
    crate::validation_metrics::record(|metrics| metrics.replay_preflights = metrics.replay_preflights.saturating_add(1));
    changes.validate_payload()?;
    let json = crate::archive::bounded_v2_json(&RecordRef { checkpoint: Blake3::from_bytes([0; 32]), sequence: u64::MAX, changes }, record_cap)?;
    let referenced = crate::assets_in_changes(changes)?;
    let mut binary = 56_usize; // outer length/hash40 + payload JSON length/asset count16
    for hash in &referenced {
        let bytes = assets.get(hash).ok_or_else(|| invalid("journal replay references a missing retained asset"))?;
        binary = binary.saturating_add(40).saturating_add(bytes.len());
    }
    let record_limit = frame_cap.checked_sub(binary).ok_or_else(|| invalid("journal replay assets exceed frame budget"))?.min(record_cap);
    let empty = Changeset::default();
    let envelope = crate::archive::bounded_json(&RecordRef { checkpoint: Blake3::from_bytes([0; 32]), sequence: u64::MAX, changes: &empty }, 1024)?
        .len()
        .saturating_sub(2);
    let canonical_limit = record_limit.checked_sub(envelope).ok_or_else(|| invalid("journal replay exceeds record/frame budget"))?;
    changes.validate_replay_budget(canonical_limit)?;
    if json.len() > record_limit {
        return Err(invalid("journal replay exceeds record/frame budget"));
    }
    Ok(referenced)
}

/// Frozen wire/payload admission for one immutable forward replay. This does not
/// prove graph application: owning sessions must pair it privately with their draft.
/// The proof owns its changes and only their referenced immutable bytes, so callers
/// cannot substitute another changeset or store after admission. Not Clone.
pub struct AdmittedReplay {
    changes: Changeset,
    assets: AssetStore,
    referenced: BTreeSet<Blake3>,
}
impl AdmittedReplay {
    pub fn new(changes: Changeset, assets: &AssetStore) -> Result<Self, Error> {
        let referenced = replay_references(&changes, assets, MAX_CHANGES_BYTES, MAX_FRAME_BYTES)?;
        let assets = assets.referenced_snapshot(&referenced)?;
        Ok(Self { changes, assets, referenced })
    }
    /// Partial post-stage asset check against this immutable admitted payload.
    /// Owning sessions must establish the valid root and graph application first.
    pub fn validate_changed_assets(&self, document: &nodecraft_graph::Project, source_assets: &AssetStore) -> Result<(), Error> {
        // Surviving touched nodes may retain references absent from the forward
        // payload (for example SetParam changes only one of several parameters).
        // This method therefore needs the complete admitted source store, not only
        // the replay's retained references.
        source_assets.validate_changed_assets(document, &self.changes)
    }
    /// Consume this admission, adding only forward references not already journaled.
    pub fn into_frame(self, identity: Blake3, sequence: u64, recorded: &BTreeSet<Blake3>) -> Result<(Vec<u8>, BTreeSet<Blake3>), Error> {
        let additional = self.referenced.difference(recorded).copied().collect();
        let bytes = frame_admitted(identity, sequence, &self.changes, &self.assets, &additional)?;
        Ok((bytes, additional))
    }
}

pub fn frame(identity: Blake3, sequence: u64, changes: &Changeset, assets: &AssetStore, additional: &BTreeSet<Blake3>) -> Result<Vec<u8>, Error> {
    validate_replay(changes, assets)?;
    frame_admitted(identity, sequence, changes, assets, additional)
}
fn frame_admitted(identity: Blake3, sequence: u64, changes: &Changeset, assets: &AssetStore, additional: &BTreeSet<Blake3>) -> Result<Vec<u8>, Error> {
    if additional.len() > MAX_ASSETS {
        return Err(invalid("recovery asset count exceeds limit"));
    }
    let record = RecordRef { checkpoint: identity, sequence, changes };
    // Both public frame() and the private admitted capability already checked these immutable
    // v2 roles with the longest sequence; identity/sequence cannot substitute graph payloads.
    let json = crate::archive::bounded_json(&record, MAX_CHANGES_BYTES)?;
    let mut payload = Vec::new();
    payload.extend_from_slice(&(json.len() as u64).to_le_bytes());
    payload.extend_from_slice(&json);
    payload.extend_from_slice(&(additional.len() as u64).to_le_bytes());
    for digest in additional {
        let data = assets.get(digest).ok_or_else(|| invalid("journal references a missing asset"))?;
        if payload.len().saturating_add(40).saturating_add(data.len()).saturating_add(40) > MAX_FRAME_BYTES {
            return Err(invalid("recovery frame exceeds size limit; save a checkpoint"));
        }
        payload.extend_from_slice(digest.as_bytes());
        payload.extend_from_slice(&(data.len() as u64).to_le_bytes());
        payload.extend_from_slice(data);
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(hash(&payload).as_bytes());
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

pub struct Recovered {
    pub snapshot: Snapshot,
    pub applied: u64,
    /// Physical frame damage stopped replay. Retained for existing hosts; `damage` gives detail.
    /// The caller must preserve the original bytes and checkpoint before continuing.
    pub truncated_tail: bool,
    /// Checkpoint migrated recovery before any v2 append.
    pub legacy: bool,
    pub damage: Option<RecoveryDamage>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryDamage {
    /// Absolute byte offset of the first damaged frame, after the fully validated prefix.
    pub offset: usize,
    pub reason: RecoveryDamageReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryDamageReason {
    IncompleteHeader,
    ZeroFilledHeader,
    InvalidFrameLength,
    IncompletePayload,
    ChecksumMismatch,
}

impl RecoveryDamageReason {
    pub fn description(self) -> &'static str {
        match self {
            Self::IncompleteHeader => "incomplete recovery frame header",
            Self::ZeroFilledHeader => "zero-filled recovery frame header",
            Self::InvalidFrameLength => "invalid recovery frame length",
            Self::IncompletePayload => "incomplete recovery frame payload",
            Self::ChecksumMismatch => "recovery frame checksum mismatch",
        }
    }
}

pub fn recover(bytes: &[u8]) -> Result<Recovered, Error> {
    let legacy = bytes.get(..8) == Some(LEGACY_MAGIC.as_slice());
    if bytes.len() > MAX_JOURNAL_BYTES || (!legacy && bytes.get(..8) != Some(MAGIC.as_slice())) {
        return Err(invalid("invalid recovery file or size limit exceeded"));
    }
    let mut input = bytes.get(8..).ok_or_else(|| invalid("truncated recovery file"))?;
    let len = length(&mut input, MAX_ARCHIVE_BYTES)?;
    let archive = take(&mut input, len)?;
    let identity = hash(archive);
    // Frame identities bind the original archive bytes, never a migrated re-encoding.
    let (mut snapshot, version) = crate::archive::decode_versioned(archive)?;
    if (version == 1) != legacy {
        return Err(invalid("recovery header and archive version disagree"));
    }
    let mut applied = 0_u64;
    let mut damage = None;
    while !input.is_empty() {
        let offset = bytes.len().saturating_sub(input.len());
        let stopped = |reason| Some(RecoveryDamage { offset, reason });
        if input.len() < 40 {
            damage = stopped(RecoveryDamageReason::IncompleteHeader);
            break;
        }
        // Only outer, unverified framing is recoverable physical damage. After a
        // checksum validates, malformed record/asset envelopes are semantic errors.
        if input.get(..40).is_some_and(|header| header.iter().all(|byte| *byte == 0)) {
            damage = stopped(RecoveryDamageReason::ZeroFilledHeader);
            break;
        }
        let len = match length(&mut input, MAX_FRAME_BYTES.saturating_sub(40)) {
            Ok(len) if len >= 16 => len,
            _ => {
                damage = stopped(RecoveryDamageReason::InvalidFrameLength);
                break;
            }
        };
        let digest = take(&mut input, 32)?;
        if input.len() < len {
            damage = stopped(RecoveryDamageReason::IncompletePayload);
            break;
        }
        let payload = take(&mut input, len)?;
        if hash(payload).as_bytes().as_slice() != digest {
            damage = stopped(RecoveryDamageReason::ChecksumMismatch);
            break;
        }
        let mut frame = payload;
        let json_len = length(&mut frame, MAX_CHANGES_BYTES)?;
        let json = take(&mut frame, json_len)?;
        if legacy {
            crate::legacy::validate(json)?;
        } else {
            crate::v2::validate(json)?;
        }
        let record: Record = serde_json::from_slice(json).map_err(|_| invalid("invalid recovery changeset"))?;
        let sequence = applied.checked_add(1).ok_or_else(|| invalid("recovery sequence overflow"))?;
        if record.checkpoint != identity || record.sequence != sequence {
            return Err(invalid("stale or out-of-order recovery changeset"));
        }
        let count = length(&mut frame, MAX_ASSETS)?;
        let mut seen = BTreeSet::new();
        for _ in 0..count {
            let digest: [u8; 32] = take(&mut frame, 32)?.try_into().map_err(|_| invalid("invalid recovery digest"))?;
            let digest = Blake3::from_bytes(digest);
            if !seen.insert(digest) || snapshot.assets.get(&digest).is_some() {
                return Err(invalid("duplicate recovery asset"));
            }
            let len = length(&mut frame, MAX_ASSET_BYTES)?;
            let data = take(&mut frame, len)?;
            if hash(data) != digest {
                return Err(invalid("recovery asset checksum mismatch"));
            }
            snapshot.assets.insert(data)?;
        }
        if !frame.is_empty() {
            return Err(invalid("unexpected recovery frame data"));
        }
        record.changes.apply(&mut snapshot.project)?;
        snapshot.assets.validate_changed_assets(&snapshot.project, &record.changes)?;
        applied = sequence;
    }
    Ok(Recovered { snapshot, applied, truncated_tail: damage.is_some(), damage, legacy })
}

fn take<'a>(bytes: &mut &'a [u8], len: usize) -> Result<&'a [u8], Error> {
    let (head, tail) = bytes.split_at_checked(len).ok_or_else(|| invalid("truncated recovery data"))?;
    *bytes = tail;
    Ok(head)
}
fn length(bytes: &mut &[u8], cap: usize) -> Result<usize, Error> {
    let raw: [u8; 8] = take(bytes, 8)?.try_into().map_err(|_| invalid("invalid recovery length"))?;
    let len = usize::try_from(u64::from_le_bytes(raw)).map_err(|_| invalid("recovery length overflow"))?;
    if len > cap {
        return Err(invalid("recovery data exceeds size limit"));
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Change, Node};
    use nodecraft_types::{AssetRef, CanvasId, MediaMeta, NodeId, Value};
    #[test]
    fn admitted_replay_freezes_referenced_bytes_and_preserves_public_frame_validation_and_wire() {
        let mut assets = AssetStore::default();
        let digest = assets.insert(b"public text fixture").unwrap();
        let changes = Changeset::new(vec![Change::SetParam {
            canvas: CanvasId::from_bytes([1; 16]),
            id: NodeId::from_bytes([2; 16]),
            name: "text".into(),
            value: Some(Value::Media(AssetRef { hash: digest, mime: "text/plain".parse().unwrap(), meta: Default::default() })),
        }]);
        let identity = Blake3::from_bytes([3; 32]);
        let additional = BTreeSet::from([digest]);
        let expected = frame(identity, 17, &changes, &assets, &additional).unwrap();
        let admitted = AdmittedReplay::new(changes.clone(), &assets).unwrap();
        assets.retain(&Default::default());
        let (actual, added) = admitted.into_frame(identity, 17, &Default::default()).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(added, additional);
        assert!(frame(identity, 17, &changes, &assets, &additional).is_err(), "arbitrary callers must still validate their actual store");
        assert!(AdmittedReplay::new(changes, &assets).is_err());
        let v2_scalar = Changeset::new(vec![Change::SetParam {
            canvas: CanvasId::from_bytes([1; 16]),
            id: NodeId::from_bytes([2; 16]),
            name: "future".into(),
            value: Some(Value::Bool(true)),
        }]);
        assert!(frame(identity, 17, &v2_scalar, &assets, &Default::default()).is_ok());
    }
    fn replay_measurement(nodes: u128, frames: u64) {
        use nodecraft_graph::{Canvas, Position, Project};
        use nodecraft_types::ProjectId;
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Replay measurement").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let mut assets = AssetStore::default();
        let hash = assets.insert(b"Shared public fixture").unwrap();
        let value = Value::List(vec![Value::Media(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() })]);
        Changeset::new(
            (1..=nodes)
                .map(|id| {
                    let mut node = Node::new(NodeId::from_bytes(id.to_be_bytes()), "source.text");
                    node.params.insert("asset".into(), value.clone());
                    Change::AddNode { canvas, node }
                })
                .collect(),
        )
        .apply(&mut project)
        .unwrap();
        let snapshot = Snapshot { project, assets, epoch: 0 };
        let checkpoint = checkpoint(&snapshot).unwrap();
        let mut bytes = checkpoint.bytes;
        for sequence in 1..=frames {
            let changes =
                Changeset::new(vec![Change::MoveNode { canvas, id: NodeId::from_bytes(1u128.to_be_bytes()), pos: Position { x: sequence as f32, y: 1.0 } }]);
            bytes.extend(frame(checkpoint.identity, sequence, &changes, &snapshot.assets, &Default::default()).unwrap());
        }
        crate::assets::ASSET_NODE_VISITS.with(|visits| visits.set(0));
        let start = std::time::Instant::now();
        let recovered = recover(&bytes).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(recovered.applied, frames);
        assert!(recovered.damage.is_none());
        assert_eq!(crate::assets::ASSET_NODE_VISITS.with(|visits| visits.get()), nodes as usize, "checkpoint only; moves never revisit node asset data");
        eprintln!(
            "{nodes}-node/{frames}-frame recovery: {elapsed:?}; asset-node visits={nodes} (checkpoint only). Graph apply still clones/validates the project per frame."
        );
    }

    #[test]
    fn move_replay_does_not_repeat_whole_asset_walks() {
        replay_measurement(32, 64);
    }

    #[test]
    #[ignore = "manual release recovery measurement; no timing assertion"]
    fn two_thousand_node_two_thousand_frame_recovery_measurement() {
        replay_measurement(2000, 2000);
    }

    #[test]
    fn actual_future_pin_redo_is_charged_at_journal_record_and_frame_boundaries() {
        use nodecraft_graph::{Canvas, Comment, Position, Project};
        use nodecraft_types::{CommentId, ProjectId};
        let canvas = CanvasId::from_bytes([1; 16]);
        let root = CommentId::from_bytes([3; 16]);
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Journal budget").unwrap();
        nodecraft_graph::Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        nodecraft_graph::Change::PutComment { canvas, comment: Comment::new(root, "Root", Position::default()) }.apply(&mut project).unwrap();
        let node = Node::new(NodeId::from_bytes([2; 16]), "source.text");
        let forward = Changeset::new(vec![
            nodecraft_graph::Change::SetCommentPin { canvas, id: root, node: Some(node.id), pos: Position { x: 1.0, y: 2.0 } },
            nodecraft_graph::Change::AddNode { canvas, node },
        ]);
        let staged = forward.stage_certified(&project).unwrap();
        let redo = staged.actual_redo.unwrap();
        let assets = AssetStore::default();
        let record =
            crate::archive::bounded_json(&RecordRef { checkpoint: Blake3::from_bytes([0; 32]), sequence: u64::MAX, changes: &redo }, 4096).unwrap().len();
        let frame = record + 56;
        validate_replay_with_limits(&forward, &assets, record - 1, frame - 1).unwrap();
        validate_replay_with_limits(&staged.inverse, &assets, record - 1, frame - 1).unwrap();
        assert!(validate_replay_with_limits(&redo, &assets, record - 1, frame).is_err());
        assert!(validate_replay_with_limits(&redo, &assets, record, frame - 1).is_err());
        assert!(validate_replay_with_limits(&redo, &assets, record, frame).is_ok());
    }

    #[test]
    fn replay_budget_reserves_max_sequence_canonical_redo_and_all_binary_headers() {
        let mut assets = AssetStore::default();
        let hash = assets.insert(&vec![b'x'; 1024]).unwrap();
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut node = Node::new(NodeId::from_bytes([2; 16]), "test.text");
        node.params.insert("asset".into(), Value::Media(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() }));
        let add = Changeset::new(vec![Change::AddNode { canvas, node: node.clone() }]);
        let restore = Changeset::new(vec![Change::RestoreNode { canvas, node, edges: vec![], pins: vec![] }]);
        let json = crate::archive::bounded_json(&RecordRef { checkpoint: Blake3::from_bytes([0; 32]), sequence: u64::MAX, changes: &restore }, 4096).unwrap();
        let exact = json.len() + 56 + 40 + 1024;
        assert!(validate_replay_with_limits(&add, &assets, json.len(), exact).is_ok());
        assert!(validate_replay_with_limits(&add, &assets, json.len() - 1, exact).is_err());
        assert!(validate_replay_with_limits(&add, &assets, json.len(), exact - 1).is_err());
    }
}

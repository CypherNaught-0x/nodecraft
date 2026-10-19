#![cfg(not(target_arch = "wasm32"))]
#[cfg(feature = "validation-metrics")]
use nodecraft_format::asset_source::TRANSFER_BYTES;
use nodecraft_format::{
    ArchivePlan, AssetStore, CheckpointPlan, NativeStorage, ProjectStorage, RecordedAssets, Snapshot, StorageLocation,
    asset_source::{AssetRange, AssetSource, StreamMetrics, TransferCancel},
    journal::{AdmittedReplay, RecoveryDamageReason},
};
use nodecraft_graph::{Canvas, Change, Changeset, Node, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, Error, MediaMeta, NodeId, ProjectId, Value};
use std::{io::Read, sync::Arc};
fn canvas() -> CanvasId {
    CanvasId::from_bytes([2; 16])
}
fn node(n: u64) -> NodeId {
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&n.to_le_bytes());
    NodeId::from_bytes(bytes)
}
fn reference(hash: Blake3) -> AssetRef {
    AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() }
}
fn source(n: u64, hash: Blake3) -> Node {
    let mut value = Node::new(node(n), "source.text");
    value.params.insert("content".into(), Value::Media(reference(hash)));
    value
}
fn root(assets: AssetStore) -> Snapshot {
    let mut project = Project::new(ProjectId::from_bytes([1; 16]), "Root").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas(), "Canvas") }.apply(&mut project).unwrap();
    Snapshot { project, assets, epoch: 7 }
}
fn checkpoint() -> (Vec<u8>, Blake3) {
    let dir = tempfile::tempdir().unwrap();
    let mut storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
    let snapshot = root(AssetStore::with_context(storage.asset_context().unwrap()));
    let archive = ArchivePlan::prepare(&snapshot, storage.capability(), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let plan = CheckpointPlan::new(archive).unwrap();
    let id = plan.identity();
    storage.checkpoint_plan(&plan, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    (std::fs::read(dir.path().join("recovery")).unwrap(), id)
}
fn raw_frame(json: &[u8], assets: &[(Blake3, &[u8])]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(json.len() as u64).to_le_bytes());
    payload.extend_from_slice(json);
    payload.extend_from_slice(&(assets.len() as u64).to_le_bytes());
    for (hash, bytes) in assets {
        payload.extend_from_slice(hash.as_bytes());
        payload.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        payload.extend_from_slice(bytes);
    }
    let mut frame = Vec::new();
    frame.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    frame.extend_from_slice(blake3::hash(&payload).as_bytes());
    frame.extend_from_slice(&payload);
    frame
}
fn frame(identity: Blake3, sequence: u64, changes: Changeset, assets: &[(Blake3, &[u8])]) -> Vec<u8> {
    raw_frame(&serde_json::to_vec(&serde_json::json!({"checkpoint":identity,"sequence":sequence,"changes":changes})).unwrap(), assets)
}
fn title(text: &str) -> Changeset {
    Changeset::new(vec![Change::SetProjectTitle { title: text.into() }])
}
fn recover(bytes: &[u8]) -> Result<nodecraft_format::journal::Recovered, Error> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    std::fs::write(&path, bytes).unwrap();
    let storage = NativeStorage::new(path.clone()).unwrap();
    let result = storage.recover();
    assert_eq!(std::fs::read(path).unwrap(), bytes, "recovery preserves evidence");
    result
}
struct Pattern {
    length: u64,
    value: u8,
}
impl AssetSource for Pattern {
    fn byte_len(&self) -> u64 {
        self.length
    }
    fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
        let n = out.len().min(usize::try_from(self.length.saturating_sub(at)).unwrap_or(usize::MAX));
        out[..n].fill(self.value);
        Ok(n)
    }
}
#[test]
fn large_checkpoint_and_inline_asset_recover_after_source_delete_with_bounded_buffers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    let destination = dir.path().join("project.ncraft");
    let mut storage = NativeStorage::new(path.clone()).unwrap();
    let context = storage.asset_context().unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let mut assets = AssetStore::with_context(context.clone());
    let bytes = 20 * 1024 * 1024 + 1;
    let mut hashes = Vec::new();
    for value in [65, 66, 67] {
        hashes.push(assets.import_stream(&AssetRange::new(Arc::new(Pattern { length: bytes, value }), 0, bytes).unwrap(), &cancel, &metrics).unwrap());
    }
    let mut snapshot = root(assets);
    for (n, hash) in hashes.iter().enumerate() {
        Change::AddNode { canvas: canvas(), node: source(n as u64, *hash) }.apply(&mut snapshot.project).unwrap();
    }
    let archive = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let plan = CheckpointPlan::new(archive.clone()).unwrap();
    assert!(snapshot.assets.byte_len() > 48 * 1024 * 1024);
    assert_eq!(context.storage_usage().unwrap().journals, plan.byte_len(), "reserve before Save publication");
    storage.save_plan(&StorageLocation::Path(destination.clone()), &archive, &cancel, &metrics).unwrap();
    storage.checkpoint_plan(&plan, &cancel, &metrics).unwrap();
    let mut assets = snapshot.assets.clone();
    let added = assets.import_stream(&AssetRange::new(Arc::new(Pattern { length: bytes, value: 68 }), 0, bytes).unwrap(), &cancel, &metrics).unwrap();
    let changes = Changeset::new(vec![Change::AddNode { canvas: canvas(), node: source(3, added) }]);
    let (frame, _) = AdmittedReplay::new(changes.clone(), &assets)
        .unwrap()
        .into_plan(plan.identity(), 1, &RecordedAssets::new(plan.asset_lengths()).unwrap(), storage.capability(), &cancel, &metrics)
        .unwrap();
    storage.append_plan(&frame, &cancel, &metrics).unwrap();
    storage.flush().unwrap();
    let mut expected = snapshot.project.clone();
    changes.apply(&mut expected).unwrap();
    std::fs::write(&destination, b"overwritten source").unwrap();
    std::fs::remove_file(&destination).unwrap();
    let archive_bytes = archive.byte_len();
    drop((frame, plan, archive, assets, snapshot, storage, context));
    let storage = NativeStorage::new(path.clone()).unwrap();
    let context = storage.asset_context().unwrap();
    let measured = StreamMetrics::default();
    let recovered = storage.recover_with(&cancel, &measured).unwrap();
    assert_eq!(recovered.snapshot.project, expected);
    assert_eq!(recovered.snapshot.epoch, 7);
    assert_eq!(recovered.applied, 1);
    assert!(recovered.damage.is_none());
    assert_eq!(context.retained_usage().unwrap(), (4, 4 * bytes));
    assert_eq!(context.storage_usage().unwrap().verified, bytes, "unchanged checkpoint bodies remain indexed/unverified");
    assert_eq!(context.storage_usage().unwrap().bases, archive_bytes);
    assert!(recovered.snapshot.assets.hashes().all(|hash| recovered.snapshot.assets.get(hash).is_none()));
    for (n, hash) in hashes.into_iter().chain([added]).enumerate() {
        let lease = recovered.snapshot.assets.open_verified(&reference(hash), &cancel, &measured).unwrap();
        let mut first = [0];
        lease.reader(measured.clone()).read_exact(&mut first).unwrap();
        assert_eq!(first[0], 65 + n as u8);
    }
    #[cfg(feature = "validation-metrics")]
    {
        let stats = measured.stats();
        assert!(stats.source_bytes > archive_bytes + bytes);
        assert!(stats.maximum_read <= TRANSFER_BYTES as u64);
        assert!(stats.peak_transfer_bytes <= 3 * TRANSFER_BYTES as u64);
        assert_eq!(stats.live_transfer_bytes, 0);
    }
}
#[test]
fn physical_damage_returns_exact_validated_prefix_and_does_not_replay_later_frames() {
    let (checkpoint, id) = checkpoint();
    let mut prefix = checkpoint;
    prefix.extend(frame(id, 1, title("Prefix"), &[]));
    let good = frame(id, 2, title("Bad"), &[]);
    let later = frame(id, 3, title("Never"), &[]);
    let mut checksum = good.clone();
    checksum[8] ^= 1;
    let mut invalid_length = good.clone();
    invalid_length[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    for (bad, reason, append_later) in [
        (good[..17].to_vec(), RecoveryDamageReason::IncompleteHeader, false),
        (good[..good.len() - 1].to_vec(), RecoveryDamageReason::IncompletePayload, false),
        (checksum, RecoveryDamageReason::ChecksumMismatch, true),
        (vec![0; 40], RecoveryDamageReason::ZeroFilledHeader, true),
        (invalid_length, RecoveryDamageReason::InvalidFrameLength, true),
    ] {
        let mut bytes = prefix.clone();
        bytes.extend(bad);
        if append_later {
            bytes.extend(&later);
        }
        let recovered = recover(&bytes).unwrap();
        assert_eq!(recovered.applied, 1);
        assert_eq!(recovered.snapshot.project.title(), "Prefix");
        assert_eq!(recovered.damage.unwrap(), nodecraft_format::journal::RecoveryDamage { offset: prefix.len() as u64, reason });
    }
}
#[test]
fn valid_outer_semantics_are_hard_errors_but_bad_outer_wins_over_bad_inner() {
    let (checkpoint, id) = checkpoint();
    let body = b"new body";
    let hash = Blake3::from_bytes(*blake3::hash(body).as_bytes());
    let mismatched = Blake3::from_bytes([7; 32]);
    for mut bad in [
        raw_frame(b"not-json", &[]),
        frame(id, 2, title("Wrong sequence"), &[]),
        frame(mismatched, 1, title("Wrong root"), &[]),
        frame(id, 1, title("Duplicate"), &[(hash, body), (hash, body)]),
        frame(id, 1, title("Wrong hash"), &[(mismatched, body)]),
    ] {
        let mut valid_outer = checkpoint.clone();
        valid_outer.extend(&bad);
        assert!(recover(&valid_outer).is_err());
        bad[8] ^= 1;
        let mut bad_outer = checkpoint.clone();
        bad_outer.extend(&bad);
        let recovered = recover(&bad_outer).unwrap();
        assert_eq!(recovered.applied, 0);
        assert_eq!(recovered.damage.unwrap().reason, RecoveryDamageReason::ChecksumMismatch);
    }
}
#[test]
fn checkpoint_header_archive_digest_truncation_and_version_are_hard_errors() {
    let (checkpoint, _) = checkpoint();
    let mut version = checkpoint.clone();
    version[7] = b'9';
    let mut length = checkpoint.clone();
    length[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    let mut header = checkpoint.clone();
    header[48] ^= 1;
    let mut digest = checkpoint.clone();
    digest[16] ^= 1;
    let mut h = blake3::Hasher::new();
    h.update(b"nodecraft.recovery.header.v4");
    h.update(&digest[..48]);
    digest[48..80].copy_from_slice(h.finalize().as_bytes());
    for bytes in [Vec::new(), checkpoint[..79].to_vec(), checkpoint[..checkpoint.len() - 1].to_vec(), version, length, header, digest] {
        assert!(recover(&bytes).is_err());
    }
}
#[test]
fn inline_mime_mismatch_cannot_be_hidden_by_a_later_removal() {
    let (checkpoint, id) = checkpoint();
    let body = b"plain text, not png";
    let hash = Blake3::from_bytes(*blake3::hash(body).as_bytes());
    let mut wrong = source(1, hash);
    if let Some(Value::Media(asset)) = wrong.params.get_mut("content") {
        asset.mime = "image/png".parse().unwrap();
        asset.meta.width = Some(100);
    }
    let first = frame(id, 1, Changeset::new(vec![Change::AddNode { canvas: canvas(), node: wrong }]), &[(hash, body)]);
    let later = frame(id, 2, Changeset::new(vec![Change::RemoveNode { canvas: canvas(), id: node(1) }]), &[]);
    let mut bytes = checkpoint;
    bytes.extend(first);
    bytes.extend(later);
    assert!(recover(&bytes).is_err());
}
#[test]
fn one_frame_with_more_than_32_assets_uses_one_verified_aggregate_backing() {
    let (checkpoint, id) = checkpoint();
    let bodies: Vec<_> = (0u64..40).map(|n| format!("public fixture {n}").into_bytes()).collect();
    let hashes: Vec<_> = bodies.iter().map(|b| Blake3::from_bytes(*blake3::hash(b).as_bytes())).collect();
    let changes = Changeset::new(hashes.iter().enumerate().map(|(n, hash)| Change::AddNode { canvas: canvas(), node: source(n as u64, *hash) }).collect());
    let inline: Vec<_> = hashes.iter().zip(&bodies).map(|(hash, b)| (*hash, b.as_slice())).collect();
    let mut bytes = checkpoint;
    bytes.extend(frame(id, 1, changes, &inline));
    let recovered = recover(&bytes).unwrap();
    assert_eq!(recovered.applied, 1);
    assert_eq!(recovered.snapshot.assets.len(), 40);
    for (hash, body) in hashes.iter().zip(&bodies) {
        assert_eq!(
            recovered
                .snapshot
                .assets
                .open_verified(&reference(*hash), &TransferCancel::default(), &StreamMetrics::default())
                .unwrap()
                .read_small(4096, &StreamMetrics::default())
                .unwrap(),
            *body
        );
    }
}

#[test]
fn real_recorded_cap_survives_gc_and_failed_save_then_checkpoint_resets_for_history_restore() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    let mut storage = NativeStorage::new(path.clone()).unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let mut assets = AssetStore::with_context(storage.asset_context().unwrap());
    let mut hashes = Vec::new();
    for n in 0u64..1024 {
        hashes.push(assets.insert(format!("asset {n}").as_bytes()).unwrap());
    }
    let mut snapshot = root(assets);
    Changeset::new(hashes.iter().enumerate().map(|(n, hash)| Change::AddNode { canvas: canvas(), node: source(n as u64, *hash) }).collect())
        .apply(&mut snapshot.project)
        .unwrap();
    let mut history = snapshot.assets.clone();
    history.retain(&std::collections::BTreeSet::from([hashes[0]]));
    let old_archive = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let old_checkpoint = CheckpointPlan::new(old_archive.clone()).unwrap();
    let old_id = old_checkpoint.identity();
    let full = RecordedAssets::new(old_checkpoint.asset_lengths()).unwrap();
    assert_eq!(full.len(), 1024);
    storage.checkpoint_plan(&old_checkpoint, &cancel, &metrics).unwrap();
    let remove = Changeset::new((0u64..1024).map(|n| Change::RemoveNode { canvas: canvas(), id: node(n) }).collect());
    let (removal, _) =
        AdmittedReplay::new(remove.clone(), &snapshot.assets).unwrap().into_plan(old_id, 1, &full, storage.capability(), &cancel, &metrics).unwrap();
    storage.append_plan(&removal, &cancel, &metrics).unwrap();
    remove.apply(&mut snapshot.project).unwrap();
    drop((removal, old_checkpoint, old_archive));
    snapshot.assets.retain(&Default::default());
    snapshot.assets.collect_garbage().unwrap();
    assert_eq!(snapshot.assets.context().retained_usage().unwrap().0, 1, "only private history pins the old identity");
    let fresh = snapshot.assets.insert(b"new after live GC").unwrap();
    let add = Changeset::new(vec![Change::AddNode { canvas: canvas(), node: source(2000, fresh) }]);
    let before = std::fs::read(&path).unwrap();
    assert!(AdmittedReplay::new(add.clone(), &snapshot.assets).unwrap().into_plan(old_id, 2, &full, storage.capability(), &cancel, &metrics).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let archive = ArchivePlan::prepare(&snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let checkpoint = CheckpointPlan::new(archive.clone()).unwrap();
    assert!(checkpoint.asset_lengths().is_empty());
    assert!(storage.save_plan(&StorageLocation::Path(dir.path().to_path_buf()), &archive, &cancel, &metrics).is_err());
    let (premature, _) = AdmittedReplay::new(add.clone(), &snapshot.assets)
        .unwrap()
        .into_plan(old_id, 2, &RecordedAssets::default(), storage.capability(), &cancel, &metrics)
        .unwrap();
    assert!(storage.append_plan(&premature, &cancel, &metrics).is_err(), "failed Save must not reset the actual journal ledger");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(premature);
    let destination = dir.path().join("after-gc.ncraft");
    storage.save_plan(&StorageLocation::Path(destination.clone()), &archive, &cancel, &metrics).unwrap();
    storage.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
    let (addition, next) = AdmittedReplay::new(add, &snapshot.assets)
        .unwrap()
        .into_plan(checkpoint.identity(), 1, &RecordedAssets::default(), storage.capability(), &cancel, &metrics)
        .unwrap();
    storage.append_plan(&addition, &cancel, &metrics).unwrap();
    drop(addition);
    let restore = Changeset::new(vec![Change::RestoreNode { canvas: canvas(), node: source(0, hashes[0]), edges: vec![], pins: vec![] }]);
    let (restoration, next) =
        AdmittedReplay::new(restore, &history).unwrap().into_plan(checkpoint.identity(), 2, &next, storage.capability(), &cancel, &metrics).unwrap();
    assert_eq!(next.len(), 2);
    storage.append_plan(&restoration, &cancel, &metrics).unwrap();
    storage.flush().unwrap();
    std::fs::remove_file(destination).unwrap();
    drop((restoration, checkpoint, archive, snapshot, history, storage));
    let storage = NativeStorage::new(path).unwrap();
    let recovered = storage.recover().unwrap();
    assert_eq!(recovered.applied, 2);
    assert_eq!(recovered.snapshot.assets.len(), 2);
    assert_eq!(recovered.snapshot.project.canvases().get(&canvas()).unwrap().nodes().len(), 2);
    assert_eq!(recovered.snapshot.assets.open_verified(&reference(hashes[0]), &cancel, &metrics).unwrap().read_small(4096, &metrics).unwrap(), b"asset 0");
    assert_eq!(
        recovered.snapshot.assets.open_verified(&reference(fresh), &cancel, &metrics).unwrap().read_small(4096, &metrics).unwrap(),
        b"new after live GC"
    );
}
#[test]
fn many_small_frames_pack_before_backing_exhaustion_and_cancel_refuses_before_reads() {
    let (checkpoint, id) = checkpoint();
    let mut bytes = checkpoint;
    for n in 0u64..40 {
        let body = format!("sequential frame {n}").into_bytes();
        let hash = Blake3::from_bytes(*blake3::hash(&body).as_bytes());
        bytes.extend(frame(id, n + 1, Changeset::new(vec![Change::AddNode { canvas: canvas(), node: source(n, hash) }]), &[(hash, &body)]));
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery");
    std::fs::write(&path, &bytes).unwrap();
    let storage = NativeStorage::new(path.clone()).unwrap();
    let cancel = TransferCancel::default();
    cancel.cancel();
    let metrics = StreamMetrics::default();
    assert!(storage.recover_with(&cancel, &metrics).is_err());
    #[cfg(feature = "validation-metrics")]
    assert_eq!(metrics.stats().source_bytes, 0);
    let recovered = storage.recover().unwrap();
    assert_eq!(recovered.applied, 40);
    assert_eq!(recovered.snapshot.assets.len(), 40);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn native_open_preserves_exact_v1_archive_inside_streaming_and_fresh_recovery_eagerly_verifies_it() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("v1.ncraft");
    let recovery_path = dir.path().join("recovery");
    let mut memory = AssetStore::default();
    let hash = memory.insert(b"frozen v1 text").unwrap();
    let mut snapshot = root(memory);
    Change::AddNode { canvas: canvas(), node: source(1, hash) }.apply(&mut snapshot.project).unwrap();
    let original = nodecraft_format::encode(&snapshot).unwrap();
    std::fs::write(&source_path, &original).unwrap();
    let mut storage = NativeStorage::new(recovery_path.clone()).unwrap();
    let prepared = storage.prepare_open_snapshot(&StorageLocation::Path(source_path.clone()), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let (loaded, checkpoint, _) = prepared.into_parts();
    let mut exact = Vec::new();
    checkpoint.archive().write_to(&mut exact, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert_eq!(exact, original);
    storage.checkpoint_plan(&checkpoint, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    storage.commit_open();
    std::fs::remove_file(source_path).unwrap();
    drop((loaded, checkpoint, storage));
    let storage = NativeStorage::new(recovery_path).unwrap();
    let recovered = storage.recover().unwrap();
    assert_eq!(recovered.snapshot, snapshot);
    assert_eq!(
        storage.asset_context().unwrap().storage_usage().unwrap().verified,
        b"frozen v1 text".len() as u64,
        "v1 remains eager under the shared stage token"
    );
    assert_eq!(
        recovered
            .snapshot
            .assets
            .open_verified(&reference(hash), &TransferCancel::default(), &StreamMetrics::default())
            .unwrap()
            .read_small(4096, &StreamMetrics::default())
            .unwrap(),
        b"frozen v1 text"
    );
}
fn wrapped_archive(archive: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"NCRJ0004");
    bytes.extend_from_slice(&(archive.len() as u64).to_le_bytes());
    bytes.extend_from_slice(blake3::hash(archive).as_bytes());
    let mut hash = blake3::Hasher::new();
    hash.update(b"nodecraft.recovery.header.v4");
    hash.update(&bytes);
    bytes.extend_from_slice(hash.finalize().as_bytes());
    bytes.extend_from_slice(archive);
    bytes
}
#[test]
fn valid_checkpoint_digest_does_not_bypass_eager_v1_asset_hash_or_deferred_streaming_consumer_verification() {
    use std::io::{Cursor, Write};
    let hash = Blake3::from_bytes(*blake3::hash(b"good").as_bytes());
    let mut project = root(AssetStore::default()).project;
    Change::AddNode { canvas: canvas(), node: source(1, hash) }.apply(&mut project).unwrap();
    for version in [1, 4] {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("project.json", options).unwrap();
        writer.write_all(&serde_json::to_vec(&serde_json::json!({"version":version,"epoch":0,"project":project})).unwrap()).unwrap();
        writer.start_file(format!("assets/{hash}"), options).unwrap();
        writer.write_all(b"evil").unwrap();
        let archive = writer.finish().unwrap().into_inner();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery");
        std::fs::write(&path, wrapped_archive(&archive)).unwrap();
        let storage = NativeStorage::new(path).unwrap();
        if version == 1 {
            assert!(storage.recover().is_err(), "v1 must verify even without a consuming frame");
        } else {
            let recovered = storage.recover().unwrap();
            assert_eq!(storage.asset_context().unwrap().storage_usage().unwrap().verified, 0);
            let error = recovered.snapshot.assets.open_verified(&reference(hash), &TransferCancel::default(), &StreamMetrics::default()).err().unwrap();
            assert_eq!(error.message, "asset content does not match its expected hash or checksum");
            assert!(
                ArchivePlan::prepare(&recovered.snapshot, storage.capability(), &TransferCancel::default(), &StreamMetrics::default()).is_err(),
                "Save refuses deferred corrupt content before output"
            );
        }
    }
}

#[test]
fn stored_restored_content_corruption_requires_replacement_checkpoint_before_append() {
    use std::io::{Cursor, Write};
    let hash = Blake3::from_bytes(*blake3::hash(b"good").as_bytes());
    let project = root(AssetStore::default()).project;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("project.json", options).unwrap();
    writer.write_all(&serde_json::to_vec(&serde_json::json!({"version":4,"epoch":0,"project":project})).unwrap()).unwrap();
    writer.start_file(format!("assets/{hash}"), options).unwrap();
    writer.write_all(b"evil").unwrap();
    let archive = writer.finish().unwrap().into_inner();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.ncraft");
    let path = dir.path().join("recovery");
    std::fs::write(&input, archive).unwrap();
    let mut storage = NativeStorage::new(path.clone()).unwrap();
    let context = storage.asset_context().unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let prepared = storage.prepare_open_snapshot(&StorageLocation::Path(input), &cancel, &metrics).unwrap();
    let (snapshot, checkpoint, _) = prepared.into_parts();
    storage.checkpoint_plan(&checkpoint, &cancel, &metrics).unwrap();
    storage.commit_open();
    let identity = checkpoint.identity();
    let recorded = RecordedAssets::new(checkpoint.asset_lengths()).unwrap();
    // The same valid asset may arrive from an independent import/history store.
    let mut good = AssetStore::with_context(context.clone());
    assert_eq!(good.insert(b"good").unwrap(), hash);
    let changes = Changeset::new(vec![Change::AddNode { canvas: canvas(), node: source(1, hash) }]);
    let (addition, after) = AdmittedReplay::new(changes, &good).unwrap().into_plan(identity, 1, &recorded, storage.capability(), &cancel, &metrics).unwrap();
    storage.append_plan(&addition, &cancel, &metrics).unwrap();
    storage.flush().unwrap();
    drop((snapshot, checkpoint, addition, good));
    let before = std::fs::read(&path).unwrap();
    let error = storage.recover().err().unwrap();
    assert_eq!(error.kind, nodecraft_types::ErrorKind::InvalidInput);
    assert_eq!(error.message, "asset content does not match its expected hash or checksum");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let empty = AssetStore::with_context(context.clone());
    let (next, _) = AdmittedReplay::new(title("Still appendable after hard corruption"), &empty)
        .unwrap()
        .into_plan(identity, 2, &after, storage.capability(), &cancel, &metrics)
        .unwrap();
    let result = storage.append_plan(&next, &cancel, &metrics);
    assert!(result.is_err(), "hard content corruption must require a replacement checkpoint");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(next);
    // Only an explicit new checkpoint clears the failed generation. Preparation and
    // successful append use the same public typed routes as ordinary native Save.
    let replacement_snapshot = root(empty);
    let archive = ArchivePlan::prepare(&replacement_snapshot, storage.capability(), &cancel, &metrics).unwrap();
    let replacement = CheckpointPlan::new(archive).unwrap();
    storage.checkpoint_plan(&replacement, &cancel, &metrics).unwrap();
    let recorded = RecordedAssets::new(replacement.asset_lengths()).unwrap();
    let (next, _) = AdmittedReplay::new(title("After replacement checkpoint"), &replacement_snapshot.assets)
        .unwrap()
        .into_plan(replacement.identity(), 1, &recorded, storage.capability(), &cancel, &metrics)
        .unwrap();
    storage.append_plan(&next, &cancel, &metrics).unwrap();
    storage.flush().unwrap();
    assert_eq!(storage.recover().unwrap().snapshot.project.title(), "After replacement checkpoint");
}

//! Actual typed native split consumers; no legacy host fallback or forged verified asset.
use super::*;
use nodecraft_engine::split::{Split, split_grid};
use nodecraft_format::asset_source::AssetRange;
use nodecraft_types::{AssetRef, Blake3, MediaMeta};
use std::sync::Arc;
const SOURCE: NodeId = NodeId::from_bytes([70; 16]);
fn png(side: u32) -> Vec<u8> {
    nodecraft_media::encode_png(side, side, &[20, 30, 40, 255].repeat((side * side) as usize), &Default::default()).unwrap()
}
fn input_node(s: &Session, hash: Blake3, width: u32, height: u32) -> Node {
    let supplied = [(
        "asset".into(),
        Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta { width: Some(width), height: Some(height), ..Default::default() } }),
    )]
    .into();
    s.kinds().get("source.image").unwrap().instantiate_instance(SOURCE, Default::default(), &supplied).unwrap()
}
fn add_source(s: &mut Session, bytes: &[u8], side: u32) {
    let hash = s.import_asset(bytes).unwrap();
    let node = input_node(s, hash, side, side);
    s.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
}
fn ids(first: u8, count: usize) -> Vec<NodeId> {
    (0..count).map(|n| NodeId::from_bytes([first + n as u8; 16])).collect()
}
fn split(s: &mut Session, rows: u32, columns: u32, ids: &[NodeId]) -> Result<Vec<NodeId>, Error> {
    split_grid(s, &Split { canvas: C, node: SOURCE, rows, columns, ids, step: (100.0, 100.0) })
}
#[test]
fn real_twenty_mib_source_and_sixty_mib_project_split_without_resident_store_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut s = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let mut hashes = Vec::new();
    for fill in [1, 2, 3] {
        let len = 20 * 1024 * 1024;
        let range = AssetRange::new(Arc::new(Padded { bytes: len, fill }), 0, len).unwrap();
        hashes.push(s.import_asset_from(&range, &cancel, &metrics).unwrap());
    }
    let source = input_node(&s, hashes[0], 2, 3);
    s.dispatch("node.add", &json!({"canvas":C,"node":source})).unwrap();
    assert_eq!(s.assets().byte_len(), 60 * 1024 * 1024);
    assert!(s.assets().get(&hashes[0]).is_none());
    let before = s.document().clone();
    let made = split(&mut s, 1, 2, &ids(80, 2)).unwrap();
    assert_eq!(made, ids(80, 2));
    for id in made {
        let Value::Media(reference) = s.document().canvases()[&C].nodes()[&id].params["asset"].clone() else { panic!("media") };
        let lease = s.assets().open_verified(&reference, &cancel, &metrics).unwrap();
        let bytes = lease.read_small(1024, &metrics).unwrap();
        let (meta, _) = nodecraft_media::decode_image(&bytes, &Default::default()).unwrap();
        assert_eq!((meta.width, meta.height), (1, 3));
        assert!(s.assets().get(&reference.hash).is_none());
    }
    for hash in hashes {
        assert!(s.assets().contains(&hash), "unrelated staged20MiB inputs survive");
    }
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(host.0.borrow().native.recover().unwrap().snapshot.project, *s.document());
}
#[test]
fn native_uniform_sixty_four_tile_batch_needs_one_count_slot_and_repeat_is_free() {
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut s = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    add_source(&mut s, &png(8), 8);
    for n in 0..1022 {
        s.import_asset(format!("staged {n}").as_bytes()).unwrap();
    }
    assert_eq!(s.assets().len(), 1023);
    split(&mut s, 8, 8, &ids(80, 64)).unwrap();
    assert_eq!(s.assets().len(), 1024);
    let before = s.document().clone();
    split(&mut s, 8, 8, &ids(150, 64)).unwrap();
    assert_eq!(s.assets().len(), 1024);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
    assert_eq!(host.0.borrow().native.recover().unwrap().snapshot.project, *s.document());
}
#[test]
fn native_preflight_and_append_refusal_preserve_staged_assets_readers_and_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut s = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    add_source(&mut s, &png(2), 2);
    let hash = s.import_asset(b"staged A").unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let lease = s.assets().open_verified(&AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }, &cancel, &metrics).unwrap();
    let receipt = s.prepare_reader_imports(&mut [&mut std::io::Cursor::new(b"pending B")], &cancel, &metrics).unwrap();
    let before = s.document().clone();
    assert!(split(&mut s, 2, 2, &[SOURCE; 4]).is_err());
    assert_eq!(s.document(), &before);
    assert!(s.assets().contains(&hash));
    s.dispatch_with_imports("node.move", &movement(1), receipt).unwrap();
    let before = s.document().clone();
    let revision = s.revision();
    let events = s.events_since(0).unwrap();
    let hashes = s.assets().hashes().copied().collect::<Vec<_>>();
    let journal = std::fs::read(dir.path().join("recovery")).unwrap();
    host.0.borrow_mut().fail_append = true;
    assert!(split(&mut s, 2, 2, &ids(80, 4)).is_err());
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
    assert_eq!(s.events_since(0).unwrap(), events);
    assert_eq!(s.assets().hashes().copied().collect::<Vec<_>>(), hashes);
    assert!(s.file_status().recovery_required);
    assert_eq!(std::fs::read(dir.path().join("recovery")).unwrap(), journal);
    assert_eq!(lease.read_small(1024, &metrics).unwrap(), b"staged A");
}
#[test]
fn corrupt_native_source_or_existing_tile_never_gets_repaired_by_the_split() {
    use std::io::Write;
    for bad_source in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let mut s = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
        let input = png(2);
        let tile = png(1);
        let source_hash = Blake3::from_bytes(*blake3::hash(&input).as_bytes());
        let tile_hash = Blake3::from_bytes(*blake3::hash(&tile).as_bytes());
        let mut project = s.document().clone();
        Change::AddNode { canvas: C, node: input_node(&s, source_hash, 2, 2) }.apply(&mut project).unwrap();
        let path = dir.path().join("indexed.ncraft");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("project.json", options).unwrap();
        zip.write_all(&serde_json::to_vec(&json!({"version":2,"epoch":0,"project":project})).unwrap()).unwrap();
        let bad_input = vec![b'x'; input.len()];
        let bad_tile = vec![b'x'; tile.len()];
        let mut bodies = [
            (source_hash, if bad_source { bad_input.as_slice() } else { input.as_slice() }),
            (tile_hash, if bad_source { tile.as_slice() } else { bad_tile.as_slice() }),
        ];
        bodies.sort_by_key(|(hash, _)| *hash);
        for (hash, body) in bodies {
            zip.start_file(format!("assets/{hash}"), options).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();
        s.dispatch("file.open", &json!({"location":path,"discard_dirty":true})).unwrap();
        let a = s.import_asset(b"unrelated A").unwrap();
        let before = s.document().clone();
        let status = s.file_status();
        let journal = std::fs::read(dir.path().join("recovery")).unwrap();
        assert!(split(&mut s, 2, 2, &ids(80, 4)).is_err());
        assert_eq!(s.document(), &before);
        assert_eq!(s.file_status(), status);
        assert_eq!(std::fs::read(dir.path().join("recovery")).unwrap(), journal);
        assert!(s.assets().contains(&a));
        let bad = if bad_source {
            AssetRef { hash: source_hash, mime: "image/png".parse().unwrap(), meta: Default::default() }
        } else {
            AssetRef { hash: tile_hash, mime: "image/png".parse().unwrap(), meta: Default::default() }
        };
        assert!(s.assets().open_verified(&bad, &TransferCancel::default(), &StreamMetrics::default()).is_err());
    }
}

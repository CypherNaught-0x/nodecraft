//! Real native authority and opaque candidate composition for clipboard admission.
use super::*;
use nodecraft_format::{
    AssetStore, NativeStorage, Snapshot,
    asset_source::{AssetRange, MemorySource, StreamMetrics, TransferCancel},
};
use nodecraft_types::{AssetRef, Blake3, MediaMeta};
use std::{io::Read, sync::Arc};

fn native() -> (tempfile::TempDir, Session) {
    let dir = tempfile::tempdir().unwrap();
    let storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
    let session = Session::with_storage(Snapshot { project: project(), assets: AssetStore::default(), epoch: 0 }, Box::new(storage)).unwrap();
    (dir, session)
}
fn media_node(id: NodeId, asset: AssetRef) -> Node {
    let mut node = Node::new(id, "test.unknown");
    node.config.insert("nested".into(), GraphValue::List(vec![GraphValue::List(vec![GraphValue::Media(asset)])]));
    node
}
fn reference(hash: Blake3) -> AssetRef {
    AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }
}
#[test]
fn native_above_small_limit_pastes_cuts_and_replays_without_resident_bytes() {
    let (dir, mut s) = native();
    let bytes = 17 * 1024 * 1024u64;
    let hash = s.import_asset_reader(&mut std::io::repeat(b'a').take(bytes), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert_eq!(s.assets().descriptor(&hash).unwrap().bytes, bytes);
    assert!(s.assets().get(&hash).is_none());
    let node = media_node(B, reference(hash));
    let paste = json!({"canvas":C,"nodes":[node]});
    s.enabled("canvas.paste", &paste).unwrap();
    s.dispatch("canvas.paste", &paste).unwrap();
    let before = s.document().clone();
    let next = NodeId::from_bytes([13; 16]);
    let pasted = media_node(next, reference(hash));
    let paste = json!({"canvas":C,"nodes":[pasted]});
    s.dispatch_clipboard_cut(&json!({"canvas":C,"nodes":[B]}), &paste).unwrap();
    assert!(!s.document().canvases().get(&C).unwrap().nodes().contains_key(&B));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
    s.dispatch("edit.redo", &json!({})).unwrap();
    s.dispatch("canvas.paste", &paste).unwrap();
    assert!(s.assets().get(&hash).is_none(), "clipboard must never materialize a resident convenience body");
    let expected = s.document().clone();
    drop(s);
    let storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
    let recovered = storage.recover().unwrap();
    assert_eq!(recovered.snapshot.project, expected);
    recovered.snapshot.assets.validate_reference(&reference(hash)).unwrap();
}
#[test]
fn opaque_candidate_paste_uses_candidate_authority_and_refusal_keeps_live_assets() {
    for native_host in [false, true] {
        let (_dir, mut s) = if native_host { native() } else { (tempfile::tempdir().unwrap(), Session::new(project()).unwrap()) };
        let old = s.import_asset(b"unrelated pre-existing staged asset").unwrap();
        let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"new clipboard candidate".as_slice()))), 0, 23).unwrap();
        let prepared = s.prepare_imports(std::slice::from_ref(&range), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        let hash = prepared.hashes()[0];
        assert!(!s.assets().contains(&hash));
        s.dispatch_with_imports("canvas.paste", &json!({"canvas":C,"nodes":[media_node(B,reference(hash))]}), prepared).unwrap();
        assert!(s.assets().contains(&old) && s.assets().contains(&hash));
        let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"refused clipboard candidate".as_slice()))), 0, 27).unwrap();
        let prepared = s.prepare_imports(&[range], &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        let refused = prepared.hashes()[0];
        let mut asset = reference(refused);
        asset.mime = "image/png".parse().unwrap();
        let before = s.document().clone();
        let revision = s.revision();
        let events = s.events_since(0).unwrap();
        let identity = s.admission_identity.clone();
        let assets = s.assets().clone();
        assert!(s.dispatch_with_imports("canvas.paste", &json!({"canvas":C,"nodes":[media_node(NodeId::from_bytes([14;16]),asset)]}), prepared).is_err());
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), revision);
        assert_eq!(s.events_since(0).unwrap(), events);
        assert!(Arc::ptr_eq(&identity, &s.admission_identity));
        assert_eq!(s.assets(), &assets);
        assert!(s.assets().contains(&old) && !s.assets().contains(&refused));
    }
}
#[test]
fn native_missing_mime_and_dimensions_refuse_without_publishing() {
    let (_dir, mut s) = native();
    let png = nodecraft_media::encode_png(1, 1, &[30, 60, 90, 255], &Default::default()).unwrap();
    let hash = s.import_asset(&png).unwrap();
    assert!(s.assets().get(&hash).is_none());
    let good = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta { width: Some(1), height: Some(1), ..Default::default() } };
    let mut wrong_mime = good.clone();
    wrong_mime.mime = "text/plain".parse().unwrap();
    let mut wrong_size = good.clone();
    wrong_size.meta.height = Some(2);
    let mut missing = good.clone();
    missing.hash = Blake3::from_bytes([99; 32]);
    let before = s.document().clone();
    let revision = s.revision();
    let identity = s.admission_identity.clone();
    for asset in [wrong_mime, wrong_size, missing] {
        let p = json!({"canvas":C,"nodes":[media_node(B,asset)]});
        assert!(s.enabled("canvas.paste", &p).is_err());
        assert!(s.dispatch("canvas.paste", &p).is_err());
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), revision);
        assert!(Arc::ptr_eq(&identity, &s.admission_identity));
        assert!(s.events_since(0).unwrap().is_empty());
        assert!(!s.can_undo() && !s.can_redo());
    }
    s.dispatch("canvas.paste", &json!({"canvas":C,"nodes":[media_node(B,good)]})).unwrap();
}

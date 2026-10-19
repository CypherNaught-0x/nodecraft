//! Public small-file adapter controls on the actual streaming native host.
use super::*;
use nodecraft_engine::import::{Imported, Incoming, import_files};
use nodecraft_format::asset_source::{AssetRange, AssetSource};
use nodecraft_graph::Position;
use std::{io::Write, sync::Arc};
fn drop_image(s: &mut Session, canvas: CanvasId, ids: &[NodeId]) -> Result<Imported, Error> {
    let files = ids.iter().map(|_| Incoming { name: "image.png", bytes: PNG }).collect::<Vec<_>>();
    import_files(s, canvas, Position { x: 0.0, y: 0.0 }, 100.0, &files, ids)
}
#[test]
fn native_project_above_resident_limit_accepts_small_media_without_resident_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut s = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    for fill in [1, 2, 3] {
        let size = 20 * 1024 * 1024;
        let range = AssetRange::new(Arc::new(Padded { bytes: size, fill }), 0, size).unwrap();
        s.import_asset_from(&range, &cancel, &metrics).unwrap();
    }
    assert_eq!(s.assets().byte_len(), 60 * 1024 * 1024);
    let id = NodeId::from_bytes([30; 16]);
    let imported = drop_image(&mut s, C, &[id]).unwrap();
    assert_eq!(imported.nodes, [id]);
    assert!(imported.skipped.is_empty());
    assert_eq!(s.assets().len(), 4);
    let hash = nodecraft_types::Blake3::from_bytes(*blake3::hash(PNG).as_bytes());
    assert!(s.assets().get(&hash).is_none(), "native content stays disk backed");
    let lease = s.assets().open_verified(&asset(hash), &cancel, &metrics).unwrap();
    assert_eq!(lease.read_small(1024, &metrics).unwrap(), PNG);
    assert_eq!(host.0.borrow().appends, 1);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(!s.can_undo());
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(host.0.borrow().native.recover().unwrap().snapshot.project, *s.document());
}
#[test]
fn native_count_full_duplicate_uses_spool_and_batch_charges_one_new_identity() {
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut s = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    for n in 0..1023 {
        s.import_asset(format!("existing {n}").as_bytes()).unwrap();
    }
    let ids = [31, 32].map(|n| NodeId::from_bytes([n; 16]));
    assert_eq!(drop_image(&mut s, C, &ids).unwrap().nodes, ids);
    assert_eq!(s.assets().len(), 1024);
    let id = NodeId::from_bytes([33; 16]);
    assert_eq!(drop_image(&mut s, C, &[id]).unwrap().nodes, [id]);
    assert_eq!(s.assets().len(), 1024);
    assert_eq!(host.0.borrow().appends, 2);
}
#[test]
fn native_refusal_preserves_unrelated_pins_and_ambiguous_append_only_latches_status() {
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut s = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    let hash = s.import_asset(b"unrelated staged A").unwrap();
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let lease = s
        .assets()
        .open_verified(&nodecraft_types::AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: Default::default() }, &cancel, &metrics)
        .unwrap();
    let receipt = s.prepare_reader_imports(&mut [&mut std::io::Cursor::new(b"pending B")], &cancel, &metrics).unwrap();
    let before = s.document().clone();
    let revision = s.revision();
    let events = s.events_since(0).unwrap();
    let status = s.file_status();
    assert!(drop_image(&mut s, CanvasId::from_bytes([99; 16]), &[NodeId::from_bytes([34; 16])]).is_err());
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
    assert_eq!(s.events_since(0).unwrap(), events);
    assert_eq!(s.file_status(), status);
    assert_eq!(host.0.borrow().appends, 0);
    assert_eq!(s.assets().len(), 1);
    assert_eq!(lease.read_small(1024, &metrics).unwrap(), b"unrelated staged A");
    s.dispatch_with_imports("node.move", &movement(1), receipt).unwrap();
    let before = s.document().clone();
    let revision = s.revision();
    let events = s.events_since(0).unwrap();
    let hashes = s.assets().hashes().copied().collect::<Vec<_>>();
    let journal = std::fs::read(dir.path().join("recovery")).unwrap();
    host.0.borrow_mut().fail_append = true;
    assert!(drop_image(&mut s, C, &[NodeId::from_bytes([35; 16])]).is_err());
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
    assert_eq!(s.events_since(0).unwrap(), events);
    assert_eq!(s.assets().hashes().copied().collect::<Vec<_>>(), hashes);
    assert_eq!(std::fs::read(dir.path().join("recovery")).unwrap(), journal, "spy failure occurs before real append; status is still conservatively latched");
    assert!(s.file_status().recovery_required);
    assert_eq!(lease.read_small(1024, &metrics).unwrap(), b"unrelated staged A");
}
fn archive(path: &std::path::Path, project: &Project, hash: nodecraft_types::Blake3, body: &[u8]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("project.json", options).unwrap();
    zip.write_all(&serde_json::to_vec(&json!({"version":4,"epoch":0,"project":project})).unwrap()).unwrap();
    zip.start_file(format!("assets/{hash}"), options).unwrap();
    zip.write_all(body).unwrap();
    zip.finish().unwrap();
}
#[test]
fn good_duplicate_input_never_repairs_corrupt_indexed_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt.ncraft");
    let hash = nodecraft_types::Blake3::from_bytes(*blake3::hash(PNG).as_bytes());
    let bad = vec![b'x'; PNG.len()];
    archive(&path, &snapshot_empty().project, hash, &bad);
    let host = host(dir.path());
    let mut s = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    s.dispatch("file.open", &json!({"location":path,"discard_dirty":true})).unwrap();
    let before = s.document().clone();
    let status = s.file_status();
    let revision = s.revision();
    let journal = std::fs::read(dir.path().join("recovery")).unwrap();
    assert!(drop_image(&mut s, C, &[NodeId::from_bytes([36; 16])]).is_err());
    assert_eq!(s.document(), &before);
    assert_eq!(s.file_status(), status);
    assert_eq!(s.revision(), revision);
    assert_eq!(std::fs::read(dir.path().join("recovery")).unwrap(), journal);
    assert_eq!(s.assets().context().storage_usage().unwrap().verified, 0);
    assert!(s.assets().open_verified(&asset(hash), &TransferCancel::default(), &StreamMetrics::default()).is_err());
}
#[test]
fn bounded_operation_contention_preserves_original_error_and_reports_maintenance_refusal() {
    use std::{
        sync::{Mutex, mpsc},
        time::Duration,
    };
    struct Blocked {
        entered: Mutex<Option<mpsc::Sender<()>>>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl AssetSource for Blocked {
        fn byte_len(&self) -> u64 {
            1
        }
        fn read_at(&self, _: u64, _: &mut [u8]) -> Result<usize, Error> {
            if let Some(tx) = self.entered.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            let _ = self.release.lock().unwrap().recv_timeout(Duration::from_secs(3));
            Err(Error::new(ErrorKind::Conflict, "bounded fixture source ended"))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let host = host(dir.path());
    let mut s = Session::with_storage(snapshot_empty(), Box::new(host)).unwrap();
    let hash = s.import_asset(b"unrelated A").unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let range = AssetRange::new(Arc::new(Blocked { entered: Mutex::new(Some(entered_tx)), release: Mutex::new(release_rx) }), 0, 1).unwrap();
    let mut clone = s.assets().clone();
    let worker = std::thread::spawn(move || clone.import_stream(&range, &TransferCancel::default(), &StreamMetrics::default()));
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let before = s.document().clone();
    let error = drop_image(&mut s, C, &[NodeId::from_bytes([37; 16])]).unwrap_err();
    release_tx.send(()).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert_eq!(error.kind, ErrorKind::Busy);
    assert!(error.message.ends_with("Asset maintenance also failed; retry maintenance before importing again."));
    assert_eq!(s.document(), &before);
    assert_eq!(s.assets().len(), 1);
    assert!(s.assets().contains(&hash));
    assert!(!s.can_undo());
    s.assets().collect_garbage().unwrap();
}
#[test]
fn native_edge_order_roundtrip_does_not_verify_untouched_indexed_body() {
    use nodecraft_graph::{Edge, Endpoint, Port};
    use nodecraft_types::{EdgeId, Modality, PortType};
    let mut project = snapshot_empty().project;
    let hub = NodeId::from_bytes([40; 16]);
    let mut node = Node::new(hub, "test.many");
    node.inputs.push(Port { name: "in".into(), port_type: PortType { modality: Modality::Text, many: true } });
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    for n in 1..=3 {
        let id = NodeId::from_bytes([n + 40; 16]);
        let mut node = Node::new(id, "test.source");
        node.outputs.push(Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } });
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
        let edge = Edge {
            id: EdgeId::from_bytes([n; 16]),
            from: Endpoint { node: id, port: "out".into() },
            to: Endpoint { node: hub, port: "in".into() },
            order: u32::from(n) - 1,
        };
        Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("indexed.ncraft");
    let hash = nodecraft_types::Blake3::from_bytes(*blake3::hash(b"good").as_bytes());
    archive(&path, &project, hash, b"evil");
    let host = host(dir.path());
    let mut s = Session::with_storage(snapshot_empty(), Box::new(host.clone())).unwrap();
    s.dispatch("file.open", &json!({"location":path,"discard_dirty":true})).unwrap();
    assert_eq!(s.assets().context().storage_usage().unwrap().verified, 0);
    let params = json!({"canvas":C,"to":{"node":hub,"port":"in"},"edges":[EdgeId::from_bytes([3;16]),EdgeId::from_bytes([1;16]),EdgeId::from_bytes([2;16])]});
    s.enabled("edge.reorder", &params).unwrap();
    s.dispatch("edge.reorder", &params).unwrap();
    let reordered = s.document().clone();
    assert_ne!(reordered, project);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &project);
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(s.document(), &reordered);
    assert_eq!(host.0.borrow().native.recover().unwrap().snapshot.project, reordered);
    assert_eq!(s.assets().context().storage_usage().unwrap().verified, 0, "SetEdge/replay never needs the untouched corrupt body");
}

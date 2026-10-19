//! Clipboard authority through real native storage, including deferred corrupt content.
use super::*;
use nodecraft_format::{
    AssetStore, NativeStorage, Snapshot,
    asset_source::{AssetRange, MemorySource, StreamMetrics, TransferCancel, copy_and_hash},
};
use nodecraft_types::{AssetRef, Blake3, MediaMeta};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
const C: CanvasId = CanvasId::from_bytes([51; 16]);
const N: NodeId = NodeId::from_bytes([52; 16]);
struct OwnedTemp(PathBuf);
impl OwnedTemp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..16 {
            let path = std::env::temp_dir().join(format!("nodecraft-clipboard273-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create clipboard fixture: {e}"),
            }
        }
        panic!("clipboard fixture names exhausted");
    }
}
impl Drop for OwnedTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn project() -> Project {
    let mut p = Project::new(ProjectId::from_bytes([53; 16]), "Public clipboard asset fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    p
}
fn native(root: &Path) -> Session {
    Session::with_storage(
        Snapshot { project: project(), assets: AssetStore::default(), epoch: 0 },
        Box::new(NativeStorage::new(root.join("recovery")).unwrap()),
    )
    .unwrap()
}
fn media_node(asset: AssetRef) -> Node {
    let mut n = Node::new(N, "fixture.unknown");
    n.config.insert("nested".into(), Value::List(vec![Value::List(vec![Value::Media(asset)])]));
    n
}
fn app(s: Session) -> Harness<'static, NodeCraftApp> {
    Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(s, C).unwrap())
}
fn payload(n: Node) -> clipboard::Payload {
    clipboard::Payload { format: clipboard::FORMAT.into(), version: 1, project: Some(ProjectId::from_bytes([99; 16])), nodes: vec![n], edges: vec![] }
}
#[test]
fn clipboard273_native_ui_above_small_cap_cuts_pastes_and_undoes() {
    let root = OwnedTemp::new();
    let mut s = native(&root.0);
    let hash = s.import_asset_reader(&mut std::io::repeat(0xff).take(17 * 1024 * 1024), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert!(s.assets().get(&hash).is_none());
    assert_eq!(s.assets().descriptor(&hash).unwrap().bytes, 17 * 1024 * 1024);
    let asset = AssetRef { hash, mime: "application/octet-stream".parse().unwrap(), meta: Default::default() };
    let mut h = app(s);
    h.event(egui::Event::Paste(clipboard::to_text(&payload(media_node(asset))).unwrap()));
    h.run();
    assert_eq!(counts(&h).0, 1);
    let id = *h.state().selection.nodes().iter().next().unwrap();
    let before = h.state().session.document().clone();
    h.event(egui::Event::Cut);
    h.step();
    let text = copied(&h).expect("native Cut publishes its admitted payload");
    assert_eq!(counts(&h).0, 0);
    assert!(h.state().session.assets().get(&hash).is_none());
    h.event(egui::Event::Paste(text));
    h.run();
    assert_eq!(counts(&h).0, 1);
    for _ in 0..2 {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run();
    }
    assert_eq!(h.state().session.document(), &before);
    assert!(h.state().session.document().canvases().get(&C).unwrap().nodes().contains_key(&id));
}
#[test]
fn clipboard273_native_ui_paste_refuses_missing_mime_and_dimensions_atomically() {
    for fault in 0..3 {
        let root = OwnedTemp::new();
        let mut s = native(&root.0);
        let png = nodecraft_media::encode_png(1, 1, &[30, 60, 90, 255], &Default::default()).unwrap();
        let hash = s.import_asset(&png).unwrap();
        assert!(s.assets().get(&hash).is_none());
        let mut asset = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta { width: Some(1), height: Some(1), ..Default::default() } };
        match fault {
            0 => asset.hash = Blake3::from_bytes([98; 32]),
            1 => asset.mime = "text/plain".parse().unwrap(),
            _ => asset.meta.width = Some(2),
        }
        let mut h = app(s);
        let before = h.state().session.document().clone();
        let rev = h.state().session.revision();
        let bytes = std::fs::read(root.0.join("recovery")).unwrap();
        h.event(egui::Event::Paste(clipboard::to_text(&payload(media_node(asset))).unwrap()));
        h.step();
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), rev);
        assert!(h.state().session.events_since(0).unwrap().is_empty());
        assert!(!h.state().session.can_undo() && !h.state().session.can_redo());
        assert!(copied(&h).is_none());
        assert_eq!(std::fs::read(root.0.join("recovery")).unwrap(), bytes);
    }
}

// This finite fixture rewrites ONE known stored-ZIP body and its two CRC fields. It is
// not a production archive parser or an independent hostile-ZIP validator.
fn change_fixture(bytes: &mut [u8], fault: usize) {
    fn u16_at(b: &[u8], at: usize) -> usize {
        u16::from_le_bytes(b[at..at + 2].try_into().unwrap()) as usize
    }
    fn u32_at(b: &[u8], at: usize) -> usize {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
    }
    let mut at = 0usize;
    let mut target = None;
    while bytes.get(at..at + 4) == Some(b"PK\x03\x04") {
        let size = u32_at(bytes, at + 18);
        let name = u16_at(bytes, at + 26);
        let extra = u16_at(bytes, at + 28);
        let name_start = at + 30;
        let start = name_start + name + extra;
        let end = start + size;
        if &bytes[name_start..name_start + name] == b"project.json" {
            assert!(std::str::from_utf8(&bytes[start..end]).unwrap().contains("\"version\":4"));
        }
        let selected = if fault == 2 {
            bytes[name_start..name_start + name].starts_with(b"assets/") && size < 1024
        } else {
            &bytes[name_start..name_start + name] == b"project.json"
        };
        if selected {
            assert!(target.is_none());
            target = Some((at, start, end));
        }
        at = end;
    }
    let central = at;
    let (header, start, end) = target.unwrap();
    if fault == 2 {
        bytes[start] ^= 0x40;
        return;
    }
    let text = std::str::from_utf8(&bytes[start..end]).unwrap();
    let changed = if fault == 0 { text.replacen("image/png", "image/gif", 1) } else { text.replacen("\"width\":1", "\"width\":2", 1) };
    assert_ne!(text, changed);
    assert_eq!(text.len(), changed.len());
    bytes[start..end].copy_from_slice(changed.as_bytes());
    let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(changed.as_bytes()))), 0, changed.len() as u64).unwrap();
    let crc = copy_and_hash(&range, &mut std::io::sink(), &TransferCancel::default(), &StreamMetrics::default()).unwrap().crc32.to_le_bytes();
    bytes[header + 14..header + 18].copy_from_slice(&crc);
    let mut at = central;
    let mut found = false;
    while bytes.get(at..at + 4) == Some(b"PK\x01\x02") {
        let name = u16_at(bytes, at + 28);
        let extra = u16_at(bytes, at + 30);
        let comment = u16_at(bytes, at + 32);
        if &bytes[at + 46..at + 46 + name] == b"project.json" {
            bytes[at + 16..at + 20].copy_from_slice(&crc);
            found = true;
        }
        at += 46 + name + extra + comment;
    }
    assert!(found);
}
#[test]
fn clipboard273_native_open_bad_references_refuse_copy_cut_and_duplicate() {
    for fault in 0..3 {
        for action in 0..3 {
            let root = OwnedTemp::new();
            let path = root.0.join("public.ncraft");
            let source_root = root.0.join("source");
            std::fs::create_dir(&source_root).unwrap();
            let hash = {
                let mut source = native(&source_root);
                let png = nodecraft_media::encode_png(1, 1, &[30, 60, 90, 255], &Default::default()).unwrap();
                let hash = source.import_asset(&png).unwrap();
                let asset = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta { width: Some(1), height: Some(1), ..Default::default() } };
                let mut node = media_node(asset);
                // Small native saves intentionally choose classic-v3 compatibility. A real
                // referenced >16MiB anchor selects native-v4 without spoofing a version.
                let anchor = source
                    .import_asset_reader(&mut std::io::repeat(0xff).take(17 * 1024 * 1024), &TransferCancel::default(), &StreamMetrics::default())
                    .unwrap();
                node.config.insert(
                    "zzz_anchor".into(),
                    Value::Media(AssetRef { hash: anchor, mime: "application/octet-stream".parse().unwrap(), meta: Default::default() }),
                );
                source.dispatch("canvas.paste", &json!({"canvas":C,"nodes":[node]})).unwrap();
                source.dispatch("file.save_as", &json!({"location":path.to_str().unwrap()})).unwrap();
                source.close_recovery(false).unwrap();
                hash
            };
            // Use the actual current native writer. Classic-v3 migration validates bodies
            // before publication; only the current native family defers their content proof.
            // The source host and its destination guard are gone before hostile mutation.
            // Explicit bounded hostile-fixture memory only; product admission never
            // materializes this archive or either native asset.
            assert!(std::fs::metadata(&path).unwrap().len() < 20 * 1024 * 1024);
            let mut archive = std::fs::read(&path).unwrap();
            change_fixture(&mut archive, fault);
            std::fs::write(&path, archive).unwrap();

            let mut s = native(&root.0);
            s.dispatch("file.open", &json!({"location":path.to_str().unwrap(),"discard_dirty":true})).unwrap();
            assert!(s.assets().descriptor(&hash).is_some() && s.assets().get(&hash).is_none());
            let mut h = app(s);
            h.state_mut().selection.only(N);
            let before = h.state().session.document().clone();
            let rev = h.state().session.revision();
            let events = h.state().session.events_since(0).unwrap();
            let selection = h.state().selection.clone();
            let bytes = std::fs::read(root.0.join("recovery")).unwrap();
            match action {
                0 => h.event(egui::Event::Copy),
                1 => h.event(egui::Event::Cut),
                _ => h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::D),
            }
            h.step();
            assert!(copied(&h).is_none());
            assert_eq!(h.state().session.document(), &before);
            assert_eq!(h.state().session.revision(), rev);
            assert_eq!(h.state().session.events_since(0).unwrap(), events);
            assert_eq!(h.state().selection, selection);
            assert!(!h.state().session.can_undo() && !h.state().session.can_redo());
            assert_eq!(std::fs::read(root.0.join("recovery")).unwrap(), bytes);
        }
    }
}

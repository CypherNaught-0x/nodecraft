//! Images on cards (#386), decoded off the frame by the preview worker.
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};

/// Frames until `label` shows up (the worker decodes off the frame), or `None`.
fn wait_for(h: &mut Harness<'static, NodeCraftApp>, label: &str) -> Option<()> {
    for _ in 0..200 {
        h.run();
        if h.query_by_label(label).is_some() {
            return Some(());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    None
}

#[test]
fn an_image_card_shows_its_picture_once_decoded() {
    let mut h = harness();
    let png = nodecraft_media::encode_png(8, 6, &[90; 8 * 6 * 4], &Default::default()).unwrap();
    let canvas = h.state().canvas;
    let at = nodecraft_graph::Position { x: 200.0, y: -110.0 };
    let id = NodeId::from_bytes([40; 16]);
    import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "pic.png", bytes: &png }], &[id]).unwrap();
    assert!(wait_for(&mut h, "Image 8×6").is_some(), "the thumbnail appears");
    assert!(h.state().previews.bytes() > 0);
    // The text card shows no image.
    assert_eq!(h.query_all_by_label("Image 8×6").count(), 1);
}

#[test]
fn a_corrupt_image_says_so_without_panicking() {
    let mut h = harness();
    let canvas = h.state().canvas;
    // Bytes that sniff as PNG but don't decode, stored as an Image node's asset.
    let bytes = b"\x89PNG\r\n\x1a\nnot really a png".to_vec();
    let hash = h.state_mut().session.import_asset(&bytes).unwrap();
    let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let supplied: nodecraft_types::ParamValues = [("asset".to_owned(), Value::Media(asset))].into_iter().collect();
    let mut node = kinds.get("source.image").unwrap().instantiate_instance(NodeId::from_bytes([41; 16]), Default::default(), &supplied).unwrap();
    node.pos = nodecraft_graph::Position { x: 200.0, y: -110.0 };
    h.state_mut().session.dispatch("node.add", &json!({"canvas": canvas, "node": node})).unwrap();
    assert!(wait_for(&mut h, "Can't preview this image").is_some());
}

#[cfg(not(target_arch = "wasm32"))]
fn native_image_fixture(open: bool) {
    use nodecraft_format::{AssetStore, NativeStorage, Snapshot};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!("nodecraft-preview348-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir(&root).unwrap();
    struct OwnedTemp(std::path::PathBuf);
    impl Drop for OwnedTemp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _owned = OwnedTemp(root.clone());
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Native preview").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let storage = NativeStorage::new(root.join("recovery")).unwrap();
    let mut session = Session::with_storage(Snapshot { project: project.clone(), assets: AssetStore::default(), epoch: 0 }, Box::new(storage)).unwrap();
    let png = nodecraft_media::encode_png(8, 6, &[90; 8 * 6 * 4], &Default::default()).unwrap();
    let id = NodeId::from_bytes([42; 16]);
    let import = |s: &mut Session| {
        let result =
            import_files(s, canvas, nodecraft_graph::Position { x: 200.0, y: -110.0 }, 300.0, &[Incoming { name: "native.png", bytes: &png }], &[id]).unwrap();
        assert_eq!(result.nodes, vec![id]);
    };
    let external = root.join("picture.ncraft");
    if open {
        let mut resident = Session::new(project).unwrap();
        import(&mut resident);
        let bytes = nodecraft_format::encode(&Snapshot { project: resident.document().clone(), assets: resident.assets().clone(), epoch: 0 }).unwrap();
        std::fs::write(&external, bytes).unwrap();
        let outcome = session.dispatch("file.open", &json!({"location":external.to_str().unwrap(),"discard_dirty":true})).unwrap();
        assert_eq!(outcome["opened"], true);
    } else {
        import(&mut session);
    }
    let asset = nodecraft_engine::split::image_of(session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap()).unwrap();
    assert!(session.assets().descriptor(&asset.hash).is_some());
    assert!(session.assets().get(&asset.hash).is_none(), "fixture must exercise real native nonresident content");
    if open {
        std::fs::remove_file(external).unwrap();
    }
    let app = NodeCraftApp::from_session(session, canvas).unwrap();
    let mut h = Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    assert!(wait_for(&mut h, "Image 8×6").is_some(), "native image must reach Ready rather than being reported missing");
    assert!(h.state().previews.bytes() > 0);
    assert!(h.query_by_label("This image's data is missing").is_none());
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_open_image_previews_after_external_archive_is_deleted() {
    native_image_fixture(true);
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_import_image_previews_without_resident_materialization() {
    native_image_fixture(false);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_preview_busy_recovers_on_an_external_frame_without_a_busy_wake() {
    use nodecraft_format::{
        AssetStore, NativeStorage, Snapshot,
        asset_source::{AssetRange, AssetSource},
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::sync_channel,
    };
    let root = std::env::temp_dir().join(format!("nodecraft-preview348-busy-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    struct OwnedTemp(std::path::PathBuf);
    impl Drop for OwnedTemp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _owned = OwnedTemp(root.clone());
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Busy preview").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let storage = NativeStorage::new(root.join("recovery")).unwrap();
    let mut session = Session::with_storage(Snapshot { project, assets: AssetStore::default(), epoch: 0 }, Box::new(storage)).unwrap();
    let png = nodecraft_media::encode_png(8, 6, &[90; 8 * 6 * 4], &Default::default()).unwrap();
    let id = NodeId::from_bytes([43; 16]);
    let imported =
        import_files(&mut session, canvas, nodecraft_graph::Position { x: 200.0, y: -110.0 }, 300.0, &[Incoming { name: "busy.png", bytes: &png }], &[id])
            .unwrap();
    assert_eq!(imported.nodes, vec![id]);
    assert!(imported.skipped.is_empty());
    let asset = nodecraft_engine::split::image_of(session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap()).unwrap().clone();
    let mut import_store = session.assets().clone();
    let preview_store = session.assets().clone();
    let (ready, entered) = sync_channel(1);
    let (go, wait) = sync_channel(1);
    struct Block {
        ready: std::sync::mpsc::SyncSender<()>,
        wait: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl AssetSource for Block {
        fn byte_len(&self) -> u64 {
            1
        }
        fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
            if at != 0 || out.is_empty() {
                return Ok(0);
            }
            self.ready.send(()).unwrap();
            self.wait.lock().unwrap().recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            out[0] = b'x';
            Ok(1)
        }
    }
    let source = AssetRange::new(Arc::new(Block { ready, wait: Mutex::new(wait) }), 0, 1).unwrap();
    let handle = std::thread::spawn(move || import_store.import_stream(&source, &Default::default(), &Default::default()));
    struct ImportOwner {
        go: std::sync::mpsc::SyncSender<()>,
        handle: Option<std::thread::JoinHandle<Result<nodecraft_types::Blake3, Error>>>,
    }
    impl Drop for ImportOwner {
        fn drop(&mut self) {
            let _ = self.go.try_send(());
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }
    let mut owner = ImportOwner { go, handle: Some(handle) };
    entered.recv_timeout(std::time::Duration::from_secs(5)).unwrap(); // real import owns context.operation
    let app = NodeCraftApp::from_session(session, canvas).unwrap();
    let mut h = Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    let wakes = Arc::new(AtomicUsize::new(0));
    let worker_wakes = wakes.clone();
    h.ctx.set_request_repaint_callback(move |_| {
        if std::thread::current().name() == Some("nodecraft-previews") {
            worker_wakes.fetch_add(1, Ordering::SeqCst);
        }
    });
    assert!(wait_for(&mut h, "Preview busy; try again").is_some());
    assert_eq!(h.state().previews.test_pending_and_failed(), (0, 0));
    let mut acquisitions = 0;
    let ctx = h.ctx.clone();
    let _ = h.state_mut().previews.request_asset(&ctx, asset.hash, 320.0, || {
        acquisitions += 1;
        preview_store.preview_read(&asset)
    });
    assert_eq!(acquisitions, 0, "same-frame Busy must suppress reacquisition");
    for _ in 0..3 {
        assert!(wait_for(&mut h, "Preview busy; try again").is_some());
    }
    assert_eq!(wakes.load(Ordering::SeqCst), 0, "Busy never schedules its own next frame");
    owner.go.send(()).unwrap();
    assert!(owner.handle.take().unwrap().join().unwrap().is_ok());
    assert!(wait_for(&mut h, "Image 8×6").is_some(), "external frame recovers without resetting cache");
    assert!(h.state().previews.bytes() > 0);
}

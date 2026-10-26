use super::super::tests::harness;
use super::*;
use egui_kittest::Harness;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn nodes(h: &Harness<'static, NodeCraftApp>) -> Vec<nodecraft_graph::Node> {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().cloned().collect()
}
/// A clipboard that answers `picture` and counts its reads.
fn clipboard(h: &mut Harness<'static, NodeCraftApp>, picture: impl Fn() -> Result<Option<Picture>, String> + Send + Sync + 'static) -> Arc<AtomicUsize> {
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    h.state_mut().image_paste.reader = Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        picture().map(|picture| picture.map(Contents::Picture))
    });
    reads
}
/// What a test clipboard answers.
type Answer = fn() -> Result<Option<Picture>, String>;
fn grey(width: usize, height: usize) -> Picture {
    Picture { width, height, rgba: vec![90; width * height * 4] }
}
/// ⌘V as egui-winit reports it for a clipboard without text: only the release.
fn release_paste(h: &mut Harness<'static, NodeCraftApp>, at: Pos2) {
    h.hover_at(at);
    h.run();
    h.input_mut().events.push(egui::Event::Key { key: egui::Key::V, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::COMMAND });
    h.step();
}
fn finish(h: &mut Harness<'static, NodeCraftApp>) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while h.state().image_paste_pending() {
        h.step();
        assert!(std::time::Instant::now() < end, "the clipboard read finishes");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    h.run();
}

#[test]
fn a_copied_image_becomes_an_image_node_at_the_pointer_as_one_undo_step() {
    let mut h = harness();
    let reads = clipboard(&mut h, || Ok(Some(grey(6, 4))));
    let before = h.state().session.document().clone();
    let at = Pos2::new(640.0, 260.0);
    release_paste(&mut h, at);
    finish(&mut h);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let existing = before.canvases().get(&h.state().canvas).unwrap().nodes().clone();
    let added: Vec<_> = nodes(&h).into_iter().filter(|n| !existing.contains_key(&n.id)).collect();
    assert_eq!(added.len(), 1, "{:?}", h.state().notice);
    assert_eq!(added[0].kind, "source.image");
    let world = h.state().view.to_world(h.state().viewport.min, at);
    let t = *h.state().theme.tokens();
    let rect = super::super::payload_rect(&h.state().session, &added[0], &t);
    assert!((rect.center() - world).length() < 1.0, "actual image body centered under pointer");
    assert_eq!(h.state().selection.nodes(), &added.iter().map(|n| n.id).collect());
    assert!(h.state().notice.is_none());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before, "one undo removes the pasted image");
}

#[test]
fn a_text_paste_never_reads_the_image_clipboard() {
    let mut h = harness();
    let reads = clipboard(&mut h, || Ok(Some(grey(2, 2))));
    let count = nodes(&h).len();
    h.hover_at(Pos2::new(600.0, 300.0));
    h.run();
    // egui-winit reports the press as a text paste, then the key's release.
    h.input_mut().events.push(egui::Event::Paste("Pasted words".into()));
    h.step();
    release_paste(&mut h, Pos2::new(600.0, 300.0));
    finish(&mut h);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let added = nodes(&h);
    assert_eq!(added.len(), count + 1);
    assert!(added.iter().all(|n| n.kind != "source.image"), "the text became a text node, not an image");
    // Both events in one frame read as the same text paste.
    h.input_mut().events.push(egui::Event::Paste("More words".into()));
    h.input_mut().events.push(egui::Event::Key { key: egui::Key::V, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::COMMAND });
    h.step();
    finish(&mut h);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

#[test]
fn command_let_go_just_before_v_still_pastes_but_a_typed_v_never_does() {
    let mut h = harness();
    let reads = clipboard(&mut h, || Ok(Some(grey(3, 3))));
    let at = Pos2::new(600.0, 300.0);
    h.hover_at(at);
    h.run();
    let v = |pressed| egui::Event::Key { key: egui::Key::V, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
    // A typed v: its press is reported, so its release is no paste, even with ⌘ just held.
    h.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::COMMAND));
    h.step();
    h.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.input_mut().events.push(v(true));
    h.step();
    h.input_mut().events.push(v(false));
    h.step();
    finish(&mut h);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let count = nodes(&h).len();
    // ⌘V whose press egui swallowed, with ⌘ let go first: the bare release pastes.
    h.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::COMMAND));
    h.step();
    h.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.step();
    h.input_mut().events.push(v(false));
    h.step();
    finish(&mut h);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(nodes(&h).len(), count + 1, "{:?}", h.state().notice);
}

#[test]
fn an_empty_unreadable_or_oversized_clipboard_adds_nothing_and_says_why() {
    let mut h = harness();
    let count = nodes(&h).len();
    let cases: [(Answer, &str); 5] = [
        (|| Ok(None), "The clipboard holds no image or text to paste."),
        (|| Err("The copied image can't be read; nothing was pasted.".into()), "The copied image can't be read; nothing was pasted."),
        (|| Ok(Some(Picture { width: 16_385, height: 1, rgba: Vec::new() })), "Copied images are at most 16384×16384 pixels; nothing was pasted."),
        (|| Ok(Some(Picture { width: 16_384, height: 4097, rgba: Vec::new() })), "Copied images are at most 67 megapixels; nothing was pasted."),
        (|| Ok(Some(Picture { width: 4, height: 4, rgba: vec![0; 3] })), "The copied image can't be read; nothing was pasted."),
    ];
    for (picture, notice) in cases {
        clipboard(&mut h, picture);
        release_paste(&mut h, Pos2::new(600.0, 300.0));
        finish(&mut h);
        assert_eq!(nodes(&h).len(), count);
        assert_eq!(h.state().notice.as_deref(), Some(notice));
        assert!(!h.state().session.can_undo());
    }
}

#[test]
fn an_edit_while_the_image_is_read_wins_and_a_second_paste_waits() {
    let mut h = harness();
    let gate = Arc::new(Mutex::new(()));
    let held = gate.lock().unwrap();
    let waiting = Arc::clone(&gate);
    let reads = clipboard(&mut h, move || {
        drop(waiting.lock());
        Ok(Some(grey(2, 2)))
    });
    let at = Pos2::new(600.0, 300.0);
    release_paste(&mut h, at);
    assert!(h.state().image_paste_pending());
    release_paste(&mut h, at);
    assert_eq!(h.state().notice.as_deref(), Some("The copied image is still being read; try again in a moment."));
    let canvas = h.state().canvas;
    let node = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([77; 16]), "fixture.unknown");
    h.state_mut().session.dispatch("node.add", &serde_json::json!({"canvas": canvas, "node": node})).unwrap();
    let count = nodes(&h).len();
    drop(held);
    finish(&mut h);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(nodes(&h).len(), count);
    assert_eq!(h.state().notice.as_deref(), Some("The document changed while reading the clipboard; nothing was added. Paste again."));
}

/// The real OS clipboard: copy an image first, e.g. on macOS
/// `osascript -e 'set the clipboard to (read (POSIX file "/path/x.png") as «class PNGf»)'`.
#[test]
#[ignore = "reads the real clipboard; copy an image first"]
fn the_system_clipboard_reads_a_copied_image() {
    let Contents::Picture(picture) = system().unwrap().expect("the clipboard holds an image") else { panic!("the clipboard holds files") };
    assert_eq!(picture.rgba.len(), picture.width * picture.height * 4);
    let png = encode(&picture).unwrap();
    let meta = nodecraft_media::probe_image(&png, &Default::default()).unwrap();
    println!("read {}×{} from the clipboard, {} bytes as PNG", meta.width, meta.height, png.len());
}

/// Prepare only public fixture files in the supplied directory, copy both in Finder/Explorer,
/// then run this ignored test. It refuses other paths before importing any clipboard file.
#[test]
#[ignore = "reads real file references; copy the public fixture files first"]
fn the_system_clipboard_imports_copied_file_references() {
    let root = std::env::var_os("NODECRAFT_CLIPBOARD_FIXTURE_ROOT").map(std::path::PathBuf::from).expect("set the public fixture root");
    let expected = [root.join("note.txt").canonicalize().unwrap(), root.join("picture.png").canonicalize().unwrap()];
    let Contents::Files(paths) = system().unwrap().expect("the clipboard holds file references") else { panic!("copy the public files, not image pixels") };
    assert_eq!(paths.len(), 2, "copy exactly the two public fixture files");
    assert!(paths.iter().all(|path| path.canonicalize().ok().is_some_and(|path| expected.contains(&path))), "clipboard references must be the public fixtures");
    let mut h = harness();
    // Keep this real OS receipt: a concurrent user clipboard change must never import private files.
    h.state_mut().image_paste.files_reader = Some(Arc::new(move || Ok(Some(paths.clone()))));
    let before = h.state().session.document().clone();
    h.hover_at(Pos2::new(600.0, 300.0));
    h.run();
    h.event(egui::Event::Paste("note.txt\npicture.png".into()));
    release_paste(&mut h, Pos2::new(600.0, 300.0));
    finish(&mut h);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while h.state().drop_pending() {
        h.step();
        assert!(std::time::Instant::now() < end, "copied file acquisition completes");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let existing = before.canvases().get(&h.state().canvas).unwrap().nodes();
    let added: Vec<_> = nodes(&h).into_iter().filter(|node| !existing.contains_key(&node.id)).collect();
    assert_eq!(added.len(), 2, "{:?}", h.state().notice);
    assert!(added.iter().any(|node| node.kind == "source.text"));
    assert!(added.iter().any(|node| node.kind == "source.image"));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    println!("real OS copied files: text + image imported, one undo restored the document");
}

#[test]
fn text_without_typed_file_references_keeps_its_payload_and_never_reads_pixels() {
    let mut h = harness();
    let pixels = clipboard(&mut h, || Ok(Some(grey(2, 2))));
    let reads = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&reads);
    h.state_mut().image_paste.files_reader = Some(Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }));
    let before = h.state().session.document().clone();
    h.hover_at(Pos2::new(600.0, 300.0));
    h.run();
    h.event(egui::Event::Paste("Public ordinary text".into()));
    h.step();
    finish(&mut h);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(pixels.load(Ordering::SeqCst), 0);
    assert!(nodes(&h).iter().any(|node| node.params.get("text") == Some(&nodecraft_types::Value::Text("Public ordinary text".into()))));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
#[ignore = "manual public-safe clipboard922 probe failure evidence"]
fn render_clipboard922_probe_failure() {
    let path = std::path::PathBuf::from(std::env::var_os("NODECRAFT_CLIPBOARD922_CAPTURE").unwrap());
    let mut h = harness();
    h.state_mut().image_paste.files_reader = Some(Arc::new(|| Err("The clipboard can't be opened; nothing was pasted.".into())));
    h.hover_at(Pos2::new(650.0, 180.0));
    h.run();
    h.event(egui::Event::Paste("Public text survives a clipboard probe failure.".into()));
    h.step();
    finish(&mut h);
    h.render().unwrap().save(path).unwrap();
}

#[test]
fn failed_file_probe_or_worker_start_keeps_egui_text_and_one_undo_step() {
    for fail_spawn in [false, true] {
        let mut h = harness();
        let pixels = clipboard(&mut h, || Ok(Some(grey(2, 2))));
        let probes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&probes);
        h.state_mut().image_paste.files_reader = Some(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Err("The clipboard can't be opened; nothing was pasted.".into())
        }));
        if fail_spawn {
            h.state_mut().image_paste.spawn = |_, _| Err(std::io::Error::other("fixture worker unavailable"));
        }
        let before = h.state().session.document().clone();
        let at = Pos2::new(650.0, 180.0);
        h.hover_at(at);
        h.run();
        h.event(egui::Event::Paste("Public text survives a clipboard probe failure.".into()));
        h.event(egui::Event::Key { key: egui::Key::V, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::COMMAND });
        h.step();
        finish(&mut h);
        let existing = before.canvases().get(&h.state().canvas).unwrap().nodes();
        let added: Vec<_> = nodes(&h).into_iter().filter(|n| !existing.contains_key(&n.id)).collect();
        assert_eq!(added.len(), 1, "{:?}", h.state().notice);
        assert_eq!(added[0].params.get("text"), Some(&nodecraft_types::Value::Text("Public text survives a clipboard probe failure.".into())));
        assert_eq!(probes.load(Ordering::SeqCst), usize::from(!fail_spawn));
        assert_eq!(pixels.load(Ordering::SeqCst), 0);
        assert!(h.state().notice.is_none());
        let world = h.state().view.to_world(h.state().viewport.min, at);
        let rect = super::super::payload_rect(&h.state().session, &added[0], h.state().theme.tokens());
        assert!((rect.center() - world).length() < 1.0);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run();
        assert_eq!(h.state().session.document(), &before);
    }
}

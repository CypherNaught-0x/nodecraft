use super::super::tests::harness;
use super::*;
use egui::Pos2;
use egui_kittest::{Harness, kittest::Queryable};
use std::time::{Duration, Instant};

/// A folder dropped by the test, as eframe hands it over.
#[derive(Debug)]
struct Dropped(PathBuf);
impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        panic!("a dropped folder is read by its own reader")
    }
}

/// A temp folder per test, removed when the test ends.
struct Scratch(PathBuf);
impl Scratch {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("nodecraft-folder-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn png(shade: u8) -> Vec<u8> {
    nodecraft_media::encode_png(4, 4, &[shade; 64], &Default::default()).unwrap()
}

fn shoot(test: &str) -> Scratch {
    let scratch = Scratch::new(test);
    scratch.write("shot10.png", &png(10));
    scratch.write("shot2.png", &png(20));
    scratch.write("raw/shot1.png", &png(30));
    scratch.write(".DS_Store", b"\0\0\0\x01Bud1");
    scratch.write(".hidden/shot.png", &png(40));
    scratch.write("Thumbs.db", b"thumbs");
    // Windows rejects the carriage return in this macOS icon file name.
    #[cfg(not(windows))]
    scratch.write("Icon\r", b"icon");
    scratch.write("notes.txt", b"not a picture");
    scratch
}

#[test]
fn a_folder_is_read_in_natural_order_without_hidden_entries() {
    let scratch = shoot("read");
    let (cancel, read) = (AtomicBool::new(false), AtomicUsize::new(0));
    let folder = read_folder(&scratch.0, &cancel, &read).unwrap();
    assert!(folder.name.starts_with("nodecraft-folder-"));
    let names: Vec<&str> = folder.files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["notes.txt", "raw/shot1.png", "shot2.png", "shot10.png"]);
    assert_eq!(read.load(Ordering::Relaxed), 4);
    assert!(folder.skipped.is_empty(), "{:?}", folder.skipped);
    assert_eq!(folder.files.get(2).map(|(_, bytes)| bytes.clone()), Some(png(20)));

    cancel.store(true, Ordering::Release);
    assert!(read_folder(&scratch.0, &cancel, &read).unwrap_err().contains("cancelled"));
    let missing = scratch.0.join("missing");
    assert!(read_folder(&missing, &AtomicBool::new(false), &read).is_err());
}

#[cfg(unix)]
#[test]
fn links_to_folders_and_special_files_are_left_out_with_reasons() {
    let scratch = shoot("links");
    std::os::unix::fs::symlink(&scratch.0, scratch.0.join("loop")).unwrap();
    // A link to a file outside the folder could bring in a secret: it isn't followed either.
    let outside = Scratch::new("links-outside");
    outside.write("credentials", b"secret");
    std::os::unix::fs::symlink(outside.0.join("credentials"), scratch.0.join("notes2.txt")).unwrap();
    let status = std::process::Command::new("mkfifo").arg(scratch.0.join("pipe")).status();
    let folder = read_folder(&scratch.0, &AtomicBool::new(false), &AtomicUsize::new(0)).unwrap();
    assert_eq!(folder.files.len(), 4, "a link back to the folder isn't followed");
    assert!(folder.skipped.iter().any(|(name, why)| name == "loop" && why.contains("links aren't followed")), "{:?}", folder.skipped);
    assert!(folder.skipped.iter().any(|(name, _)| name == "notes2.txt"), "{:?}", folder.skipped);
    assert!(folder.files.iter().all(|(_, bytes)| bytes != b"secret"));
    if status.is_ok_and(|s| s.success()) {
        assert!(folder.skipped.iter().any(|(name, _)| name == "pipe"), "{:?}", folder.skipped);
    }
}

fn finish(h: &mut Harness<'static, NodeCraftApp>) {
    let end = Instant::now() + Duration::from_secs(20);
    h.step();
    while h.state().folder_import_busy() {
        h.step();
        assert!(Instant::now() < end, "bounded folder import");
        std::thread::sleep(Duration::from_millis(2));
    }
    h.step();
}

fn folders(h: &Harness<'static, NodeCraftApp>) -> Vec<nodecraft_graph::Node> {
    let app = h.state();
    app.session.document().canvases().get(&app.canvas).unwrap().nodes().values().filter(|n| n.kind == folder::ID).cloned().collect()
}

fn drop_at(h: &mut Harness<'static, NodeCraftApp>, at: Pos2, path: &Path) {
    h.hover_at(at);
    h.run();
    h.input_mut().dropped_files.push(Arc::new(Dropped(path.to_path_buf())));
    h.step();
    finish(h);
}

#[test]
fn a_dropped_folder_becomes_one_card_and_dropping_on_it_refills_it() {
    let mut h = harness();
    let scratch = shoot("drop");
    drop_at(&mut h, Pos2::new(600.0, 300.0), &scratch.0);
    let added = folders(&h);
    let [card] = added.as_slice() else { panic!("one Folder card: {:?}", h.state().notice) };
    assert_eq!(folder::stored(card).unwrap().len(), 3, "the images; the text is left out");
    assert!(h.state().notice.as_deref().is_some_and(|n| n.contains("Added 3 files") && n.contains("notes.txt")), "{:?}", h.state().notice);
    assert!(h.state().selection.contains(card.id));
    // The card says what it holds and shows its images.
    h.run();
    h.get_by_label_contains("3 images from");
    let undo = h.state().session.document().clone();

    // Dropped on the card, another folder refills it.
    let other = Scratch::new("drop-again");
    other.write("only.png", &png(99));
    let rect = h.state().painted.get(&card.id).copied().unwrap();
    let at = h.state().view.rect_to_screen(h.state().viewport.min, rect).center();
    drop_at(&mut h, at, &other.0);
    let refilled = folders(&h);
    assert_eq!(refilled.len(), 1);
    assert_eq!(refilled.first().map(|n| n.id), Some(card.id));
    assert_eq!(folder::stored(refilled.first().unwrap()).unwrap().len(), 1);
    h.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(h.state().session.document(), &undo);
}

#[test]
fn an_empty_folder_card_imports_from_its_button() {
    let mut h = harness();
    let canvas = h.state().canvas;
    let id = NodeId::from_bytes([42; 16]);
    h.state_mut().session.dispatch("node.create", &serde_json::json!({"canvas": canvas, "id": id, "kind": folder::ID, "pos": {"x": 150, "y": -300}})).unwrap();
    h.run();
    h.get_by_label("Drop a folder here, or import one.");
    let scratch = Scratch::new("button");
    scratch.write("a.txt", b"first prompt");
    scratch.write("b.txt", b"second prompt");
    h.state_mut().folder_import.pick = Some(scratch.0.clone());
    // A real pointer is over the button for a frame before it presses it.
    let pos = h.get_by_label("Import folder…").rect().center();
    h.hover_at(pos);
    h.run();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    finish(&mut h);
    let card = folders(&h).into_iter().next().unwrap();
    assert_eq!(card.id, id);
    assert_eq!(folder::values(&card, folder::ALL_NAMES).unwrap().len(), 2, "{:?}", h.state().notice);
    assert_eq!(folder::modality(&card).unwrap(), nodecraft_types::Modality::Text);
}

#[test]
fn reading_can_be_cancelled_and_nothing_is_added() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    let scratch = shoot("cancel");
    // Cancelled before the reader reports: the result is dropped unread.
    h.state_mut().read_folder_for(scratch.0.clone(), Target::New(Position::default()));
    h.state_mut().folder_import.reading = None;
    finish(&mut h);
    assert_eq!(h.state().session.document(), &before);
}

#[test]
#[ignore = "manual public-safe capture; FOLDER_RENDER_DIR"]
fn folder_card_visual() {
    let dir = PathBuf::from(std::env::var_os("FOLDER_RENDER_DIR").unwrap());
    let mut h = Harness::builder()
        .wgpu()
        .with_size(egui::Vec2::new(1000.0, 760.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
    let scratch = Scratch::new("visual");
    for n in 0..9u8 {
        let pixels: Vec<u8> = (0..64 * 64).flat_map(|p| [n.wrapping_mul(28), (p % 64) as u8 * 4, 255 - n.wrapping_mul(20), 255]).collect();
        scratch.write(&format!("frame{}.png", n + 1), &nodecraft_media::encode_png(64, 64, &pixels, &Default::default()).unwrap());
    }
    drop_at(&mut h, Pos2::new(560.0, 220.0), &scratch.0);
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    h.render().unwrap().save(dir.join("folder-card.png")).unwrap();
}

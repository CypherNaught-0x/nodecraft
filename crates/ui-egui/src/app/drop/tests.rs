use super::super::tests::harness;
use super::*;
use egui_kittest::Harness;
use std::{path::PathBuf, sync::Arc};

/// A file dropped by the test, read from a real path as eframe's would be.
#[derive(Debug)]
struct Dropped(PathBuf);
impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    /// The shell reads through its own bounded reader, never this unbounded one.
    fn bytes(&self) -> Result<Vec<u8>, String> {
        panic!("a dropped file must not be read whole")
    }
}

/// A temp folder per test, removed when the test ends.
struct Scratch(PathBuf);
impl Scratch {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("nodecraft-drop-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn drop_at(h: &mut Harness<'static, NodeCraftApp>, at: Pos2, paths: &[PathBuf]) {
    h.hover_at(at);
    h.run();
    for path in paths {
        h.input_mut().dropped_files.push(Arc::new(Dropped(path.clone())));
    }
    h.run();
}

fn nodes(h: &Harness<'static, NodeCraftApp>) -> Vec<nodecraft_graph::Node> {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().cloned().collect()
}

#[test]
fn dropped_files_become_nodes_at_the_pointer_as_one_undo_step() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    let png = nodecraft_media::encode_png(4, 4, &[90; 64], &Default::default()).unwrap();
    let scratch = Scratch::new("nodes");
    let paths = [scratch.write("note.txt", b"Dropped words"), scratch.write("pic.png", &png)];
    let at = Pos2::new(700.0, 150.0);
    drop_at(&mut h, at, &paths);
    let existing = before.canvases().get(&h.state().canvas).unwrap().nodes().clone();
    let added: Vec<_> = nodes(&h).into_iter().filter(|n| !existing.contains_key(&n.id)).collect();
    assert_eq!(added.len(), 2, "{:?}", h.state().notice);
    let text = added.iter().find(|n| n.kind == "source.text").unwrap();
    assert_eq!(text.params.get("text"), Some(&nodecraft_types::Value::Text("Dropped words".into())));
    assert!(added.iter().any(|n| n.kind == "source.image"));
    let world = h.state().view.to_world(h.state().viewport.min, at);
    let t = *h.state().theme.tokens();
    assert!((text.pos.x + t.card_width * 0.5 - world.x).abs() < 1.0, "centred under the pointer");
    assert_eq!(h.state().selection.nodes(), &added.iter().map(|n| n.id).collect());
    assert!(h.state().notice.is_none());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before, "one undo removes the whole drop");
}

#[test]
fn unsupported_and_oversized_files_are_skipped_with_a_notice() {
    let mut h = harness();
    let count = nodes(&h).len();
    let scratch = Scratch::new("skips");
    let big = scratch.write("big.bin", &vec![0u8; MAX_DROP_BYTES + 1]);
    let pdf = scratch.write("paper.pdf", b"%PDF-1.7\n");
    drop_at(&mut h, Pos2::new(600.0, 300.0), &[big, pdf]);
    assert_eq!(nodes(&h).len(), count);
    assert_eq!(h.state().notice.as_deref(), Some("big.bin wasn't added: files over 16 MiB can't be added (and 1 more files weren't)."));
    assert!(!h.state().session.can_undo());
    let missing = std::env::temp_dir().join("nodecraft-drop-does-not-exist.txt");
    drop_at(&mut h, Pos2::new(600.0, 300.0), &[missing]);
    assert_eq!(h.state().notice.as_deref(), Some("nodecraft-drop-does-not-exist.txt wasn't added: the file can't be read."));
    drop_at(&mut h, Pos2::new(600.0, 300.0), std::slice::from_ref(&scratch.0));
    assert_eq!(h.state().notice.as_deref().map(|n| n.ends_with("only files can be added, not folders or devices.")), Some(true));
}

#[test]
fn a_drop_reads_at_most_its_total_and_its_file_count() {
    let mut h = harness();
    let scratch = Scratch::new("totals");
    // Four text-free files of almost 16 MiB: the fourth would pass the drop's 48 MiB and isn't read.
    let big: Vec<PathBuf> = (0..4u8).map(|n| scratch.write(&format!("{n}.bin"), &vec![b'a' + n; MAX_DROP_BYTES - 1])).collect();
    drop_at(&mut h, Pos2::new(600.0, 300.0), &big);
    let notice = h.state().notice.clone().unwrap_or_default();
    assert!(notice.starts_with("3.bin wasn't added: a drop reads at most 48 MiB"), "{notice}");
    // 40 tiny text files: 32 are added, the rest are counted in the notice.
    let many: Vec<PathBuf> = (0..40).map(|n| scratch.write(&format!("t{n}.txt"), format!("note {n}").as_bytes())).collect();
    let before = nodes(&h).len();
    drop_at(&mut h, Pos2::new(300.0, 300.0), &many);
    assert_eq!(nodes(&h).len(), before + MAX_FILES);
    assert!(h.state().notice.as_deref().is_some_and(|n| n.contains("at most 32 files")), "{:?}", h.state().notice);
}

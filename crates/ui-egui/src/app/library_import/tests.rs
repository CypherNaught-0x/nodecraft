//! The Assets browser's Import files… (#96), driven through real pointer events.
use super::super::tests::harness;
use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use serde_json::json;
use std::time::{Duration, Instant};

fn png(shade: u8) -> Vec<u8> {
    nodecraft_media::encode_png(4, 4, &[shade; 64], &Default::default()).unwrap()
}

fn open_assets(h: &mut Harness<'static, NodeCraftApp>) {
    crate::app::topbar_tests::reveal(h, "Techniques");
    h.get_by_label("Techniques").click();
    h.run();
    h.get_by_label("Assets").click();
    h.run();
}

/// Frames until the import has finished reading and importing.
fn settle(h: &mut Harness<'static, NodeCraftApp>) {
    let end = Instant::now() + Duration::from_secs(10);
    while h.state().library.browsers.import.busy() {
        h.step();
        assert!(Instant::now() < end, "the import finishes");
        std::thread::sleep(Duration::from_millis(2));
    }
    h.run();
}

fn sources(h: &Harness<'static, NodeCraftApp>) -> Vec<String> {
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    let mut titles: Vec<String> = canvas.nodes().values().filter(|n| n.kind == "source.image").map(|n| n.title.clone()).collect();
    titles.sort();
    titles
}

#[test]
fn picked_files_are_imported_with_a_line_each_and_only_the_failed_ones_are_retried() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    };
    let paths = vec![
        write("beach.png", &png(10)),
        write("again.png", &png(10)),
        write("broken.png", b"\x89PNG\r\n\x1a\nnot an image"),
        write("notes.txt", b"plain notes"),
        dir.path().join("gone.png"),
    ];
    let mut h = harness();
    open_assets(&mut h);
    h.state_mut().library.browsers.import.pick = Some(paths);
    let before = h.state().session.document().clone();
    h.get_by_label("Import files…").click();
    h.run();
    settle(&mut h);
    assert_eq!(sources(&h), ["beach.png"], "one card for the two identical files: {:?}", h.state().notice);
    assert!(h.query_by_label("Import: 1 added · 1 duplicate · 2 failed · 1 couldn't be read").is_some());
    assert!(h.query_by_label("again.png: the same file as beach.png").is_some());
    assert!(h.query_by_label_contains("broken.png: the image can't be read").is_some());
    assert!(h.query_by_label_contains("notes.txt: text isn't kept in the library").is_some());
    assert!(h.query_by_label("gone.png: the file can't be read").is_some());
    let saved = serde_json::to_string(h.state().session.document()).unwrap();
    assert!(!saved.contains(&dir.path().to_string_lossy().into_owned()), "no folder reaches the document");

    // Fix two of the failed files, then retry only those that failed.
    std::fs::write(dir.path().join("broken.png"), png(30)).unwrap();
    std::fs::write(dir.path().join("gone.png"), png(50)).unwrap();
    h.get_by_label("Retry failed (3)").click();
    h.run();
    settle(&mut h);
    assert_eq!(sources(&h), ["beach.png", "broken.png", "gone.png"], "{:?}", h.state().notice);
    assert!(h.query_by_label("Retry failed (1)").is_some(), "the text file still fails");
    h.get_by_label("Dismiss").click();
    h.run();
    assert!(h.query_by_label("Retry failed (1)").is_none());
    // Each import is one undo step.
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn reading_shows_per_file_progress_and_cancel_adds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<PathBuf> = (0..3u8)
        .map(|i| {
            let path = dir.path().join(format!("shot{i}.png"));
            std::fs::write(&path, png(10 + i)).unwrap();
            path
        })
        .collect();
    let mut h = harness();
    open_assets(&mut h);
    let (release, hold) = mpsc::channel();
    h.state_mut().library.browsers.import.pick = Some(paths);
    h.state_mut().library.browsers.import.hold = Some(hold);
    let before = h.state().session.document().clone();
    h.get_by_label("Import files…").click();
    h.run();
    assert!(h.query_by_label("Reading files for the library… 0 of 3").is_some());
    assert!(h.get_by_label("Import files…").accesskit_node().is_disabled(), "one import at a time");
    release.send(()).unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while h.query_by_label("Reading files for the library… 1 of 3").is_none() {
        h.step();
        assert!(Instant::now() < end, "progress counts each file");
        std::thread::sleep(Duration::from_millis(2));
    }
    h.get_by_label("Cancel import").click();
    h.run();
    assert!(!h.state().library.browsers.import.busy());
    assert_eq!(h.state().notice.as_deref(), Some("Import cancelled; nothing was added."));
    drop(release);
    h.run();
    assert_eq!(h.state().session.document(), &before, "nothing was added");
}

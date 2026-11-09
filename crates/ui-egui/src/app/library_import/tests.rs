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
    assert_eq!(sources(&h), ["beach.png"], "one card for the two identical files: {:?}", h.state().notice());
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
    assert_eq!(sources(&h), ["beach.png", "broken.png", "gone.png"], "{:?}", h.state().notice());
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
    super::super::panel_check::menu_evidence(&mut h, "library-import-progress");
    h.get_by_label("Cancel import").click();
    h.run();
    assert!(!h.state().library.browsers.import.busy());
    assert_eq!(h.state().notice(), Some("Import cancelled; nothing was added."));
    drop(release);
    h.run();
    assert_eq!(h.state().session.document(), &before, "nothing was added");
}

/// Reads `paths` without a test hold and without cancelling.
fn read(paths: Vec<PathBuf>) -> Vec<ReadFile> {
    read_files(paths, &AtomicBool::new(false), &AtomicUsize::new(0), &|| {}).unwrap()
}

fn reason(file: &ReadFile) -> &str {
    file.bytes.as_ref().err().map_or("", String::as_str)
}

#[test]
fn the_reader_refuses_files_over_16_mib_and_stops_at_48_mib_in_all() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, len: usize| {
        let path = dir.path().join(name);
        std::fs::write(&path, vec![7u8; len]).unwrap();
        path
    };
    const MIB: usize = 1024 * 1024;
    let big = write("big.bin", 16 * MIB + 1);
    let fifteen: Vec<PathBuf> = (0..4).map(|i| write(&format!("part{i}.bin"), 15 * MIB)).collect();
    let files = read([vec![big], fifteen].concat());
    assert_eq!(reason(&files[0]), "files over 16 MiB can't be added");
    assert!(files[1..4].iter().all(|file| file.bytes.is_ok()), "45 MiB fits");
    assert_eq!(reason(&files[4]), OVER_ROOM, "the fourth 15 MiB file would pass 48 MiB");
    // A cancelled reader stops before the next file.
    let cancelled = read_files(vec![dir.path().join("part0.bin")], &AtomicBool::new(true), &AtomicUsize::new(0), &|| {});
    assert!(cancelled.is_err());
}

#[test]
#[cfg(unix)]
fn the_reader_refuses_folders_and_pipes_without_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    let pipe = dir.path().join("pipe");
    assert!(std::process::Command::new("mkfifo").arg(&pipe).status().unwrap().success());
    let files = read(vec![folder, pipe]);
    assert_eq!(reason(&files[0]), "only regular files are read, not devices or pipes");
    assert_eq!(reason(&files[1]), "only regular files are read, not devices or pipes", "a FIFO opens at once and is refused");
}

#[test]
fn retry_is_disabled_while_files_are_imported_and_keeps_the_failed_paths() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("later.png");
    let mut h = harness();
    open_assets(&mut h);
    h.state_mut().library.browsers.report = Some(super::super::library_browsers::ImportReport {
        summary: "1 failed".into(),
        lines: vec![ImportLine { name: "later.png".into(), state: "the file can't be read".into(), failed: true }],
        retry: vec![missing.clone()],
    });
    let shot = dir.path().join("shot.png");
    std::fs::write(&shot, png(70)).unwrap();
    let (release, hold) = mpsc::channel();
    h.state_mut().library.browsers.import.pick = Some(vec![shot]);
    h.state_mut().library.browsers.import.hold = Some(hold);
    h.get_by_label("Import files…").click();
    h.run();
    assert!(h.state().library.browsers.import.busy());
    assert!(h.get_by_label("Retry failed (1)").accesskit_node().is_disabled(), "no retry while another import runs");
    // Even asked directly, a refused retry keeps the paths to retry later.
    let t = *h.state().theme.tokens();
    h.state_mut().browse_action(super::super::library_browsers::Browse::RetryImport, &t);
    assert_eq!(h.state().library.browsers.report.as_ref().unwrap().retry, std::slice::from_ref(&missing));
    release.send(()).unwrap();
    drop(release);
    settle(&mut h);
    assert_eq!(sources(&h), ["shot.png"]);
}

/// A fragmented MP4 states no length in its headers; the configured video service measures
/// it before the import, as for a dropped file.
#[test]
#[ignore = "runs explicitly configured ffmpeg and ffprobe"]
fn an_imported_video_keeps_the_length_the_video_service_measured() {
    use nodecraft_types::Value;
    let ffmpeg = PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let ffprobe = PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fragmented.mp4");
    let status = std::process::Command::new(&ffmpeg)
        .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", "color=c=green:s=64x48:r=4", "-t", "0.5", "-an", "-c:v", "libx264"])
        .args(["-pix_fmt", "yuv420p", "-movflags", "empty_moov+frag_keyframe+default_base_moof"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(nodecraft_media::probe_movie(&std::fs::read(&path).unwrap()).unwrap().duration_ms, None);
    let mut h = harness();
    h.state_mut().videos.configure(Some((ffmpeg.as_path(), ffprobe.as_path())));
    open_assets(&mut h);
    h.state_mut().library.browsers.import.pick = Some(vec![path]);
    h.get_by_label("Import files…").click();
    h.run();
    settle(&mut h);
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    let video = canvas.nodes().values().find(|n| n.kind == "source.video").expect("a video card");
    let Some(Value::Media(asset)) = video.params.get("asset") else { panic!("video source") };
    assert_eq!((asset.meta.duration_ms, asset.meta.width, asset.meta.height), (Some(500), Some(64), Some(48)));
}

/// Import from URL… hands the fetched file to the import a picked file takes: a card titled with
/// the address's file name, and nothing else of the address in the document. The fetch is
/// canned, so no test reaches the network.
#[test]
fn a_file_from_an_address_becomes_a_card_named_after_it_and_keeps_nothing_else_of_the_address() {
    let mut h = harness();
    open_assets(&mut h);
    h.get_by_label("Import from URL…").click();
    h.run();
    assert!(h.state().library.browsers.url.is_some(), "the address row opens");

    // A local address is refused before anything starts.
    h.state_mut().library.browsers.url = Some("https://192.168.1.20/photo.png".into());
    h.run();
    assert!(h.get_by_label("Import").accesskit_node().is_disabled());
    assert!(h.query_by_label("addresses on this computer or its local network aren't imported").is_some());

    let address = "https://cdn.example.com/photos/sunset%20glow.png?sig=s3cr3t";
    h.state_mut().library.browsers.url = Some(address.into());
    h.state_mut().library.browsers.import.fetched =
        Some(Ok(Fetched { name: "sunset glow.png".into(), host: "cdn.example.com".into(), kind: nodecraft_types::Modality::Image, bytes: png(90) }));
    let before = h.state().session.document().clone();
    h.run();
    h.get_by_label("Import").click();
    h.run();
    settle(&mut h);
    assert_eq!(sources(&h), ["sunset glow.png"], "{:?}", h.state().notice());
    assert!(h.query_by_label("Import: 1 added").is_some());
    let saved = serde_json::to_string(h.state().session.document()).unwrap();
    for leaked in ["s3cr3t", "cdn.example.com", "photos", "https"] {
        assert!(!saved.contains(leaked), "{leaked} reached the document");
    }
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before, "one undo step");

    // A failed download is reported by host, with nothing to retry.
    h.state_mut().library.browsers.url = Some("https://files.example.org/x.png".into());
    h.state_mut().library.browsers.import.fetched =
        Some(Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::NotFound, "files.example.org has no file at that address")));
    h.run();
    h.get_by_label("Import").click();
    h.run();
    settle(&mut h);
    assert!(h.query_by_label("From files.example.org: files.example.org has no file at that address").is_some());
    assert!(h.query_by_label_contains("Retry failed").is_none());
    assert_eq!(h.state().session.document(), &before);
}

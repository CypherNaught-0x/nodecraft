//! Videos on cards (#105): set-up messages, and real playback when tools are given.
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};

/// A few bytes that sniff as WebM; enough for the card, not for ffmpeg.
fn webm_header() -> Vec<u8> {
    let mut bytes = b"\x1A\x45\xDF\xA3\x9f\x42\x82\x84webm".to_vec();
    bytes.resize(64, 0);
    bytes
}

fn add_video(h: &mut Harness<'static, NodeCraftApp>, bytes: &[u8]) -> NodeId {
    let canvas = h.state().canvas;
    let id = NodeId::from_bytes([50; 16]);
    let at = nodecraft_graph::Position { x: -80.0, y: -160.0 };
    let imported = import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "clip.webm", bytes }], &[id]).unwrap();
    assert_eq!(imported.nodes, vec![id]);
    id
}

/// Frames until `label` shows up (work happens off the frame), or `None`. Steps rather than
/// runs: while video work is outstanding the card keeps asking to be redrawn.
fn wait_for(h: &mut Harness<'static, NodeCraftApp>, label: &str, frames: usize) -> Option<()> {
    for _ in 0..frames {
        h.step();
        if h.query_by_label(label).is_some() {
            return Some(());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    None
}

#[test]
fn a_video_card_says_how_to_set_up_playback() {
    let mut h = harness();
    add_video(&mut h, &webm_header());
    h.run();
    assert!(h.query_by_label(crate::video::UNCONFIGURED).is_some());
    assert!(h.query_by_label("Play video").is_none(), "no controls without a video");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn tools_that_arent_there_say_so_without_panicking() {
    let mut h = harness();
    let missing = std::env::temp_dir().join(format!("nodecraft-no-such-tool-{}", std::process::id()));
    let mut settings = NodeCraftSettings::default();
    settings.media.ffmpeg = Some(missing.join("ffmpeg"));
    settings.media.ffprobe = Some(missing.join("ffprobe"));
    let ctx = h.ctx.clone();
    h.state_mut().apply_settings(&ctx, &settings);
    add_video(&mut h, &webm_header());
    let mut said = None;
    for _ in 0..400 {
        h.step();
        said = ["The ffmpeg or ffprobe in [media] settings can't play videos", "Can't run ffmpeg or ffprobe; check [media] in settings"]
            .into_iter()
            .find(|label| h.query_by_label(label).is_some());
        if said.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(said.is_some(), "the card says the tools can't run");
    // Nothing is kept for tools that can't run, and nothing is left polling.
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(h.state().videos.test_counts().0, 0, "the read video was let go");
}

/// Plays a real clip through installed tools: opt-in, like `media`'s own real-tool tests.
/// `NODECRAFT_VIDEO_TEST_FFMPEG=/abs/ffmpeg NODECRAFT_VIDEO_TEST_FFPROBE=/abs/ffprobe cargo test
/// -p nodecraft-ui-egui actual_ -- --ignored`; `NODECRAFT_VIDEO_TEST_SHOT=/abs/file.png` also
/// saves a screenshot of the playing card.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "needs installed ffmpeg and ffprobe"]
fn actual_video_plays_pauses_and_scrubs_on_its_card() {
    let ffmpeg = std::path::PathBuf::from(std::env::var("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let ffprobe = std::path::PathBuf::from(std::env::var("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap());
    let clip = std::env::temp_dir().join(format!("nodecraft-video105-{}.webm", std::process::id()));
    let made = std::process::Command::new(&ffmpeg)
        .args(["-v", "error", "-f", "lavfi", "-i", "testsrc=size=160x90:rate=12:duration=2", "-c:v", "libvpx-vp9", "-pix_fmt", "yuv420p"])
        .arg(&clip)
        .status()
        .unwrap();
    assert!(made.success());
    let bytes = std::fs::read(&clip).unwrap();
    std::fs::remove_file(&clip).unwrap();

    let mut h = Harness::builder()
        .with_step_dt(1.0 / 30.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
    let mut settings = NodeCraftSettings::default();
    settings.media.ffmpeg = Some(ffmpeg);
    settings.media.ffprobe = Some(ffprobe);
    let ctx = h.ctx.clone();
    h.state_mut().apply_settings(&ctx, &settings);
    let id = add_video(&mut h, &bytes);
    assert!(wait_for(&mut h, "Video, 0:02", 1000).is_some(), "the poster shows");
    assert!(h.query_by_label("0:00 / 0:02").is_some());

    h.get_by_label("Play video").click();
    assert!(wait_for(&mut h, "Pause video", 50).is_some(), "playing");
    if let Ok(shot) = std::env::var("NODECRAFT_VIDEO_TEST_SHOT") {
        for _ in 0..20 {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        h.render().unwrap().save(shot).unwrap();
    }
    // Two seconds of clip at 1/30 s a frame: it ends and offers to play again.
    assert!(wait_for(&mut h, "Play video", 400).is_some(), "it stops at the end");
    assert!(h.query_by_label("0:02 / 0:02").is_some());

    // Scrubbing while paused shows that frame; nothing is written to the document.
    let revision = h.state().session.revision();
    let ctx = h.ctx.clone();
    h.state_mut().videos.act(&ctx, id, crate::video::VideoIntent::Seek(1_000_000));
    assert!(wait_for(&mut h, "0:01 / 0:02", 50).is_some());
    assert_eq!(h.state().session.revision(), revision);
}

#[cfg(not(target_arch = "wasm32"))]
#[path = "video_tests/vp9_573.rs"]
mod vp9_573;

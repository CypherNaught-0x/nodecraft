//! Real installed-tool regression for #573; fixtures are generated, never downloaded.
use super::*;
use std::{
    io::{Read, Seek},
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn run(command: &mut Command) -> Vec<u8> {
    let mut output = tempfile::tempfile().unwrap();
    let mut errors = tempfile::tempfile().unwrap();
    let mut child = command.stdin(Stdio::null()).stdout(output.try_clone().unwrap()).stderr(errors.try_clone().unwrap()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("could not poll installed video tool: {error}");
            }
        }
        let within_capture = [&output, &errors].into_iter().all(|file| file.metadata().is_ok_and(|meta| meta.len() <= 64 * 1024));
        if Instant::now() >= deadline || !within_capture {
            let _ = child.kill();
            let _ = child.wait();
            panic!("installed video tool exceeded its time or diagnostic-output bound");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    output.rewind().unwrap();
    errors.rewind().unwrap();
    let mut diagnostic = Vec::new();
    errors.take(64 * 1024).read_to_end(&mut diagnostic).unwrap();
    assert!(status.success(), "installed tool refused the procedural VP9 fixture: {}", String::from_utf8_lossy(&diagnostic));
    let mut bytes = Vec::new();
    output.take(64 * 1024).read_to_end(&mut bytes).unwrap();
    bytes
}

/// Run with NODECRAFT_VIDEO_TEST_FFMPEG and NODECRAFT_VIDEO_TEST_FFPROBE set to installed
/// executables. `cargo test -p nodecraft-ui-egui actual_vp9_profiles -- --ignored --nocapture`.
#[test]
#[ignore = "needs installed ffmpeg with libvpx-vp9, and ffprobe"]
fn actual_vp9_profiles_reach_a_playable_card_or_an_explicit_refusal() {
    let ffmpeg = PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let ffprobe = PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap());
    let fixtures = tempfile::tempdir().unwrap();
    for (pixel_format, profile, expected) in
        [("yuv420p", "Profile 0", "Video, 0:02"), ("yuv444p", "Profile 1", "Video, 0:02"), ("gbrp", "Profile 1", "This video's format isn't supported")]
    {
        // The reported testsrc command chooses gbrp with installed ffmpeg 9. Explicit formats
        // keep the YUV444 and unsupported RGB cases distinct across future encoder defaults.
        let clip = fixtures.path().join(format!("{pixel_format}.webm"));
        run(Command::new(&ffmpeg)
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=10",
                "-t",
                "2",
                "-c:v",
                "libvpx-vp9",
                "-b:v",
                "0",
                "-pix_fmt",
                pixel_format,
            ])
            .arg(&clip));
        let metadata: serde_json::Value = serde_json::from_slice(&run(Command::new(&ffprobe)
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,profile,pix_fmt", "-of", "json"])
            .arg(&clip)))
        .unwrap();
        assert_eq!(metadata["streams"][0]["codec_name"], "vp9");
        assert_eq!(metadata["streams"][0]["profile"], profile);
        assert_eq!(metadata["streams"][0]["pix_fmt"], pixel_format);

        let mut h = Harness::builder()
            .with_step_dt(1.0 / 30.0)
            .with_size(Vec2::new(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
        let mut settings = NodeCraftSettings::default();
        settings.media.ffmpeg = Some(ffmpeg.clone());
        settings.media.ffprobe = Some(ffprobe.clone());
        let ctx = h.ctx.clone();
        h.state_mut().apply_settings(&ctx, &settings);
        add_video(&mut h, &std::fs::read(&clip).unwrap());
        let revision = h.state().session.revision();
        let started = Instant::now();
        loop {
            h.step();
            if h.query_by_label(expected).is_some() {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(25),
                "{pixel_format}: never showed {expected}; sources/posters/players={:?}, notice={:?}",
                h.state().videos.test_counts(),
                h.state().notice
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(h.query_by_label("Play video").is_some(), pixel_format != "gbrp");
        assert_eq!(h.state().session.revision(), revision, "preview/refusal is view state");
        eprintln!("VP9 {profile}/{pixel_format}: {expected} after {} ms", started.elapsed().as_millis());
    }
}

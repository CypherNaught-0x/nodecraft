//! Sounds on cards (#106): set-up messages, and the waveform, playing and seeking when tools are
//! given.
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};

/// A few bytes that sniff as WAV; enough for the card, not for ffmpeg.
fn wav_header() -> Vec<u8> {
    let mut bytes = b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec();
    bytes.resize(64, 0);
    bytes
}

fn add_sound(h: &mut Harness<'static, NodeCraftApp>, bytes: &[u8]) -> NodeId {
    let canvas = h.state().canvas;
    let id = NodeId::from_bytes([60; 16]);
    let at = nodecraft_graph::Position { x: -80.0, y: -160.0 };
    let imported = import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "tone.wav", bytes }], &[id]).unwrap();
    assert_eq!(imported.nodes, vec![id]);
    id
}

/// Frames until `found` holds (work happens off the frame), stepping rather than running: while
/// sound work is outstanding the card keeps asking to be redrawn.
fn wait_until(h: &mut Harness<'static, NodeCraftApp>, frames: usize, found: impl Fn(&Harness<'static, NodeCraftApp>) -> bool) -> bool {
    for _ in 0..frames {
        h.step();
        if found(h) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

fn wait_for(h: &mut Harness<'static, NodeCraftApp>, label: &str, frames: usize) -> bool {
    wait_until(h, frames, |h| h.query_by_label(label).is_some())
}

/// A real pointer press and release at `pos`, after hovering it for a frame.
fn click_at(h: &mut Harness<'static, NodeCraftApp>, pos: egui::Pos2) {
    h.hover_at(pos);
    h.step();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
}

#[test]
fn a_sound_card_says_how_to_set_up_playback() {
    let mut h = harness();
    add_sound(&mut h, &wav_header());
    h.run();
    assert!(h.query_by_label(crate::audio::UNCONFIGURED).is_some());
    assert!(h.query_by_label("Play sound").is_none(), "no controls without a sound");
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
    add_sound(&mut h, &wav_header());
    let said = wait_until(&mut h, 400, |h| {
        ["The ffmpeg or ffprobe in [media] settings can't play sounds", "Can't run ffmpeg or ffprobe; check [media] in settings"]
            .into_iter()
            .any(|label| h.query_by_label(label).is_some())
    });
    assert!(said, "the card says the tools can't run");
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(h.state().audios.test_counts().0, 0, "the read sound was let go");
}

/// A 4 s sound: a 440 Hz tone, then silence, each for a second, twice.
#[cfg(not(target_arch = "wasm32"))]
fn tone(ffmpeg: &std::path::Path) -> Vec<u8> {
    let path = std::env::temp_dir().join(format!("nodecraft-audio106-{}.wav", std::process::id()));
    let source = "aevalsrc=exprs='if(lt(mod(t\\,2)\\,1)\\,0.5*sin(2*PI*440*t)\\,0)':d=4:s=44100";
    let made = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", source, "-c:a", "pcm_s16le"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(made.success());
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    bytes
}

/// The app with the tools in `NODECRAFT_VIDEO_TEST_FFMPEG`/`_FFPROBE`, and a sound card whose
/// waveform has been made.
#[cfg(not(target_arch = "wasm32"))]
fn with_sound(builder: egui_kittest::HarnessBuilder<NodeCraftApp>) -> (Harness<'static, NodeCraftApp>, NodeId) {
    let ffmpeg = std::path::PathBuf::from(std::env::var("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let ffprobe = std::path::PathBuf::from(std::env::var("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap());
    let bytes = tone(&ffmpeg);
    let mut h = builder.build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
    let mut settings = NodeCraftSettings::default();
    settings.media.ffmpeg = Some(ffmpeg);
    settings.media.ffprobe = Some(ffprobe);
    let ctx = h.ctx.clone();
    h.state_mut().apply_settings(&ctx, &settings);
    let id = add_sound(&mut h, &bytes);
    assert!(wait_for(&mut h, "Sound, 0:04", 1000), "the sound opens");
    assert!(wait_until(&mut h, 1000, |h| h.state().audios.test_counts().1 == 1), "the waveform is made");
    (h, id)
}

/// Shows, seeks and plays a real sound through installed tools: opt-in, like `media`'s own
/// real-tool tests. `NODECRAFT_VIDEO_TEST_FFMPEG=/abs/ffmpeg NODECRAFT_VIDEO_TEST_FFPROBE=/abs/ffprobe
/// cargo test -p nodecraft-ui-egui actual_sound -- --ignored`. It plays a second of tone through
/// the default sound device when there is one.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "needs installed ffmpeg and ffprobe"]
fn actual_sound_shows_its_waveform_seeks_and_plays_on_its_card() {
    let builder = Harness::builder().with_step_dt(1.0 / 30.0).with_max_steps(120).with_size(Vec2::new(1000.0, 700.0));
    let (mut h, id) = with_sound(builder);
    assert!(h.query_by_label("0:00 / 0:04").is_some());
    let revision = h.state().session.revision();

    // A click halfway along the waveform goes to the middle of the sound.
    let wave = h.get_by_label("Sound, 0:04").rect();
    click_at(&mut h, wave.center());
    assert!(wait_for(&mut h, "0:02 / 0:04", 10), "the click seeks");
    // And one near its start goes back there.
    click_at(&mut h, egui::Pos2::new(wave.left() + 1.0, wave.center().y));
    assert!(wait_for(&mut h, "0:00 / 0:04", 10), "back to the start");

    h.get_by_label("Play sound").click();
    h.step();
    let playing = wait_for(&mut h, "Pause sound", 10);
    if playing {
        // About a second plays (the tone), then it pauses there.
        assert!(wait_for(&mut h, "0:01 / 0:04", 300), "playing moves on");
        h.get_by_label("Pause sound").click();
        assert!(wait_for(&mut h, "Play sound", 10), "paused");
        assert!(!h.state().audios.test_counts().3, "the device is let go");
        // A seek while paused keeps it paused.
        click_at(&mut h, wave.center());
        assert!(wait_for(&mut h, "0:02 / 0:04", 10));
        assert!(h.query_by_label("Play sound").is_some());
    } else {
        // No sound device here: the card says so, and nothing plays.
        assert!(wait_for(&mut h, "No sound output device found", 10) || wait_for(&mut h, "Can't open the sound device", 10));
        assert!(!h.state().audios.test_counts().3);
    }
    assert_eq!(h.state().session.revision(), revision, "nothing is written to the document");

    // Deleting the card forgets its player and stops it.
    let ctx = h.ctx.clone();
    h.state_mut().audios.act(&ctx, id, crate::audio::AudioIntent::Toggle);
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("node.remove", &serde_json::json!({ "canvas": canvas, "id": id })).unwrap();
    for _ in 0..3 {
        h.step();
    }
    let (_, _, players, playing) = h.state().audios.test_counts();
    assert_eq!((players, playing), (0, false));
}

/// Screenshots of a sound card part-way through, dark and light, for review.
/// `NODECRAFT_REVIEW_EVIDENCE=/abs/folder` plus the tools above.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "renders review evidence"]
fn render_audio_card_review_evidence() {
    let folder = std::path::PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    let builder = Harness::builder().wgpu().with_step_dt(1.0 / 30.0).with_max_steps(120).with_size(Vec2::new(900.0, 640.0));
    let (mut h, id) = with_sound(builder);
    let ctx = h.ctx.clone();
    h.state_mut().audios.act(&ctx, id, crate::audio::AudioIntent::Seek(1_500_000));
    for (kind, name) in [(crate::theme::ThemeKind::Dark, "audio-card-dark.png"), (crate::theme::ThemeKind::Light, "audio-card-light.png")] {
        crate::theme::apply(&h.ctx, kind);
        h.state_mut().theme = kind;
        for _ in 0..5 {
            h.step();
        }
        h.render().unwrap().save(folder.join(name)).unwrap();
    }
}

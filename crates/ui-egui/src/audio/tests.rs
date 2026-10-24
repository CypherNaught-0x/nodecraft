use super::*;
use nodecraft_engine::run_jobs::{JobKey, Take};
use nodecraft_graph::Generation;
use nodecraft_types::Blake3;
use nodecraft_types::{GenerationId, NodeId};

fn media(mime: &str) -> Value {
    Value::Media(AssetRef { hash: Blake3::from_bytes([7; 32]), mime: mime.parse().unwrap(), meta: Default::default() })
}

fn node(kind: &str, params: &[(&str, Value)]) -> Node {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let supplied: nodecraft_types::ParamValues = params.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
    kinds.get(kind).unwrap().instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &supplied).unwrap()
}

#[test]
fn a_source_sound_shows_its_asset_and_nothing_else_does_by_mistake() {
    let sound = node("source.audio", &[("asset", media("audio/wav"))]);
    assert_eq!(audio_of(&sound).map(|a| a.mime.essence()), Some("audio/wav"));
    let video = node("source.video", &[("asset", media("video/webm"))]);
    assert!(audio_of(&video).is_none());
    let mut wrong = Node::new(NodeId::from_bytes([2; 16]), "source.audio");
    wrong.params.insert("asset".into(), media("image/png"));
    assert!(audio_of(&wrong).is_none());
}

#[test]
fn a_result_shows_its_first_sound_inside_lists_too() {
    let mut text = node("source.text", &[]);
    let id = GenerationId::from_bytes([3; 16]);
    text.history.push(Generation::new(id, vec![Value::Text("caption".into()), Value::List(vec![media("image/png"), media("audio/mpeg")])]));
    assert!(audio_of(&text).is_none(), "only the active result counts");
    text.active = Some(id);
    assert_eq!(audio_of(&text).map(|a| a.mime.essence()), Some("audio/mpeg"));
}

#[test]
fn a_sound_card_has_room_for_its_waveform() {
    let t = *crate::theme::ThemeKind::Dark.tokens();
    let sound = node("source.audio", &[("asset", media("audio/wav"))]);
    let empty = node("source.audio", &[]);
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let d = kinds.get("source.audio").ok().map(|k| k.descriptor());
    let grown = crate::card::world_size(&sound, d, &t).y - crate::card::world_size(&empty, d, &t).y;
    assert_eq!(grown, t.card_preview_height);
}

#[test]
fn the_time_under_the_pointer_runs_from_start_to_end() {
    assert_eq!(time_at(10.0, 10.0, 100.0, 4_000_000), 0);
    assert_eq!(time_at(60.0, 10.0, 100.0, 4_000_000), 2_000_000);
    assert_eq!(time_at(110.0, 10.0, 100.0, 4_000_000), 4_000_000);
    // Off either end, it clamps; a degenerate waveform or pointer is the start.
    assert_eq!(time_at(-1e9, 10.0, 100.0, 4_000_000), 0);
    assert_eq!(time_at(1e9, 10.0, 100.0, 4_000_000), 4_000_000);
    assert_eq!(time_at(1e9, 10.0, 100.0, u64::MAX), u64::MAX);
    assert_eq!(time_at(50.0, 10.0, 0.0, 4_000_000), 0);
    assert_eq!(time_at(f32::NAN, 10.0, 100.0, 4_000_000), 0);
    assert_eq!(time_at(50.0, 10.0, f32::INFINITY, 4_000_000), 0);
    assert_eq!(time_at(50.0, 10.0, f32::NAN, 4_000_000), 0);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn unset_tools_say_how_to_set_them_and_start_nothing() {
    let ctx = egui::Context::default();
    let mut audios = Audios::default();
    audios.configure(None);
    let asset = AssetRef { hash: Blake3::from_bytes([7; 32]), mime: "audio/wav".parse().unwrap(), meta: Default::default() };
    let view = audios.view(&ctx, NodeId::from_bytes([1; 16]), &asset, || panic!("nothing is read without tools"));
    assert_eq!(view, AudioView::Unavailable(UNCONFIGURED));
    // Asking a card that never showed a sound to play does nothing.
    audios.act(&ctx, NodeId::from_bytes([1; 16]), AudioIntent::Toggle);
    audios.act(&ctx, NodeId::from_bytes([9; 16]), AudioIntent::Seek(u64::MAX));
    assert_eq!(audios.test_counts(), (0, 0, 0, false));
}

#[test]
fn a_preview_plays_on_into_its_next_preview_and_its_jobs_result_only() {
    use nodecraft_exec::scheduler::JobId;
    let job = Take { job: JobKey { run: 1, job: JobId(1) }, attempt: 1 };
    let other = Take { job: JobKey { run: 2, job: JobId(1) }, attempt: 1 };
    let retry = Take { attempt: 2, ..job };
    let [a, b] = [Blake3::from_bytes([1; 32]), Blake3::from_bytes([2; 32])];
    assert_eq!(change(a, Some(job), a, Some(job)), Change::Same);
    assert_eq!(change(a, None, a, None), Change::Same);
    // The sound so far grew, or the job finished with its result: play on from the same place.
    assert_eq!(change(a, Some(job), b, Some(job)), Change::Continue);
    // Another job's sound, a sound no job is making, or a result after a result: from the start.
    assert_eq!(change(a, Some(job), b, Some(other)), Change::Restart);
    // A retry of the same job is another take: it starts again, not where the first one was.
    assert_eq!(change(a, Some(job), b, Some(retry)), Change::Restart);
    assert_eq!(change(a, Some(job), b, None), Change::Restart);
    assert_eq!(change(a, None, b, Some(job)), Change::Restart, "only a preview plays on");
    assert_eq!(change(a, None, b, None), Change::Restart);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn unset_tools_say_so_for_a_preview_too_and_keep_nothing() {
    use nodecraft_exec::scheduler::JobId;
    let ctx = egui::Context::default();
    let mut audios = Audios::default();
    let bytes: Arc<[u8]> = Arc::from(&b"ID3\x04\0\0\0\0\0\0\xFF\xFB\x90\x64"[..]);
    let preview = nodecraft_engine::run_jobs::AudioPreview {
        take: Take { job: JobKey { run: 1, job: JobId(1) }, attempt: 1 },
        mime: "audio/mpeg",
        hash: Blake3::from_bytes(*blake3::hash(&bytes).as_bytes()),
        bytes,
    };
    assert_eq!(audios.view_preview(&ctx, NodeId::from_bytes([1; 16]), &preview), AudioView::Unavailable(UNCONFIGURED));
    assert_eq!(audios.test_counts(), (0, 0, 0, false));
}

/// A preview of an MP3 plays, then its job's result takes over at the same place (#875), and the
/// preview is let go. Opt-in like the other real-tool tests:
/// `NODECRAFT_VIDEO_TEST_FFMPEG=/abs/ffmpeg NODECRAFT_VIDEO_TEST_FFPROBE=/abs/ffprobe
/// cargo test -p nodecraft-ui-egui preview_then_result -- --ignored`. It plays through the
/// default sound device when there is one.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "needs installed ffmpeg and ffprobe"]
fn actual_preview_then_result_plays_on_from_the_same_place() {
    use nodecraft_exec::scheduler::JobId;
    let ffmpeg = std::path::PathBuf::from(std::env::var("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let ffprobe = std::path::PathBuf::from(std::env::var("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap());
    let path = std::env::temp_dir().join(format!("nodecraft-audio875-{}.mp3", std::process::id()));
    let made = std::process::Command::new(&ffmpeg)
        .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=6", "-c:a", "libmp3lame", "-b:a", "128k", "-write_xing", "0"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(made.success());
    let whole = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    // Half the MP3, cut mid-frame, as a stream would be. A stream can't know its length up
    // front, so it has no Xing header saying how long the whole is; neither does this one.
    let half: Arc<[u8]> = Arc::from(&whole[..whole.len() / 2 + 7]);
    let job = Take { job: JobKey { run: 1, job: JobId(1) }, attempt: 1 };
    let preview =
        nodecraft_engine::run_jobs::AudioPreview { take: job, mime: "audio/mpeg", hash: Blake3::from_bytes(*blake3::hash(&half).as_bytes()), bytes: half };
    let mut session =
        nodecraft_engine::Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([75; 16]), "Speech").unwrap()).unwrap();
    let hash = session.import_asset(&whole).unwrap();
    let result = AssetRef { hash, mime: "audio/mpeg".parse().unwrap(), meta: Default::default() };

    let ctx = egui::Context::default();
    let mut audios = Audios::default();
    audios.configure(Some((&ffmpeg, &ffprobe)));
    let card = NodeId::from_bytes([1; 16]);
    let wait = |audios: &mut Audios, view: &mut dyn FnMut(&mut Audios) -> AudioView, done: &dyn Fn(&AudioView) -> bool| {
        for _ in 0..2000 {
            audios.poll(&ctx);
            let shown = view(audios);
            if done(&shown) {
                return shown;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("never got there");
    };
    let shown = wait(&mut audios, &mut |a| a.view_preview(&ctx, card, &preview), &|v| matches!(v, AudioView::Sound { .. }));
    let AudioView::Sound { peaks, duration_us, .. } = shown else { panic!("not a sound") };
    assert!(peaks.is_none(), "a preview has no waveform");
    assert!((2_000_000..4_000_000).contains(&duration_us), "about three of six seconds: {duration_us}");
    audios.act(&ctx, card, AudioIntent::Seek(1_500_000));
    audios.act(&ctx, card, AudioIntent::Toggle);
    let playing = audios.test_counts().3;
    std::thread::sleep(std::time::Duration::from_millis(300));

    // The job finishes: its result takes over from where the preview was.
    let read = |session: &nodecraft_engine::Session| session.assets().preview_read(&result);
    let shown = wait(
        &mut audios,
        &mut |a| a.view_result(&ctx, card, &result, Some(job), || read(&session)),
        &|v| matches!(v, AudioView::Sound { duration_us, .. } if *duration_us > 5_000_000),
    );
    let AudioView::Sound { position_us, playing: still, .. } = shown else { panic!("not a sound") };
    assert!(position_us >= 1_500_000, "it plays on from the same place, not the start: {position_us}");
    assert_eq!(still, playing, "playing on if it played");
    assert_eq!(audios.test_counts().0, 1, "the preview is let go once the result plays");

    // Another card plays a preview whose job then ends with nothing for the card to show.
    let other = NodeId::from_bytes([2; 16]);
    wait(&mut audios, &mut |a| a.view_preview(&ctx, other, &preview), &|v| matches!(v, AudioView::Sound { .. }));
    audios.act(&ctx, other, AudioIntent::Toggle);
    assert!(audios.test_counts().3, "it plays");
    audios.let_go(other);
    let (sources, _, players, playing) = audios.test_counts();
    assert_eq!((sources, players, playing), (1, 1, false), "it stops, and the preview's bytes go with its player");
    audios.stop();
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_preview_whose_job_ended_is_let_go_and_nothing_else_is() {
    use nodecraft_exec::scheduler::JobId;
    let mut audios = Audios::default();
    let card = NodeId::from_bytes([1; 16]);
    audios.let_go(card);
    assert_eq!(audios.test_counts(), (0, 0, 0, false), "a card with no player has nothing to let go");
    audios.test_preview_player(card, Blake3::from_bytes([3; 32]), Take { job: JobKey { run: 1, job: JobId(1) }, attempt: 1 });
    assert_eq!(audios.test_counts().2, 1);
    audios.let_go(card);
    assert_eq!(audios.test_counts(), (0, 0, 0, false), "its player is gone, so nothing plays it on");
}

#[cfg(not(target_arch = "wasm32"))]
mod ring {
    use super::super::output::{MAX_FRAMES, Ring, fill, us};
    use std::sync::Mutex;

    fn stereo(frames: &[[i16; 2]]) -> Vec<i16> {
        frames.iter().flatten().copied().collect()
    }

    #[test]
    fn frames_last_their_time() {
        assert_eq!(us(0), 0);
        assert_eq!(us(48_000), 1_000_000);
        assert_eq!(us(96_000), 2_000_000);
        assert_eq!(us(1), 20);
        assert_eq!(us(u64::MAX), u64::MAX, "saturates");
    }

    #[test]
    fn at_the_same_rate_it_plays_each_frame_once_and_counts_them() {
        let mut ring = Ring::new(48_000);
        assert_eq!(ring.push(&stereo(&[[16_384, -16_384], [0, 32_767]])), 2);
        assert_eq!(ring.next(), Some([0.5, -0.5]));
        assert_eq!(ring.played(), 1);
        assert_eq!(ring.next(), Some([0.0, 32_767.0 / 32_768.0]));
        assert_eq!(ring.played(), 2);
        // Run dry: silence, and the count stays where playing stopped.
        assert_eq!(ring.next(), None);
        assert_eq!(ring.played(), 2);
        assert_eq!(ring.buffered(), 0);
    }

    #[test]
    fn a_faster_device_gets_frames_in_between() {
        // 96 kHz: two device frames per decoded frame, the second halfway to the next.
        let mut ring = Ring::new(96_000);
        ring.push(&stereo(&[[0, 0], [16_384, -16_384], [16_384, -16_384]]));
        assert_eq!(ring.next(), Some([0.0, 0.0]));
        assert_eq!(ring.next(), Some([0.25, -0.25]));
        assert_eq!(ring.played(), 1);
        assert_eq!(ring.next(), Some([0.5, -0.5]));
    }

    #[test]
    fn a_slower_device_skips_and_counts_every_frame_it_passed() {
        let mut ring = Ring::new(24_000);
        ring.push(&stereo(&[[1, 1], [2, 2], [3, 3], [4, 4], [5, 5]]));
        let played: Vec<f32> = std::iter::from_fn(|| ring.next()).map(|[l, _]| l * 32_768.0).collect();
        assert_eq!(played, vec![1.0, 3.0, 5.0]);
        assert_eq!(ring.played(), 5);
        // 44.1 kHz plays 48 kHz through in about the time it takes.
        let mut ring = Ring::new(44_100);
        ring.push(&vec![0; 2 * 48_000]);
        let device = std::iter::from_fn(|| ring.next()).count();
        assert!(device.abs_diff(44_100) <= 1, "{device}");
        assert_eq!(ring.played(), 48_000);
    }

    #[test]
    fn it_holds_no_more_than_its_cap_and_ignores_a_part_frame() {
        let mut ring = Ring::new(48_000);
        assert_eq!(ring.push(&[1, 2, 3]), 1, "a stray sample is no frame");
        let more = vec![0i16; 2 * MAX_FRAMES];
        assert_eq!(ring.push(&more), MAX_FRAMES - 1);
        assert_eq!(ring.buffered(), MAX_FRAMES);
        assert_eq!(ring.push(&more), 0);
    }

    #[test]
    fn a_reset_forgets_what_waits_and_what_played() {
        let mut ring = Ring::new(48_000);
        ring.push(&stereo(&[[1, 1], [2, 2]]));
        ring.next();
        ring.reset();
        assert_eq!((ring.played(), ring.buffered()), (0, 0));
        assert_eq!(ring.next(), None);
        // A device that reports no rate is played at the decoded one rather than divided by.
        let mut odd = Ring::new(0);
        odd.push(&stereo(&[[1, 1]]));
        assert!(odd.next().is_some());
        assert_eq!(odd.played(), 1);
    }

    #[test]
    fn every_channel_layout_is_filled_and_runs_out_into_silence() {
        let ring = Mutex::new(Ring::new(48_000));
        ring.lock().unwrap().push(&stereo(&[[16_384, -16_384], [16_384, 16_384]]));
        let mut mono = [1.0f32; 3];
        fill(&mut mono, 1, &ring);
        assert_eq!(mono, [0.0, 0.5, 0.0], "mono mixes both sides, then silence");
        ring.lock().unwrap().push(&stereo(&[[16_384, -16_384]]));
        let mut surround = [7i16; 8];
        fill(&mut surround, 4, &ring);
        assert_eq!(surround, [16_384, -16_384, 0, 0, 0, 0, 0, 0], "extra channels are silent");
        ring.lock().unwrap().push(&stereo(&[[0, 0]]));
        let mut unsigned = [0u16; 3];
        fill(&mut unsigned, 2, &ring);
        assert_eq!(unsigned, [32_768, 32_768, 32_768], "silence is the middle, and a part frame is filled too");
        let mut nothing: [f32; 0] = [];
        fill(&mut nothing, 0, &ring);
        let mut zero_channels = [5.0f32; 2];
        fill(&mut zero_channels, 0, &ring);
        assert_eq!(zero_channels, [0.0, 0.0], "a device saying zero channels is treated as mono");
    }

    #[test]
    #[ignore = "opens the real default sound device"]
    fn the_default_sound_device_opens_and_plays_silence() {
        let output = super::super::output::Output::open().expect("a sound device");
        output.push(&vec![0; 2 * 4_800]);
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert!(output.played() > 0, "the device took frames");
        assert!(!output.failed());
    }
}

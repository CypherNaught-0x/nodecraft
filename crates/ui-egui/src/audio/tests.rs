use super::*;
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

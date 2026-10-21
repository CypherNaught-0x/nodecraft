use super::*;
use crate::action_exec::LocalActions;
use nodecraft_exec::run::ActionExecutor;
use nodecraft_format::AssetStore;
use nodecraft_nodes::Registry;
use nodecraft_types::{AssetRef, Blake3, MediaMeta, NodeId, Number, Value};

/// `action`'s planned parameters: its defaults with `set` over them.
fn planned(action: &str, set: &[(&str, Value)]) -> ParamValues {
    let kinds = Registry::builtins().unwrap();
    let kind = kinds.get(action).unwrap();
    let mut node = kind.instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &Default::default()).unwrap();
    for (name, value) in set {
        node.params.insert((*name).into(), value.clone());
    }
    kind.validate_instance(&node, true).unwrap().into_visible_values()
}

fn n(value: f64) -> Value {
    Value::Number(Number::new(value).unwrap())
}

fn text(value: &str) -> Value {
    Value::Text(value.into())
}

#[test]
fn settings_are_read_and_checked_before_any_video_is_imported() {
    assert_eq!(edit(TRIM, &planned(TRIM, &[("start", n(1.5)), ("end", n(2.25))]), 1), Ok(Edit::Trim { start: 1_500_000, end: 2_250_000 }));
    assert!(edit(TRIM, &planned(TRIM, &[("start", n(3.0)), ("end", n(2.0))]), 1).unwrap_err().contains("after the end"));
    assert_eq!(edit(TRIM, &planned(TRIM, &[("start", n(3.0))]), 1), Ok(Edit::Trim { start: 3_000_000, end: 0 }));
    assert!(edit(TRIM, &planned(TRIM, &[]), 0).unwrap_err().contains("connect a video"));
    assert_eq!(edit(SPEED, &planned(SPEED, &[("speed", n(0.5)), ("sound", text("mute"))]), 1), Ok(Edit::Speed { speed: 500, mute: true }));
    assert_eq!(edit(STITCH, &planned(STITCH, &[]), 3), Ok(Edit::Stitch { crossfade: 0 }));
    assert_eq!(edit(STITCH, &planned(STITCH, &[("transition", text("crossfade"))]), 2), Ok(Edit::Stitch { crossfade: 500_000 }));
    assert!(edit(STITCH, &planned(STITCH, &[]), MAX_STITCHED + 1).unwrap_err().contains("at most"));
    assert_eq!(
        edit(REPLACE_AUDIO, &planned(REPLACE_AUDIO, &[("mode", text("mix")), ("volume", n(0.5)), ("offset", n(1.0))]), 2),
        Ok(Edit::ReplaceAudio { keep_own: true, volume: 500, offset: 1_000_000 })
    );
    assert!(edit(REPLACE_AUDIO, &planned(REPLACE_AUDIO, &[]), 1).unwrap_err().contains("a video and a sound"));
    assert!(edit("video.reverse", &ParamValues::new(), 1).is_err());

    // Hostile values a hand-written job could carry.
    for (name, bad) in [("start", n(-1.0)), ("start", n(1e300)), ("end", text("soon"))] {
        let mut params = planned(TRIM, &[]);
        params.insert(name.into(), bad);
        assert!(edit(TRIM, &params, 1).is_err(), "{name}");
    }
    for bad in [n(0.0), n(100.0), n(f64::MAX)] {
        let mut params = planned(SPEED, &[]);
        params.insert("speed".into(), bad);
        assert!(edit(SPEED, &params, 1).is_err());
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_quality_is_read_and_a_job_without_one_renders_at_standard() {
    let mut params = planned(TRIM, &[]);
    assert_eq!(quality(&params), Ok(nodecraft_media::video::Quality::Standard));
    params.remove("quality");
    assert_eq!(quality(&params), Ok(nodecraft_media::video::Quality::Standard));
    params.insert("quality".into(), text("best"));
    assert!(quality(&params).is_err());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_output_keeps_the_first_videos_upright_shape_and_rate() {
    assert_eq!(frame_for(1920, 1080, (1, 1), 0, Some((30000, 1001))), ((1920, 1080), (30000, 1001)));
    // Turned a quarter: portrait. Odd sides round down to even.
    assert_eq!(frame_for(641, 361, (1, 1), 90, Some((25, 1))), ((360, 640), (25, 1)));
    // Wide pixels widen the picture; too large fits within 4096.
    assert_eq!(frame_for(720, 576, (16, 11), 0, Some((50, 2))), ((1046, 576), (25, 1)));
    assert_eq!(frame_for(8192, 2048, (1, 1), 0, None), ((4096, 1024), (30, 1)));
    // Degenerate input still gives a frame the renderer takes.
    assert_eq!(frame_for(0, 0, (0, 0), 270, Some((0, 0))), ((16, 16), (30, 1)));
    assert_eq!(frame_for(1, 100_000, (u32::MAX, 1), 0, Some((u64::MAX, 1))), ((4096, 16), (30, 1)));
}

#[test]
fn a_video_action_without_tools_says_what_to_do() {
    let actions = LocalActions::new(AssetStore::default());
    assert!(actions.long_running(TRIM) && actions.long_running(STITCH));
    let video = Value::Media(AssetRef { hash: Blake3::from_bytes([7; 32]), mime: "video/mp4".parse().unwrap(), meta: MediaMeta::default() });
    let error = actions.execute(TRIM, &planned(TRIM, &[]), &[video]).unwrap_err();
    #[cfg(not(target_arch = "wasm32"))]
    assert!(error.contains("[media]"), "{error}");
    #[cfg(target_arch = "wasm32")]
    assert!(error.contains("desktop"), "{error}");
    let error = actions.execute(REPLACE_AUDIO, &planned(REPLACE_AUDIO, &[]), &[]).unwrap_err();
    assert!(error.contains("a video and a sound"), "{error}");
}

/// A small test-pattern MP4 at 10 frames a second, with a tone when `sound`.
#[cfg(not(target_arch = "wasm32"))]
fn clip(ffmpeg: &std::path::Path, seconds: u32, sound: bool) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.mp4");
    let length = seconds.to_string();
    let mut command = std::process::Command::new(ffmpeg);
    command.args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", "testsrc=size=64x48:rate=10"]);
    if sound {
        command.args(["-f", "lavfi", "-i", "sine=frequency=440", "-c:a", "aac"]);
    }
    let status = command.args(["-t", &length, "-c:v", "libx264", "-pix_fmt", "yuv420p"]).arg(&path).status().unwrap();
    assert!(status.success());
    std::fs::read(path).unwrap()
}

/// Opt in with NODECRAFT_VIDEO_TEST_FFMPEG and NODECRAFT_VIDEO_TEST_FFPROBE (absolute paths).
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "runs a real ffmpeg"]
fn each_action_renders_a_webm_of_the_length_it_says() {
    let ffmpeg = std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let ffprobe = std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("tone.wav");
    let status = std::process::Command::new(&ffmpeg)
        .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", "sine=frequency=1000:duration=1", "-c:a", "pcm_s16le"])
        .arg(&wav)
        .status()
        .unwrap();
    assert!(status.success());
    let mut assets = AssetStore::default();
    let (two, one, silent) =
        (assets.insert(&clip(&ffmpeg, 2, true)).unwrap(), assets.insert(&clip(&ffmpeg, 1, true)).unwrap(), assets.insert(&clip(&ffmpeg, 2, false)).unwrap());
    let tone = assets.insert(&std::fs::read(&wav).unwrap()).unwrap();
    let media = |hash, mime: &str| Value::Media(AssetRef { hash, mime: mime.parse().unwrap(), meta: MediaMeta::default() });
    let video = |hash| media(hash, "video/mp4");
    let actions = LocalActions::new(assets).with_video_tools(Some((ffmpeg, ffprobe)));
    let run = |action: &str, set: &[(&str, Value)], inputs: &[Value]| {
        let out = actions.execute(action, &planned(action, set), inputs).unwrap_or_else(|e| panic!("{action}: {e}"));
        let Value::Media(asset) = out else { panic!("{out:?}") };
        assert_eq!((asset.mime.essence(), asset.meta.width, asset.meta.height), ("video/webm", Some(64), Some(48)), "{action}");
        asset.meta.duration_ms
    };

    assert_eq!(run(TRIM, &[("start", n(0.5)), ("end", n(1.5))], &[video(two)]), Some(1000));
    assert_eq!(run(TRIM, &[("start", n(1.2))], &[video(two)]), Some(800));
    assert_eq!(run(SPEED, &[("speed", n(2.0))], &[video(two)]), Some(1000));
    assert_eq!(run(SPEED, &[("speed", n(0.5)), ("sound", text("mute")), ("quality", text("draft"))], &[video(one)]), Some(2000));
    assert_eq!(run(STITCH, &[], &[video(two), video(one), video(silent)]), Some(5000));
    // 2 s and 1 s, overlapping by 0.5 s.
    assert_eq!(run(STITCH, &[("transition", text("crossfade"))], &[video(two), video(one)]), Some(2500));
    assert_eq!(run(REPLACE_AUDIO, &[], &[video(silent), media(tone, "audio/wav")]), Some(2000));
    assert_eq!(run(REPLACE_AUDIO, &[("mode", text("mix")), ("offset", n(0.5))], &[video(two), media(tone, "audio/wav")]), Some(2000));

    // What can't be done says why, in terms of the settings.
    let error = actions.execute(TRIM, &planned(TRIM, &[("end", n(5.0))]), &[video(one)]).unwrap_err();
    assert!(error.contains("the end (5.0 s) is after the video's (1.0 s)"), "{error}");
    let error = actions.execute(TRIM, &planned(TRIM, &[("start", n(1.5))]), &[video(one)]).unwrap_err();
    assert!(error.contains("at or after the end"), "{error}");
    let error =
        actions.execute(STITCH, &planned(STITCH, &[("transition", text("crossfade")), ("transition_length", n(3.0))]), &[video(two), video(one)]).unwrap_err();
    assert!(error.contains("crossfade is longer"), "{error}");
    let error = actions.execute(REPLACE_AUDIO, &planned(REPLACE_AUDIO, &[("offset", n(9.0))]), &[video(one), media(tone, "audio/wav")]).unwrap_err();
    assert!(error.contains("after the video ends"), "{error}");
    let error = actions.execute(REPLACE_AUDIO, &planned(REPLACE_AUDIO, &[]), &[video(one), video(two)]).unwrap_err();
    assert!(error.contains("connect a sound"), "{error}");
    // The sources are never changed: every output is a new file.
    assert_eq!(actions.staged_videos().len(), 8);
}

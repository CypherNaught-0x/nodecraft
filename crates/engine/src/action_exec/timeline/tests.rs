use super::*;
use crate::action_exec::LocalActions;
use nodecraft_exec::run::ActionExecutor;
use nodecraft_format::AssetStore;
use nodecraft_nodes::Registry;
use nodecraft_nodes::timeline::Input;
use nodecraft_types::{AssetRef, Blake3, MediaMeta, NodeId, Unsigned};

/// A Timeline node's planned parameters, with `clips` stored and one source per input.
fn planned(clips: &str, sources: &[&str]) -> ParamValues {
    let kinds = Registry::builtins().unwrap();
    let kind = kinds.get(nodecraft_nodes::timeline::ID).unwrap();
    let node = kind.instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &Default::default()).unwrap();
    let mut params = kind.validate_instance(&node, true).unwrap().into_visible_values();
    params.insert(nodecraft_nodes::timeline::SPEC_PARAM.into(), Value::Text(clips.into()));
    params.insert(nodecraft_nodes::timeline::SOURCES_PARAM.into(), Value::List(sources.iter().map(|s| Value::Text((*s).into())).collect()));
    params
}

/// [`planned`] with each input's port.
fn planned_on(clips: &str, wires: &[(&str, &str)]) -> ParamValues {
    let mut params = planned(clips, &wires.iter().map(|(_, origin)| *origin).collect::<Vec<_>>());
    params.insert(nodecraft_nodes::timeline::PORTS_PARAM.into(), Value::List(wires.iter().map(|(port, _)| Value::Text((*port).into())).collect()));
    params
}

#[test]
fn the_frame_comes_from_the_parameters_and_is_checked() {
    let mut params = planned(nodecraft_nodes::timeline::EMPTY, &[]);
    assert_eq!(frame(&params).unwrap(), Frame { width: 1280, height: 720, fps: (30, 1), background: [0, 0, 0] });
    params.insert("background".into(), Value::Text("#ff8000".into()));
    params.insert("fps".into(), Value::Text("25".into()));
    assert_eq!(frame(&params).map(|f| (f.fps, f.background)).unwrap(), ((25, 1), [255, 128, 0]));
    for (name, bad) in [
        ("width", Value::Unsigned(Unsigned(641))),
        ("width", Value::Unsigned(Unsigned(8))),
        ("height", Value::Unsigned(Unsigned(8192))),
        ("fps", Value::Text("29.97".into())),
        ("background", Value::Text("red".into())),
    ] {
        let mut params = params.clone();
        params.insert(name.into(), bad);
        assert!(frame(&params).is_err(), "{name}");
    }
}

#[test]
fn sources_and_clips_are_read_from_the_plan() {
    let mut params = planned(r#"{"clips":[{"id":"a","origin":"n1","out_us":1000000}]}"#, &["n1", "n2"]);
    // A job planned before #576 carries no ports: every input is a video.
    assert_eq!(wires(&params, 3), [Wire::video("n1"), Wire::video("n2"), Wire::video("")]);
    let ports = [nodecraft_nodes::timeline::AUDIO_INPUT, "elsewhere"].map(|p| Value::Text(p.into()));
    params.insert(nodecraft_nodes::timeline::PORTS_PARAM.into(), Value::List(ports.into()));
    assert_eq!(wires(&params, 2), [Wire::new(Input::Audio, "n1"), Wire::video("n2")]);
    assert_eq!(clips(&params).unwrap().clips.len(), 1);
    assert!(clips(&planned("{", &[])).is_err());
    let mut params = ParamValues::new();
    assert!(wires(&params, 0).is_empty() && clips(&params).unwrap().clips.is_empty());
    params.insert(nodecraft_nodes::timeline::SPEC_PARAM.into(), Value::Bool(true));
    assert!(clips(&params).is_err());
}

#[test]
fn a_timeline_without_tools_or_clips_says_what_to_do() {
    let actions = LocalActions::new(AssetStore::default());
    let video = Value::Media(AssetRef { hash: Blake3::from_bytes([7; 32]), mime: "video/mp4".parse().unwrap(), meta: MediaMeta::default() });
    let empty = actions.execute(nodecraft_nodes::timeline::ID, &planned(nodecraft_nodes::timeline::EMPTY, &[]), &[]).unwrap_err();
    assert!(empty.contains("no clips"), "{empty}");
    let disabled = r#"{"clips":[{"id":"n1-0","origin":"n1","enabled":false}]}"#;
    let off = actions.execute(nodecraft_nodes::timeline::ID, &planned(disabled, &["n1"]), std::slice::from_ref(&video)).unwrap_err();
    assert!(off.contains("no clips"), "{off}");
    let untooled = actions.execute(nodecraft_nodes::timeline::ID, &planned(nodecraft_nodes::timeline::EMPTY, &["n1"]), &[video]).unwrap_err();
    #[cfg(not(target_arch = "wasm32"))]
    assert!(untooled.contains("[media]"), "{untooled}");
    #[cfg(target_arch = "wasm32")]
    assert!(untooled.contains("desktop"), "{untooled}");
    // A sound alone has no picture to play under.
    let sound = Value::Media(AssetRef { hash: Blake3::from_bytes([8; 32]), mime: "audio/wav".parse().unwrap(), meta: MediaMeta::default() });
    let params = planned_on(nodecraft_nodes::timeline::EMPTY, &[(nodecraft_nodes::timeline::AUDIO_INPUT, "n1")]);
    let silent = actions.execute(nodecraft_nodes::timeline::ID, &params, &[sound]).unwrap_err();
    assert!(silent.contains("main track"), "{silent}");
    assert!(actions.staged_videos().is_empty());
}

/// A small test-pattern MP4 with a tone, made by the ffmpeg under test.
#[cfg(not(target_arch = "wasm32"))]
fn clip(ffmpeg: &std::path::Path, seconds: u32) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.mp4");
    let length = seconds.to_string();
    let status = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", "testsrc=size=64x48:rate=10", "-f", "lavfi", "-i", "sine=frequency=440"])
        .args(["-t", &length, "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(path).unwrap()
}

/// Opt in with NODECRAFT_VIDEO_TEST_FFMPEG and NODECRAFT_VIDEO_TEST_FFPROBE (absolute paths).
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "runs a real ffmpeg"]
fn a_timeline_renders_its_clips_into_one_webm() {
    let ffmpeg = std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let ffprobe = std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap());
    let mut assets = AssetStore::default();
    let (one, two) = (assets.insert(&clip(&ffmpeg, 2)).unwrap(), assets.insert(&clip(&ffmpeg, 1)).unwrap());
    let video = |hash| Value::Media(AssetRef { hash, mime: "video/mp4".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets).with_video_tools(Some((ffmpeg, ffprobe)));
    let clips = r#"{"clips":[{"id":"a","origin":"n1","in_us":500000,"fade_in_us":200000,"transition_us":300000},{"id":"b","origin":"n2","volume":500}]}"#;
    let mut params = planned(clips, &["n1", "n2"]);
    params.insert("width".into(), Value::Unsigned(Unsigned(64)));
    params.insert("height".into(), Value::Unsigned(Unsigned(48)));
    let out = actions.execute(nodecraft_nodes::timeline::ID, &params, &[video(one), video(two)]).unwrap();
    let Value::Media(asset) = out else { panic!("{out:?}") };
    assert_eq!((asset.mime.essence(), asset.meta.width, asset.meta.height), ("video/webm", Some(64), Some(48)));
    // 1.5 s and 1 s, overlapping by 0.3 s.
    assert_eq!(asset.meta.duration_ms, Some(2200));
    let staged = actions.staged_videos();
    let [(hash, bytes)] = staged.as_slice() else { panic!("{} staged", staged.len()) };
    assert_eq!(*hash, asset.hash);
    assert_eq!(bytes.get(..4), Some([0x1a, 0x45, 0xdf, 0xa3].as_slice()), "a Matroska/WebM file");

    // A clip past its video's end says which clip and how long the video is.
    let long = r#"{"clips":[{"id":"n2-0","origin":"n2","out_us":5000000}]}"#;
    let error = actions.execute(nodecraft_nodes::timeline::ID, &planned(long, &["n1", "n2"]), &[video(one), video(two)]).unwrap_err();
    assert!(error.contains("clip 1 ends after its video does (1.0 s)"), "{error}");
}

/// Opt in with NODECRAFT_VIDEO_TEST_FFMPEG and NODECRAFT_VIDEO_TEST_FFPROBE (absolute paths).
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "runs a real ffmpeg"]
fn sounds_and_pictures_play_on_their_own_tracks() {
    use nodecraft_nodes::timeline::{AUDIO_INPUT, INPUT, STILLS_INPUT};
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
    let png = nodecraft_media::encode_png(8, 6, &[255; 8 * 6 * 4], &Default::default()).unwrap();
    let mut assets = AssetStore::default();
    let (film, tone, logo) = (assets.insert(&clip(&ffmpeg, 2)).unwrap(), assets.insert(&std::fs::read(wav).unwrap()).unwrap(), assets.insert(&png).unwrap());
    let clear = assets.insert(&nodecraft_media::encode_png(8, 6, &[0; 8 * 6 * 4], &Default::default()).unwrap()).unwrap();
    let media = |hash, mime: &str| Value::Media(AssetRef { hash, mime: mime.parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets).with_video_tools(Some((ffmpeg, ffprobe)));
    // The sound starts at 1.5 s, past the main track's 2 s, so the output runs to 2.5 s.
    let clips = r#"{"clips":[{"id":"logo","origin":"n3","input":"stills","track":2,"at_us":500000,"out_us":1000000,"fade_in_us":200000}],"tracks":[{"kind":"audio"},{"kind":"video"}]}"#;
    let mut params = planned_on(clips, &[(INPUT, "n1"), (AUDIO_INPUT, "n2"), (STILLS_INPUT, "n3")]);
    params.insert("width".into(), Value::Unsigned(Unsigned(64)));
    params.insert("height".into(), Value::Unsigned(Unsigned(48)));
    let mut stored = super::clips(&params).unwrap();
    stored
        .clips
        .push(nodecraft_nodes::timeline::Clip { at_us: 1_500_000, ..nodecraft_nodes::timeline::Clip::wired("tone".into(), Input::Audio, "n2".into(), 0, 1) });
    params.insert(nodecraft_nodes::timeline::SPEC_PARAM.into(), Value::Text(stored.encode().unwrap()));
    let inputs = [media(film, "video/mp4"), media(tone, "audio/wav"), media(logo, "image/png")];
    let out = actions.execute(nodecraft_nodes::timeline::ID, &params, &inputs).unwrap();
    let Value::Media(asset) = out else { panic!("{out:?}") };
    assert_eq!((asset.mime.essence(), asset.meta.duration_ms), ("video/webm", Some(2500)));

    // A picture wired into the clips input isn't a video.
    let wrong = planned_on(nodecraft_nodes::timeline::EMPTY, &[(INPUT, "n3")]);
    let error = actions.execute(nodecraft_nodes::timeline::ID, &wrong, &[media(logo, "image/png")]).unwrap_err();
    assert!(error.contains("clips input"), "{error}");

    // A see-through picture alone on the main track renders over the background.
    let alone = planned_on(nodecraft_nodes::timeline::EMPTY, &[(STILLS_INPUT, "n3")]);
    let out = actions.execute(nodecraft_nodes::timeline::ID, &alone, &[media(clear, "image/png")]).unwrap();
    let Value::Media(asset) = out else { panic!("{out:?}") };
    assert_eq!(asset.meta.duration_ms, Some(3000));
    // A picture that isn't there says so about the picture, not an "input image".
    let missing = media(Blake3::from_bytes([9; 32]), "image/png");
    let error = actions.execute(nodecraft_nodes::timeline::ID, &alone, &[missing]).unwrap_err();
    assert!(error.contains("a connected picture"), "{error}");
}

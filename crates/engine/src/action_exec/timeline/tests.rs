use super::*;
use crate::action_exec::LocalActions;
use nodecraft_exec::run::ActionExecutor;
use nodecraft_format::AssetStore;
use nodecraft_nodes::Registry;
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
    let params = planned(r#"{"clips":[{"id":"a","origin":"n1","out_us":1000000}]}"#, &["n1", "n2"]);
    assert_eq!(sources(&params), ["n1", "n2"]);
    assert_eq!(clips(&params).unwrap().clips.len(), 1);
    assert!(clips(&planned("{", &[])).is_err());
    let mut params = ParamValues::new();
    assert!(sources(&params).is_empty() && clips(&params).unwrap().clips.is_empty());
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

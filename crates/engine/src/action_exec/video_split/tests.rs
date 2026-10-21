use super::*;
use crate::action_exec::LocalActions;
use nodecraft_exec::run::ActionExecutor;
use nodecraft_format::AssetStore;
use nodecraft_nodes::Registry;
use nodecraft_types::{AssetRef, MediaMeta, NodeId, Number, Unsigned, Value};

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

fn u(value: u64) -> Value {
    Value::Unsigned(Unsigned(value))
}

fn text(value: &str) -> Value {
    Value::Text(value.into())
}

#[test]
fn settings_are_read_and_checked_before_any_video_is_imported() {
    assert_eq!(job(SPLIT, &planned(SPLIT, &[]), 1), Ok(Job::Split(Split::Parts(2))));
    assert_eq!(job(SPLIT, &planned(SPLIT, &[("parts", u(5))]), 1), Ok(Job::Split(Split::Parts(5))));
    assert_eq!(job(SPLIT, &planned(SPLIT, &[("by", text("length")), ("length", n(2.5))]), 1), Ok(Job::Split(Split::Length(2_500_000))));
    assert_eq!(job(SPLIT, &planned(SPLIT, &[("by", text("scenes"))]), 1), Ok(Job::Split(Split::Scenes { threshold: 300, min_us: 1_000_000 })));
    assert_eq!(job(EXTRACT_AUDIO, &planned(EXTRACT_AUDIO, &[]), 1), Ok(Job::ExtractAudio));
    assert!(job(SPLIT, &planned(SPLIT, &[]), 0).unwrap_err().contains("connect a video"));
    assert!(job(EXTRACT_AUDIO, &planned(EXTRACT_AUDIO, &[]), 0).unwrap_err().contains("connect a video"));
    assert!(job("video.trim", &ParamValues::new(), 1).is_err());

    // Hostile values a hand-written job could carry.
    let with = |name: &str, value: Value| {
        let mut params = planned(
            SPLIT,
            &[(
                "by",
                text(if name == "parts" {
                    "parts"
                } else if name == "length" {
                    "length"
                } else {
                    "scenes"
                }),
            )],
        );
        params.insert(name.into(), value);
        params
    };
    for (name, bad) in [
        ("parts", u(1)),
        ("parts", u(MAX_PARTS + 1)),
        ("parts", u(u64::MAX)),
        ("parts", n(3.0)),
        ("length", n(0.0)),
        ("length", n(-1.0)),
        ("length", n(1e300)),
        ("threshold", n(0.01)),
        ("threshold", n(1.5)),
        ("min_length", n(-0.1)),
        ("min_length", n(601.0)),
        ("by", text("halves")),
        ("by", u(2)),
    ] {
        assert!(job(SPLIT, &with(name, bad.clone()), 1).is_err(), "{name} = {bad:?}");
    }
    let mut params = planned(SPLIT, &[]);
    params.remove("by");
    assert!(job(SPLIT, &params, 1).is_err());
}

const FPS30: (u32, u32) = (30, 1);
/// NTSC: a frame is 33366.67 µs, never a whole microsecond.
const NTSC: (u32, u32) = (30_000, 1001);

fn lengths(spans: &[(u64, u64)]) -> Vec<u64> {
    spans.iter().map(|(a, b)| b - a).collect()
}

/// The spans start at 0, follow on without a gap and end at the video's end.
fn contiguous(spans: &[(u64, u64)], length: u64) {
    assert_eq!(spans.first().map(|s| s.0), Some(0));
    assert_eq!(spans.last().map(|s| s.1), Some(length));
    for pair in spans.windows(2) {
        assert_eq!(pair[0].1, pair[1].0);
        assert!(pair[0].0 < pair[0].1);
    }
}

#[test]
fn equal_parts_cut_on_frame_starts() {
    let s = spans(Split::Parts(3), 3_000_000, FPS30, &[]).unwrap();
    assert_eq!(s, vec![(0, 1_000_000), (1_000_000, 2_000_000), (2_000_000, 3_000_000)]);
    // 10 frames into 3: 3, 3 and 4 frames.
    let s = spans(Split::Parts(3), 1_000_000, (10, 1), &[]).unwrap();
    assert_eq!(s, vec![(0, 300_000), (300_000, 600_000), (600_000, 1_000_000)]);
    // At NTSC every cut is a frame start rounded up, so no part takes a sliver of the frame before.
    let length = 10_010_000; // 300 frames
    let s = spans(Split::Parts(4), length, NTSC, &[]).unwrap();
    contiguous(&s, length);
    assert_eq!(s.iter().map(|s| s.0).collect::<Vec<_>>(), vec![0, 2_502_500, 5_005_000, 7_507_500]);
    let s = spans(Split::Parts(7), length, NTSC, &[]).unwrap();
    contiguous(&s, length);
    for (start, _) in &s {
        // The start is a frame's exact start, rounded up to the microsecond.
        assert_eq!(frame_at(nearest_frame(*start, NTSC), NTSC), *start);
    }
}

#[test]
fn equal_parts_need_two_frames_each() {
    assert!(spans(Split::Parts(5), 1_000_000, (10, 1), &[]).is_ok());
    let error = spans(Split::Parts(6), 1_000_000, (10, 1), &[]).unwrap_err();
    assert!(error.contains("10 frames") && error.contains("at least 12"), "{error}");
}

#[test]
fn parts_by_length_give_the_last_part_what_is_left() {
    let s = spans(Split::Length(1_000_000), 3_500_000, FPS30, &[]).unwrap();
    assert_eq!(lengths(&s), vec![1_000_000, 1_000_000, 1_000_000, 500_000]);
    // A whole number of parts leaves no empty part at the end.
    let s = spans(Split::Length(1_000_000), 3_000_000, FPS30, &[]).unwrap();
    assert_eq!(lengths(&s), vec![1_000_000; 3]);
    // A part longer than the video is the whole video.
    assert_eq!(spans(Split::Length(10_000_000), 3_000_000, FPS30, &[]).unwrap(), vec![(0, 3_000_000)]);
    // A remainder of one frame joins the part before.
    let s = spans(Split::Length(1_000_000), 2_033_334, FPS30, &[]).unwrap();
    assert_eq!(s, vec![(0, 1_000_000), (1_000_000, 2_033_334)]);
    // Two frames left is a part of its own.
    let s = spans(Split::Length(1_000_000), 2_066_667, FPS30, &[]).unwrap();
    assert_eq!(lengths(&s), vec![1_000_000, 1_000_000, 66_667]);
}

#[test]
fn fractional_lengths_and_rates_never_drift() {
    // 1.5 s parts of a 30 s NTSC video: each cut from the start, nearest frame.
    let length = 30_030_000; // 900 frames
    let s = spans(Split::Length(1_500_000), length, NTSC, &[]).unwrap();
    contiguous(&s, length);
    // Twenty parts and a last frame (30 s to 30.03 s), which joins the twentieth.
    assert_eq!(s.len(), 20);
    assert_eq!(s.last().map(|(a, b)| b - a), Some(1_534_866));
    for (k, (start, _)) in s.iter().enumerate().skip(1) {
        assert!(start.abs_diff(k as u64 * 1_500_000) <= 16_684, "cut {k} at {start}");
    }
    // 0.25 s at 30 a second is 7.5 frames: the cuts alternate 7 and 8 frames, never adding up.
    let s = spans(Split::Length(250_000), 2_000_000, FPS30, &[]).unwrap();
    assert_eq!(s.len(), 8);
    assert!(s.iter().all(|(a, b)| (233_333..=266_667).contains(&(b - a))), "{s:?}");
}

#[test]
fn too_many_or_too_short_parts_are_refused_with_a_way_out() {
    let error = spans(Split::Length(100_000), 3_600_000_000, FPS30, &[]).unwrap_err();
    assert!(error.contains("at most 32") && error.contains("longer"), "{error}");
    // 33 parts whose last is one frame merge to 32.
    let s = spans(Split::Length(1_000_000), 32_033_334, FPS30, &[]).unwrap();
    assert_eq!(s.len(), 32);
    assert!(spans(Split::Length(1_000_000), 32_066_667, FPS30, &[]).unwrap_err().contains("33 parts"));
    // A part a frame long can't render reliably.
    let error = spans(Split::Length(40_000), 1_000_000, FPS30, &[]).unwrap_err();
    assert!(error.contains("at least 2 frames"), "{error}");
}

#[test]
fn scene_splits_cut_at_changes_but_not_too_close() {
    let length = 10_000_000;
    let scenes = |changes: &[u64], min_us: u64| spans(Split::Scenes { threshold: 300, min_us }, length, FPS30, changes);
    assert_eq!(scenes(&[], 1_000_000).unwrap(), vec![(0, length)]);
    assert_eq!(scenes(&[4_000_000], 1_000_000).unwrap(), vec![(0, 4_000_000), (4_000_000, length)]);
    // A change reported a microsecond off the frame cuts on the frame.
    assert_eq!(scenes(&[4_000_001], 0).unwrap(), vec![(0, 4_000_000), (4_000_000, length)]);
    // Too close to the start, the change before, or the end: skipped.
    assert_eq!(lengths(&scenes(&[500_000, 3_000_000, 3_400_000, 9_600_000], 1_000_000).unwrap()), vec![3_000_000, 7_000_000]);
    // With no shortest part, still two frames a part.
    assert_eq!(lengths(&scenes(&[33_334, 1_000_000, 1_033_334, 9_966_667], 0).unwrap()), vec![1_000_000, 9_000_000]);
    let many: Vec<u64> = (1..40).map(|k| k * 200_000).collect();
    let error = scenes(&many, 0).unwrap_err();
    assert!(error.contains("40 scenes") && error.contains("raise the threshold"), "{error}");
    // A longer shortest part brings the same changes under the most.
    assert_eq!(scenes(&many, 400_000).unwrap().len(), 20);
}

#[test]
fn hostile_lengths_and_rates_do_not_overflow() {
    for (length, fps) in [(u64::MAX, (1, 1)), (u64::MAX, (240, 1)), (1, (1, 1)), (0, FPS30), (3_600_000_000, (u32::MAX, 1))] {
        for split in [Split::Parts(2), Split::Length(100_000), Split::Length(u64::MAX), Split::Scenes { threshold: 300, min_us: 0 }] {
            let _ = spans(split, length, fps, &[0, 1, u64::MAX / 2, u64::MAX]);
        }
    }
}

#[test]
#[ignore = "runs a real ffmpeg"]
fn split_parts_add_up_and_extract_writes_the_sound() {
    let ffmpeg = std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let ffprobe = std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.mp4");
    // 3 s at 10 a second: red, then blue from 1.2 s, with a tone throughout.
    let graph = "color=c=red:s=64x48:r=10:d=1.2[a];color=c=blue:s=64x48:r=10:d=1.8[b];[a][b]concat=n=2:v=1:a=0[v];sine=frequency=440:duration=3[s]";
    let status = std::process::Command::new(&ffmpeg)
        .args([
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-filter_complex",
            graph,
            "-map",
            "[v]",
            "-map",
            "[s]",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let silent = dir.path().join("silent.mp4");
    let status = std::process::Command::new(&ffmpeg)
        .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", "testsrc=size=64x48:rate=10", "-t", "1", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&silent)
        .status()
        .unwrap();
    assert!(status.success());
    let mut assets = AssetStore::default();
    let clip = assets.insert(&std::fs::read(&path).unwrap()).unwrap();
    let quiet = assets.insert(&std::fs::read(&silent).unwrap()).unwrap();
    let video = Value::Media(AssetRef { hash: clip, mime: "video/mp4".parse().unwrap(), meta: MediaMeta::default() });
    let quiet = Value::Media(AssetRef { hash: quiet, mime: "video/mp4".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets).with_video_tools(Some((ffmpeg, ffprobe.clone())));
    let run = |action: &str, set: &[(&str, Value)], input: &Value| actions.execute(action, &planned(action, set), std::slice::from_ref(input));
    let durations = |value: Value| match value {
        Value::List(parts) => parts
            .iter()
            .map(|part| match part {
                Value::Media(asset) => {
                    assert_eq!((asset.mime.essence(), asset.meta.width, asset.meta.sample_rate), ("video/webm", Some(64), Some(48_000)));
                    asset.meta.duration_ms.unwrap()
                }
                other => panic!("{other:?}"),
            })
            .collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };

    let equal = durations(run(SPLIT, &[("parts", u(3))], &video).unwrap());
    assert_eq!(equal, vec![1000, 1000, 1000]);
    let by_length = durations(run(SPLIT, &[("by", text("length")), ("length", n(1.4))], &video).unwrap());
    assert_eq!(by_length, vec![1400, 1400, 200]);
    let by_scene = durations(run(SPLIT, &[("by", text("scenes")), ("min_length", n(0.5))], &video).unwrap());
    assert_eq!(by_scene, vec![1200, 1800], "the hard cut at 1.2 s");
    // Every split's parts add up to the source.
    for parts in [&equal, &by_length, &by_scene] {
        assert_eq!(parts.iter().sum::<u64>(), 3000);
    }
    // The parts are staged for the run to import, each with its sound.
    let staged = actions.staged_videos();
    let part = staged.iter().find(|(_, bytes)| bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3])).unwrap();
    let file = dir.path().join("part.webm");
    std::fs::write(&file, &part.1).unwrap();
    let probe = std::process::Command::new(&ffprobe).args(["-v", "error", "-show_entries", "stream=codec_type", "-of", "csv=p=0"]).arg(&file).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&probe.stdout).split_whitespace().collect::<Vec<_>>(), vec!["video", "audio"]);

    let Value::Media(sound) = run(EXTRACT_AUDIO, &[], &video).unwrap() else { panic!("a sound") };
    assert_eq!((sound.mime.essence(), sound.meta.sample_rate, sound.meta.channels), ("audio/ogg", Some(48_000), Some(2)));
    assert!(sound.meta.duration_ms.unwrap().abs_diff(3000) < 60, "{:?}", sound.meta.duration_ms);
    let bytes = actions.staged_videos().into_iter().find(|(hash, _)| *hash == sound.hash).unwrap().1;
    let file = dir.path().join("sound.ogg");
    std::fs::write(&file, &bytes).unwrap();
    let probe = std::process::Command::new(&ffprobe)
        .args(["-v", "error", "-show_entries", "stream=codec_name:format=format_name,duration", "-of", "default=nw=1"])
        .arg(&file)
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&probe.stdout);
    assert!(report.contains("codec_name=opus") && report.contains("format_name=ogg"), "{report}");
    assert_eq!(run(EXTRACT_AUDIO, &[], &quiet), Err("the video has no sound".to_owned()));
}

#[test]
fn parts_by_length_are_never_shorter_than_two_frames() {
    // 1.5 frames rounds to 2 but cuts would alternate 1 and 2 frames apart: refused up front.
    let error = spans(Split::Length(150_000), 3_000_000, (10, 1), &[]).unwrap_err();
    assert!(error.contains("at least 2 frames"), "{error}");
    // The slider's shortest length at 18 a second is under two frames too.
    assert!(spans(Split::Length(100_000), 3_000_000, (18, 1), &[]).is_err());
    // From two whole frames up, every part has at least two.
    for (length, fps) in [(200_000, (10, 1)), (220_000, (10, 1)), (111_112, (18, 1)), (66_734, NTSC)] {
        let s = spans(Split::Length(length), 1_000_000, fps, &[]).unwrap();
        contiguous(&s, 1_000_000);
        for (start, end) in &s {
            assert!(nearest_frame(*end, fps) - nearest_frame(*start, fps) >= 2, "{length} at {fps:?}: {s:?}");
        }
    }
}

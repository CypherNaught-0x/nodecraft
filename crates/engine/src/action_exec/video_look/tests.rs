use super::*;
use crate::action_exec::LocalActions;
use nodecraft_exec::run::ActionExecutor;
use nodecraft_format::AssetStore;
use nodecraft_nodes::Registry;
use nodecraft_types::{AssetRef, Blake3, MediaMeta, NodeId, Number, Unsigned};

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
    assert_eq!(job(GRADE, &planned(GRADE, &[]), 1), Ok(Job::Grade { brightness: 0, contrast: 1000, saturation: 1000, gamma: 1000 }));
    assert_eq!(
        job(GRADE, &planned(GRADE, &[("brightness", n(-0.25)), ("saturation", n(0.0)), ("gamma", n(2.5))]), 1),
        Ok(Job::Grade { brightness: -250, contrast: 1000, saturation: 0, gamma: 2500 })
    );
    assert_eq!(job(FILTER, &planned(FILTER, &[]), 1), Ok(Job::Filter(Effect::Grayscale)));
    assert_eq!(job(FILTER, &planned(FILTER, &[("effect", text("blur"))]), 1), Ok(Job::Filter(Effect::Blur(4000))));
    assert_eq!(job(FILTER, &planned(FILTER, &[("effect", text("vignette")), ("vignette", n(0.25))]), 1), Ok(Job::Filter(Effect::Vignette(250))));
    assert_eq!(job(KEY, &planned(KEY, &[]), 1), Ok(Job::Key { color: [0, 255, 0], similarity: 150, blend: 50, behind: [0, 0, 0] }));
    let mark = Mark::Text { text: "NodeCraft".into(), size: 50, color: [255, 255, 255, 255] };
    assert_eq!(job(WATERMARK, &planned(WATERMARK, &[]), 1), Ok(Job::Watermark { mark, spot: (2, 2), margin: 40, opacity: 700 }));
    let image = planned(WATERMARK, &[("source", text("image")), ("position", text("top")), ("scale", n(12.5))]);
    assert_eq!(job(WATERMARK, &image, 2), Ok(Job::Watermark { mark: Mark::Image { width: 125 }, spot: (1, 0), margin: 40, opacity: 700 }));
    assert!(job(WATERMARK, &image, 1).unwrap_err().contains("mark input"));
    assert!(job(WATERMARK, &planned(WATERMARK, &[("mark_text", text("  "))]), 1).unwrap_err().contains("text"));
    assert_eq!(job(DUPLICATE, &planned(DUPLICATE, &[]), 1), Ok(Job::Duplicate(2)));
    for id in [GRADE, FILTER, KEY, WATERMARK, DUPLICATE] {
        assert!(job(id, &planned(id, &[]), 0).unwrap_err().contains("connect a video"), "{id}");
    }
    assert!(job("video.trim", &ParamValues::new(), 1).is_err());

    // Hostile values a hand-written job could carry.
    for (action, name, bad) in [
        (GRADE, "brightness", n(1.5)),
        (GRADE, "contrast", n(-1.0)),
        (GRADE, "gamma", n(0.0)),
        (GRADE, "saturation", n(1e300)),
        (FILTER, "effect", text("glow")),
        (KEY, "key", text("green")),
        (KEY, "similarity", n(0.0)),
        (KEY, "blend", n(2.0)),
        (WATERMARK, "source", text("logo")),
        (WATERMARK, "position", text("middle")),
        (WATERMARK, "margin", n(-1.0)),
        (WATERMARK, "opacity", n(1.5)),
        (WATERMARK, "text_size", n(1e9)),
        (WATERMARK, "mark_text", text(&"x".repeat(5000))),
        (DUPLICATE, "copies", Value::Unsigned(Unsigned(u64::MAX))),
        (DUPLICATE, "copies", Value::Unsigned(Unsigned(1))),
    ] {
        let mut params = planned(action, &[]);
        params.insert(name.into(), bad);
        assert!(job(action, &params, 2).is_err(), "{action} {name}");
        params.remove(name);
        assert!(job(action, &params, 2).is_err(), "{action} without {name}");
    }
}

#[test]
fn a_mark_sits_where_it_says_inside_the_frame() {
    // A 100-wide picture fits a 1000×500 frame at 5× its size, so a fifth of the width is 400.
    assert_eq!(placed((100, 100), (1000, 500), 200, (0, 0), 20), (120, 240, 400));
    assert_eq!(placed((100, 100), (1000, 500), 200, (1, 1), 20), (500, 500, 400));
    assert_eq!(placed((100, 100), (1000, 500), 200, (2, 2), 20), (880, 760, 400));
    // Degenerate shapes stay in the compositor's bounds.
    let (x, y, scale) = placed((0, 0), (0, 0), 1000, (2, 2), u32::MAX);
    assert!((-1000..=2000).contains(&x) && (-1000..=2000).contains(&y) && (10..=4000).contains(&scale));
    assert_eq!(corner(0, 30, 100, 5), 4);
    assert_eq!(corner(1, 30, 100, 5), 34);
    assert_eq!(corner(2, 30, 100, 5), 64);
    assert_eq!(corner(2, 200, 100, 5), 0);
    assert_eq!(corner(0, 30, 100, 500), 70);
}

#[test]
fn duplicate_repeats_the_reference_and_the_others_need_the_tools() {
    let actions = LocalActions::new(AssetStore::default());
    for id in nodecraft_nodes::video_look::IDS {
        assert!(actions.long_running(id), "{id}");
    }
    assert!(!actions.long_running(DUPLICATE));
    let video = Value::Media(AssetRef { hash: Blake3::from_bytes([7; 32]), mime: "video/mp4".parse().unwrap(), meta: MediaMeta::default() });
    let copies = actions.execute(DUPLICATE, &planned(DUPLICATE, &[("copies", Value::Unsigned(Unsigned(3)))]), std::slice::from_ref(&video)).unwrap();
    assert_eq!(copies, Value::List(vec![video.clone(); 3]));
    let error = actions.execute(GRADE, &planned(GRADE, &[]), &[video]).unwrap_err();
    #[cfg(not(target_arch = "wasm32"))]
    assert!(error.contains("[media]"), "{error}");
    #[cfg(target_arch = "wasm32")]
    assert!(error.contains("desktop"), "{error}");
    let error = actions.execute(KEY, &planned(KEY, &[]), &[]).unwrap_err();
    assert!(error.contains("connect a video"), "{error}");
}

/// Opt in with NODECRAFT_VIDEO_TEST_FFMPEG and NODECRAFT_VIDEO_TEST_FFPROBE (absolute paths).
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "runs a real ffmpeg"]
fn each_look_changes_the_picture_as_it_says() {
    let ffmpeg = std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let ffprobe = std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let mut assets = AssetStore::default();
    let mut clip = |color: &str| {
        let path = dir.path().join(format!("{color}.mp4"));
        let source = format!("color=c={color}:size=64x48:rate=10");
        let status = std::process::Command::new(&ffmpeg)
            .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", &source, "-t", "1", "-c:v", "libx264", "-pix_fmt", "yuv444p", "-crf", "0"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        let hash = assets.insert(&std::fs::read(&path).unwrap()).unwrap();
        Value::Media(AssetRef { hash, mime: "video/mp4".parse().unwrap(), meta: MediaMeta::default() })
    };
    let (red, green) = (clip("red"), clip("lime"));
    let png = nodecraft_media::encode_png(40, 30, &[255, 160, 0, 255].repeat(40 * 30), &nodecraft_media::Limits::default()).unwrap();
    let hash = assets.insert(&png).unwrap();
    let orange = Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets).with_video_tools(Some((ffmpeg, ffprobe)));
    let run = |action: &str, set: &[(&str, Value)], inputs: &[Value]| actions.execute(action, &planned(action, set), inputs).unwrap();
    // The output's middle frame, as RGBA.
    let frame = |video: &Value| {
        match video {
            Value::Media(asset) => assert_eq!((asset.mime.essence(), asset.meta.width, asset.meta.height), ("video/webm", Some(64), Some(48))),
            other => panic!("{other:?}"),
        }
        let frames = actions.execute(nodecraft_nodes::video_frames::FRAMES, &planned_frames(), std::slice::from_ref(video)).unwrap();
        let Value::List(frames) = frames else { panic!("frames") };
        actions.raster(frames.first()).unwrap()
    };
    let at = |picture: &nodecraft_actions::image::Raster, x: u32, y: u32| {
        let i = ((y * picture.width() + x) * 4) as usize;
        let p = picture.pixels();
        [p[i], p[i + 1], p[i + 2]]
    };
    let near = |got: [u8; 3], want: [u8; 3]| got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 40);

    let dark = frame(&run(GRADE, &[("brightness", n(-1.0))], std::slice::from_ref(&red)));
    assert!(near(at(&dark, 32, 24), [0, 0, 0]), "{:?}", at(&dark, 32, 24));
    let grey = frame(&run(FILTER, &[], std::slice::from_ref(&red)));
    let [r, g, b] = at(&grey, 32, 24);
    assert!(r.abs_diff(g) < 12 && g.abs_diff(b) < 12, "{r} {g} {b}");
    let negative = frame(&run(FILTER, &[("effect", text("invert"))], std::slice::from_ref(&red)));
    assert!(near(at(&negative, 32, 24), [0, 255, 255]), "{:?}", at(&negative, 32, 24));

    let keyed = frame(&run(KEY, &[], &[green.clone(), orange.clone()]));
    assert!(near(at(&keyed, 32, 24), [255, 160, 0]), "{:?}", at(&keyed, 32, 24));
    let plain = frame(&run(KEY, &[("background_color", text("#0000ffff"))], std::slice::from_ref(&green)));
    assert!(near(at(&plain, 32, 24), [0, 0, 255]), "{:?}", at(&plain, 32, 24));

    let set = [("source", text("image")), ("position", text("top_left")), ("scale", n(50.0)), ("margin", n(0.0)), ("opacity", n(1.0))];
    let marked = frame(&run(WATERMARK, &set, &[red.clone(), orange]));
    assert!(near(at(&marked, 6, 6), [255, 160, 0]), "{:?}", at(&marked, 6, 6));
    assert!(near(at(&marked, 56, 40), [255, 0, 0]), "{:?}", at(&marked, 56, 40));
    let set = [("mark_text", text("NC")), ("text_size", n(30.0)), ("opacity", n(1.0)), ("text_color", text("#00ff00ff"))];
    let lettered = frame(&run(WATERMARK, &set, std::slice::from_ref(&red)));
    let greenest = (32..64).flat_map(|x| (24..48).map(move |y| (x, y))).map(|(x, y)| at(&lettered, x, y)[1]).max().unwrap();
    assert!(greenest > 150, "{greenest}");
    assert!(near(at(&lettered, 4, 4), [255, 0, 0]), "{:?}", at(&lettered, 4, 4));
}

#[cfg(not(target_arch = "wasm32"))]
fn planned_frames() -> ParamValues {
    use nodecraft_nodes::video_frames::FRAMES;
    planned(FRAMES, &[("pick", text("one")), ("at", n(0.5))])
}

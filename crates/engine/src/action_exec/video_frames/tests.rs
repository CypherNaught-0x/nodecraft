use super::*;
use crate::action_exec::LocalActions;
use nodecraft_exec::run::ActionExecutor;
use nodecraft_format::AssetStore;
use nodecraft_nodes::Registry;
use nodecraft_types::{AssetRef, Blake3, MediaMeta, NodeId, Number, Unsigned, Value};

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
    assert_eq!(job(FRAMES, &planned(FRAMES, &[]), 1), Ok(Job::Frames { pick: Pick::Count(8), size: 1024 }));
    assert_eq!(job(FRAMES, &planned(FRAMES, &[("pick", text("one")), ("at", n(1.5))]), 1), Ok(Job::Frames { pick: Pick::One(1_500_000), size: 1024 }));
    assert_eq!(job(FRAMES, &planned(FRAMES, &[("pick", text("every")), ("every", n(0.5))]), 1), Ok(Job::Frames { pick: Pick::Every(500_000), size: 1024 }));
    assert!(job(FRAMES, &planned(FRAMES, &[]), 0).unwrap_err().contains("connect a video"));
    assert_eq!(job(CONTACT_SHEET, &planned(CONTACT_SHEET, &[]), 1), Ok(Job::Sheet { columns: 4, rows: 4, cell: 320, gap: 8, background: [0, 0, 0, 255] }));
    assert!(job(CONTACT_SHEET, &planned(CONTACT_SHEET, &[("columns", u(16)), ("rows", u(5))]), 1).unwrap_err().contains("at most 64"));
    assert_eq!(
        job(LONG_EXPOSURE, &planned(LONG_EXPOSURE, &[("blend", text("lighten")), ("end", n(2.0))]), 1),
        Ok(Job::Exposure { blend: Blend::Lighten, start: 0, end: 2_000_000, frames: 32, size: 1280 })
    );
    assert!(job(LONG_EXPOSURE, &planned(LONG_EXPOSURE, &[("start", n(3.0)), ("end", n(2.0))]), 1).unwrap_err().contains("after the end"));
    assert_eq!(job(REVERSE, &planned(REVERSE, &[("mode", text("boomerang"))]), 1), Ok(Job::Reverse { boomerang: true }));
    let motion = Motion { zoom_from: 1.0, zoom_to: 1.3, pan: Pan::None, smooth: true };
    assert_eq!(job(KEN_BURNS, &planned(KEN_BURNS, &[]), 1), Ok(Job::Pan { length_us: 5_000_000, fps: 30, size: 1280, motion }));
    assert!(job(KEN_BURNS, &planned(KEN_BURNS, &[]), 0).unwrap_err().contains("connect a picture"));
    assert!(job("video.trim", &ParamValues::new(), 1).is_err());

    // Hostile values a hand-written job could carry.
    for (action, name, bad) in [
        (FRAMES, "size", u(0)),
        (FRAMES, "size", u(u64::MAX)),
        (FRAMES, "pick", text("all")),
        (CONTACT_SHEET, "columns", u(0)),
        (CONTACT_SHEET, "gap", u(1 << 40)),
        (CONTACT_SHEET, "background", text("black")),
        (LONG_EXPOSURE, "frames", u(1)),
        (LONG_EXPOSURE, "blend", text("screen")),
        (LONG_EXPOSURE, "start", n(-1.0)),
        (REVERSE, "mode", text("sideways")),
        (KEN_BURNS, "seconds", n(0.5)),
        (KEN_BURNS, "seconds", n(1e300)),
        (KEN_BURNS, "zoom_to", n(0.5)),
        (KEN_BURNS, "fps", text("1000")),
        (KEN_BURNS, "size", u(4096)),
    ] {
        let mut params = planned(action, &[]);
        params.insert(name.into(), bad);
        assert!(job(action, &params, 1).is_err(), "{action} {name}");
    }
    let mut params = planned(FRAMES, &[("pick", text("every"))]);
    params.insert("every".into(), n(0.01));
    assert!(job(FRAMES, &params, 1).unwrap_err().contains("at least 0.1"));
}

#[test]
fn frames_are_taken_inside_the_video_and_never_too_many() {
    assert_eq!(spread(4, 0, 4_000_000), [500_000, 1_500_000, 2_500_000, 3_500_000]);
    assert_eq!(spread(2, 1_000_000, 2_000_000), [1_250_000, 1_750_000]);
    assert_eq!(picks(Pick::One(999_999), 1_000_000), Ok(vec![999_999]));
    assert!(picks(Pick::One(1_000_000), 1_000_000).unwrap_err().contains("at or after the video's end"));
    assert_eq!(picks(Pick::Every(1_000_000), 2_500_000), Ok(vec![0, 1_000_000, 2_000_000]));
    assert!(picks(Pick::Every(100_000), 60_000_000).unwrap_err().contains("at most 64"));
    // Every frame of a long video spread out is still inside it.
    for time in spread(MAX_PICTURES, 0, u64::MAX) {
        assert!(time < u64::MAX);
    }
}

#[test]
fn reverse_plays_every_frame_once_and_a_boomerang_turns_without_a_stutter() {
    assert_eq!(frame_count(2_000_000, (10, 1)), 20);
    assert_eq!(frame_count(1_000_000, (30000, 1001)), 29);
    assert_eq!(frame_count(1, (30, 1)), 1);
    assert!(frame_count(u64::MAX, (120, 1)) > nodecraft_nodes::video_frames::MAX_REVERSED);
    assert_eq!(frame_start(0, (10, 1), 2_000_000), 0);
    assert_eq!(frame_start(19, (10, 1), 2_000_000), 1_900_000);
    // A rate's frames start a fraction of a microsecond late, never early.
    assert_eq!(frame_start(1, (30000, 1001), 2_000_000), 33_366);
    assert_eq!(frame_start(u64::MAX, (1, 1001), 2_000_000), 1_999_999);
    // A time is the start of the frame showing then, and the last frame past the end.
    assert_eq!(on_frame(1_984_375, (10, 1), 2_000_000), 1_900_000);
    assert_eq!(on_frame(1_049_999, (10, 1), 2_000_000), 1_000_000);
    assert_eq!(on_frame(u64::MAX, (10, 1), 2_000_000), 1_900_000);
    assert_eq!(on_frame(5, (30, 1), 10), 0);
    assert_eq!(reversed(4, false), [3, 2, 1, 0]);
    assert_eq!(reversed(4, true), [0, 1, 2, 3, 2, 1]);
    assert_eq!(reversed(1, true), [0]);
    assert_eq!(reversed(2, true), [0, 1]);
}

#[test]
fn pictures_fit_the_shape_they_are_put_in() {
    let red = [255, 0, 0, 255].repeat(4);
    assert_eq!(fit(&red, (2, 2), (2, 2)), red);
    // Centred on black when smaller, cropped when larger.
    let padded = fit(&red, (2, 2), (4, 2));
    assert_eq!(padded.get(..4), Some(&[0, 0, 0, 255][..]));
    assert_eq!(padded.get(4..8), Some(&[255, 0, 0, 255][..]));
    assert_eq!(fit(&red, (2, 2), (1, 1)), [255, 0, 0, 255]);
    // Fewer bytes than the shape says leave black, never a panic.
    assert_eq!(fit(&[1, 2], (2, 2), (2, 2)), [0, 0, 0, 255].repeat(4));
    assert_eq!(within_side((4096, 1024), 2048), (2048, 512));
    assert_eq!(within_side((641, 361), 2048), (640, 360));
    assert_eq!(scaled((4000, 3000), 1280), (1280, 960));
    assert_eq!(scaled((1, 10_000), 64), (16, 64));
    assert_eq!(scaled((0, 0), 64), (64, 64));
}

#[test]
fn exposures_average_or_keep_the_lightest_or_darkest() {
    let (dark, light) = ([10, 20, 30, 255], [30, 60, 90, 255]);
    for (blend, expected) in [(Blend::Average, [20, 40, 60, 255]), (Blend::Lighten, light), (Blend::Darken, dark)] {
        let mut exposure = Exposure::new(blend, 1, 1).unwrap();
        exposure.add(&dark);
        exposure.add(&light);
        assert_eq!(exposure.pixels(), expected, "{blend:?}");
    }
}

#[test]
fn a_contact_sheet_lays_frames_out_in_rows() {
    let mut sheet = Sheet::new(3, 2, (2, 1), 1, [9, 9, 9, 255]).unwrap();
    assert_eq!((sheet.width, sheet.height), (3 * 3 + 1, 2 * 2 + 1));
    assert_eq!((sheet.corner(0), sheet.corner(2), sheet.corner(3)), ((1, 1), (7, 1), (1, 3)));
    sheet.put(4, &[255, 0, 0, 255].repeat(2), (2, 1));
    let at = |x: usize, y: usize| sheet.pixels.get((y * 10 + x) * 4..(y * 10 + x) * 4 + 4).unwrap().to_vec();
    assert_eq!((at(4, 3), at(5, 3)), (vec![255, 0, 0, 255], vec![255, 0, 0, 255]));
    assert_eq!((at(3, 3), at(6, 3), at(4, 2)), (vec![9, 9, 9, 255], vec![9, 9, 9, 255], vec![9, 9, 9, 255]));
    // Too large to be a picture says so before any frame is decoded.
    assert!(Sheet::new(16, 4, (2048, 1152), 64, [0; 4]).err().unwrap().contains("too large"));
}

#[test]
fn pan_and_zoom_moves_from_one_view_to_the_other_inside_the_picture() {
    let still = Motion { zoom_from: 1.0, zoom_to: 1.0, pan: Pan::Right, smooth: false };
    assert_eq!(view(still, 0.5, (100, 50)), (1.0, 50.0, 25.0));
    let right = Motion { zoom_from: 2.0, zoom_to: 2.0, pan: Pan::Right, smooth: false };
    assert_eq!(view(right, 0.0, (100, 50)), (2.0, 25.0, 25.0));
    assert_eq!(view(right, 1.0, (100, 50)), (2.0, 75.0, 25.0));
    let zoom = Motion { zoom_from: 1.0, zoom_to: 3.0, pan: Pan::None, smooth: true };
    assert_eq!(view(zoom, 0.5, (100, 50)).0, 2.0);
    assert!(view(zoom, 0.25, (100, 50)).0 < 1.5, "smooth starts slowly");
    // Out of range stays at the ends.
    assert_eq!(view(zoom, 7.0, (100, 50)).0, 3.0);

    // Left half black, right half white: zoomed twice on the right, the frame is white.
    let mut pixels = Vec::new();
    for _ in 0..4 {
        pixels.extend([0, 0, 0, 255].repeat(4));
        pixels.extend([255, 255, 255, 255].repeat(4));
    }
    let whole = pan_frame(&pixels, (8, 4), (8, 4), (1.0, 4.0, 2.0));
    assert_eq!(whole, pixels);
    let right = pan_frame(&pixels, (8, 4), (4, 2), (2.0, 6.0, 2.0));
    assert!(right.chunks(4).all(|p| p == [255, 255, 255, 255]), "{right:?}");
    // A short buffer samples black, never panics.
    assert_eq!(pan_frame(&[], (8, 4), (2, 2), (1.0, 4.0, 2.0)).len(), 16);
}

#[test]
fn a_frame_action_without_tools_says_what_to_do() {
    let actions = LocalActions::new(AssetStore::default());
    for id in nodecraft_nodes::video_frames::IDS {
        assert!(actions.long_running(id), "{id}");
    }
    let video = Value::Media(AssetRef { hash: Blake3::from_bytes([7; 32]), mime: "video/mp4".parse().unwrap(), meta: MediaMeta::default() });
    let error = actions.execute(REVERSE, &planned(REVERSE, &[]), &[video]).unwrap_err();
    #[cfg(not(target_arch = "wasm32"))]
    assert!(error.contains("[media]"), "{error}");
    #[cfg(target_arch = "wasm32")]
    assert!(error.contains("desktop"), "{error}");
    let error = actions.execute(FRAMES, &planned(FRAMES, &[]), &[]).unwrap_err();
    assert!(error.contains("connect a video"), "{error}");
}

/// Opt in with NODECRAFT_VIDEO_TEST_FFMPEG and NODECRAFT_VIDEO_TEST_FFPROBE (absolute paths).
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "runs a real ffmpeg"]
fn each_action_makes_the_pictures_or_video_it_says() {
    let ffmpeg = std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let ffprobe = std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.mp4");
    let status = std::process::Command::new(&ffmpeg)
        .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", "testsrc=size=64x48:rate=10", "-t", "2", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let mut assets = AssetStore::default();
    let clip = assets.insert(&std::fs::read(&path).unwrap()).unwrap();
    let png = nodecraft_media::encode_png(40, 30, &[200, 100, 50, 255].repeat(40 * 30), &nodecraft_media::Limits::default()).unwrap();
    let picture = assets.insert(&png).unwrap();
    let video = Value::Media(AssetRef { hash: clip, mime: "video/mp4".parse().unwrap(), meta: MediaMeta::default() });
    let image = Value::Media(AssetRef { hash: picture, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets).with_video_tools(Some((ffmpeg, ffprobe)));
    let run = |action: &str, set: &[(&str, Value)], input: &Value| actions.execute(action, &planned(action, set), std::slice::from_ref(input));
    let size = |value: &Value| match value {
        Value::Media(asset) => (asset.mime.essence().to_owned(), asset.meta.width, asset.meta.height, asset.meta.duration_ms),
        other => panic!("{other:?}"),
    };

    let Ok(Value::List(frames)) = run(FRAMES, &[("frames", u(3)), ("size", u(32))], &video) else { panic!("frames") };
    assert_eq!(frames.iter().map(size).collect::<Vec<_>>(), vec![("image/png".to_owned(), Some(32), Some(24), None); 3]);
    let Ok(Value::List(one)) = run(FRAMES, &[("pick", text("one")), ("at", n(1.0))], &video) else { panic!("one frame") };
    assert_eq!(one.iter().map(size).collect::<Vec<_>>(), vec![("image/png".to_owned(), Some(64), Some(48), None)]);
    let sheet = run(CONTACT_SHEET, &[("columns", u(3)), ("rows", u(2)), ("cell", u(32)), ("gap", u(2))], &video).unwrap();
    assert_eq!(size(&sheet), ("image/png".to_owned(), Some(3 * 34 + 2), Some(2 * 26 + 2), None));
    let exposure = run(LONG_EXPOSURE, &[("frames", u(4)), ("size", u(64))], &video).unwrap();
    assert_eq!(size(&exposure), ("image/png".to_owned(), Some(64), Some(48), None));
    // 20 frames back, 20 + 18 there and back, at 10 a second.
    assert_eq!(size(&run(REVERSE, &[], &video).unwrap()), ("video/webm".to_owned(), Some(64), Some(48), Some(2000)));
    assert_eq!(size(&run(REVERSE, &[("mode", text("boomerang"))], &video).unwrap()), ("video/webm".to_owned(), Some(64), Some(48), Some(3800)));
    let pan = run(KEN_BURNS, &[("seconds", n(1.0)), ("size", u(64)), ("pan", text("right")), ("zoom_to", n(2.0))], &image).unwrap();
    assert_eq!(size(&pan), ("video/webm".to_owned(), Some(64), Some(48), Some(1000)));

    let error = run(FRAMES, &[("pick", text("one")), ("at", n(5.0))], &video).unwrap_err();
    assert!(error.contains("at or after the video's end"), "{error}");
    let error = run(LONG_EXPOSURE, &[("end", n(9.0))], &video).unwrap_err();
    assert!(error.contains("isn't inside the video"), "{error}");
}

#[test]
fn reverse_decodes_fewer_frames_the_larger_they_are() {
    use nodecraft_nodes::video_frames::MAX_REVERSED;
    assert_eq!(reversible((640, 360)), MAX_REVERSED);
    assert_eq!(reversible((1280, 720)), MAX_REVERSED);
    assert_eq!(reversible((1920, 1080)), 266);
    assert_eq!(reversible((2048, 2048)), 131);
    assert_eq!(reversible((0, 0)), MAX_REVERSED);
}

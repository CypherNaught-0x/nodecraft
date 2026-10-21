//! The Timeline editor (#109), driven through the card's context menu and real pointer and key
//! events on its track.
use super::mask_editor_tests::right_click;
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};
use nodecraft_types::EdgeId;

const ONE: NodeId = NodeId::from_bytes([61; 16]);
const TWO: NodeId = NodeId::from_bytes([62; 16]);
const EDITOR: NodeId = NodeId::from_bytes([63; 16]);

/// A few bytes that sniff as WebM: the editor lists it, but has no tools to measure it.
fn webm() -> Vec<u8> {
    let mut bytes = b"\x1A\x45\xDF\xA3\x9f\x42\x82\x84webm".to_vec();
    bytes.resize(64, 0);
    bytes
}

/// Two videos wired into a Timeline card, `one` before `two`.
fn with_timeline(h: &mut Harness<'static, NodeCraftApp>, one: &[u8], two: &[u8]) {
    let canvas = h.state().canvas;
    let at = nodecraft_graph::Position { x: -300.0, y: -160.0 };
    let files = [Incoming { name: "one.webm", bytes: one }, Incoming { name: "two.webm", bytes: two }];
    import_files(&mut h.state_mut().session, canvas, at, 300.0, &files, &[ONE, TWO]).unwrap();
    let session = &mut h.state_mut().session;
    session.dispatch("node.create", &json!({"canvas": canvas, "id": EDITOR, "kind": nodecraft_nodes::timeline::ID, "pos": {"x": 200, "y": -110}})).unwrap();
    for (n, from) in [(1u8, ONE), (2, TWO)] {
        let edge = json!({"id": EdgeId::from_bytes([60 + n; 16]), "from": {"node": from, "port": "out"}, "to": {"node": EDITOR, "port": nodecraft_nodes::timeline::INPUT}, "order": n});
        session.dispatch("edge.connect", &json!({"canvas": canvas, "edge": edge})).unwrap();
    }
    h.run();
}

/// Opens the editor from the Timeline card's menu.
fn open(h: &mut Harness<'static, NodeCraftApp>) {
    for _ in 0..20 {
        if h.query_by_label("Edit timeline…").is_none() {
            right_click(h, EDITOR);
        }
        h.get_by_label("Edit timeline…").click();
        h.run_steps(2);
        if h.state().timeline_editor.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(h.state().timeline_editor.is_some(), "{:?}", h.state().notice);
    h.run_steps(2);
}

fn opened() -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    with_timeline(&mut h, &webm(), &webm());
    open(&mut h);
    h
}

fn list(h: &mut Harness<'static, NodeCraftApp>) -> Vec<serde_json::Value> {
    let canvas = h.state().canvas;
    let listed = h.state_mut().session.dispatch("timeline.list", &json!({"canvas": canvas, "id": EDITOR})).unwrap();
    listed["clips"].as_array().unwrap().clone()
}

fn ids(h: &mut Harness<'static, NodeCraftApp>) -> Vec<String> {
    list(h).iter().map(|c| c["id"].as_str().unwrap().to_owned()).collect()
}

fn auto(node: NodeId) -> String {
    nodecraft_nodes::timeline::auto_id(nodecraft_nodes::timeline::Input::Clips, &node.to_string(), 0)
}

fn dispatch(h: &mut Harness<'static, NodeCraftApp>, command: &str, mut params: serde_json::Value) {
    params["canvas"] = json!(h.state().canvas);
    params["id"] = json!(EDITOR);
    h.state_mut().session.dispatch(command, &params).unwrap();
    h.run_steps(2);
}

/// Output time on screen, as the track last drew it.
fn screen_x(h: &Harness<'static, NodeCraftApp>, us: u64) -> f32 {
    let editor = h.state().timeline_editor.as_ref().unwrap();
    editor.origin.x + us as f32 / 1e6 * editor.scale
}

fn centre_of(h: &Harness<'static, NodeCraftApp>, label: &str) -> Pos2 {
    h.get_by_label(label).rect().center()
}

/// Drags by the pointer from `start` to `end`.
fn drag(h: &mut Harness<'static, NodeCraftApp>, start: Pos2, end: Pos2) {
    h.hover_at(start);
    h.step();
    h.event(egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(start + (end - start) * (i as f32 / 8.0)));
        h.step();
    }
    h.event(egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(3);
}

/// What the editor calls an untitled video's clip: its card's name, numbered in wire order.
fn clip_label(_: &Harness<'static, NodeCraftApp>, node: NodeId) -> String {
    (if node == ONE { "Video 1" } else { "Video 2" }).to_owned()
}

#[test]
fn the_menu_opens_the_editor_on_the_wired_clips_and_trims_are_one_undo_step_each() {
    let mut h = opened();
    let (one, two) = (clip_label(&h, ONE), clip_label(&h, TWO));
    assert!(h.query_by_label("Playhead").is_some());
    // Unmeasured videos take a stand-in length, and only their starts trim.
    assert!(h.query_by_label(&format!("Trim start of {one}")).is_some());
    assert!(h.query_by_label(&format!("Trim end of {one}")).is_none());

    // Once an end is set, dragging it trims the clip by the time the pointer travels.
    dispatch(&mut h, "timeline.update", json!({"clip": auto(ONE), "set": {"out_us": 4_000_000}}));
    let end = centre_of(&h, &format!("Trim end of {one}"));
    // By the screen distance from 4 s to 3 s, wherever on the grip the press lands.
    let to = end + Vec2::new(screen_x(&h, 3_000_000) - screen_x(&h, 4_000_000), 0.0);
    drag(&mut h, end, to);
    let clips = list(&mut h);
    let out = clips[0]["out_us"].as_u64().unwrap();
    assert!(out.abs_diff(3_000_000) <= 50_000, "{out} {:?}", h.state().notice);
    assert_eq!(h.state().timeline_editor.as_ref().unwrap().selected.as_deref(), Some(auto(ONE).as_str()));

    let start = centre_of(&h, &format!("Trim start of {one}"));
    let to = start + Vec2::new(screen_x(&h, 1_000_000) - screen_x(&h, 0), 0.0);
    drag(&mut h, start, to);
    let clips = list(&mut h);
    let trimmed = clips[0]["in_us"].as_u64().unwrap();
    assert!(trimmed.abs_diff(1_000_000) <= 50_000, "{trimmed}");
    assert_eq!(clips[0]["out_us"].as_u64(), Some(out), "a start trim keeps the end");

    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run_steps(2);
    assert_eq!(list(&mut h)[0]["in_us"], json!(0));
    assert!(h.query_by_label(&format!("Trim start of {two}")).is_some());
}

#[test]
fn dragging_a_clip_past_the_next_reorders_them() {
    let mut h = opened();
    let one = clip_label(&h, ONE);
    let from = centre_of(&h, &format!("{one} on the track"));
    // Past the middle of the second clip (5 s to 10 s at the stand-in length).
    let to = Pos2::new(screen_x(&h, 9_000_000), from.y);
    drag(&mut h, from, to);
    assert_eq!(ids(&mut h), [auto(TWO), auto(ONE)], "{:?}", h.state().notice);
    // A click without travel only selects.
    let back = centre_of(&h, &format!("{} on the track", clip_label(&h, TWO)));
    drag(&mut h, back, back);
    assert_eq!(ids(&mut h), [auto(TWO), auto(ONE)]);
    assert_eq!(h.state().timeline_editor.as_ref().unwrap().selected.as_deref(), Some(auto(TWO).as_str()));
}

#[test]
fn the_playhead_splits_a_clip_and_keys_delete_and_close() {
    let mut h = opened();
    let one = clip_label(&h, ONE);
    // Click the ruler 2 s in, inside the first clip, and split there.
    let ruler = h.get_by_label("Playhead").rect();
    let at = Pos2::new(screen_x(&h, 2_000_000), ruler.center().y);
    drag(&mut h, at, at);
    let head = h.state().timeline_editor.as_ref().unwrap().playhead_us;
    assert!(head.abs_diff(2_000_000) <= 50_000, "{head}");
    h.get_by_label("Split at playhead").click();
    h.run_steps(2);
    let clips = list(&mut h);
    assert_eq!(clips.len(), 3, "{:?}", h.state().notice);
    assert_eq!((clips[0]["out_us"].as_u64(), clips[1]["in_us"].as_u64()), (Some(head), Some(head)));
    assert!(h.query_by_label(&format!("{one} (part 2)")).is_some());

    // Delete removes the selected part; the video's last clip refuses and says why.
    h.get_by_label(&format!("{one} (part 2)")).click();
    h.run_steps(2);
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert_eq!(list(&mut h).len(), 2);
    h.get_by_label(&one).click();
    h.run_steps(2);
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert_eq!(list(&mut h).len(), 2);
    assert!(h.state().notice.as_deref().is_some_and(|n| n.contains("disable")), "{:?}", h.state().notice);

    // Escape clears the selection, then closes.
    h.get_by_label(&one).click();
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().timeline_editor.as_ref().is_some_and(|e| e.selected.is_none()));
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().timeline_editor.is_none());
}

#[test]
fn fields_change_the_selected_clip_and_the_output_frame() {
    let mut h = opened();
    // With nothing selected the panel shows the output frame.
    assert!(h.query_by_label("Output frame").is_some());
    h.get_by_label(&clip_label(&h, TWO)).click();
    h.run_steps(2);
    h.get_by_label("Mute").click();
    h.run_steps(2);
    assert_eq!(list(&mut h)[1]["muted"], json!(true));
    // Unticking a clip keeps it listed but takes it off the track.
    let two = clip_label(&h, TWO);
    h.get_by_label(&format!("Play {two}")).click();
    h.run_steps(2);
    assert_eq!(list(&mut h)[1]["enabled"], json!(false));
    assert!(h.query_by_label(&format!("Trim start of {two}")).is_none());
}

#[test]
fn tracks_are_added_hold_clips_dragged_onto_them_and_remove_once_empty() {
    let mut h = opened();
    let one = clip_label(&h, ONE);
    h.get_by_label("Add picture track").click();
    h.run_steps(2);
    let canvas = h.state().canvas;
    let listed = h.state_mut().session.dispatch("timeline.list", &json!({"canvas": canvas, "id": EDITOR})).unwrap();
    assert_eq!(listed["tracks"], json!([{"kind": "video"}, {"kind": "video"}]), "{:?}", h.state().notice);

    // Dragged down onto the new lane and right by a second, it plays from there over the main track.
    let block = h.get_by_label(&format!("{one} on the track")).rect();
    let to = block.center() + Vec2::new(screen_x(&h, 1_000_000) - screen_x(&h, 0), block.height() + 4.0);
    drag(&mut h, block.center(), to);
    let clips = list(&mut h);
    let moved = clips.iter().find(|c| c["id"] == json!(auto(ONE))).unwrap();
    assert_eq!(moved["track"], json!(1), "{:?}", h.state().notice);
    let at = moved["at_us"].as_u64().unwrap();
    assert!(at.abs_diff(1_000_000) <= 50_000, "{at}");
    // Then along its lane, by another second.
    let block = h.get_by_label(&format!("{one} on the track")).rect();
    let second = screen_x(&h, 1_000_000) - screen_x(&h, 0);
    drag(&mut h, block.center(), block.center() + Vec2::new(second, 0.0));
    let later = list(&mut h).iter().find(|c| c["id"] == json!(auto(ONE))).unwrap()["at_us"].as_u64().unwrap();
    assert!(later.abs_diff(at + 1_000_000) <= 50_000, "{later}");

    // A track with clips stays; undone back to the main track, it removes.
    let remove = h.get_by_label("Remove Picture track 1");
    remove.click();
    h.run_steps(2);
    assert_eq!(list(&mut h).iter().filter(|c| c["track"] == json!(1)).count(), 1);
    for _ in 0..2 {
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    }
    h.run_steps(2);
    h.get_by_label("Remove Picture track 1").click();
    h.run_steps(2);
    let listed = h.state_mut().session.dispatch("timeline.list", &json!({"canvas": canvas, "id": EDITOR})).unwrap();
    assert_eq!(listed["tracks"], json!([{"kind": "video"}]), "{:?}", h.state().notice);
}

/// Real lengths and thumbnails through installed tools: opt in with
/// `NODECRAFT_VIDEO_TEST_FFMPEG` and `NODECRAFT_VIDEO_TEST_FFPROBE` (absolute paths).
#[test]
#[ignore = "needs installed ffmpeg and ffprobe"]
fn measured_videos_show_their_lengths_and_trim_to_their_ends() {
    let ffmpeg = std::path::PathBuf::from(std::env::var("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let ffprobe = std::path::PathBuf::from(std::env::var("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap());
    let make = |seconds: &str| {
        let path = std::env::temp_dir().join(format!("nodecraft-timeline-editor-{}-{seconds}.webm", std::process::id()));
        let status = std::process::Command::new(&ffmpeg)
            .args([
                "-v",
                "error",
                "-nostdin",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=10",
                "-t",
                seconds,
                "-c:v",
                "libvpx-vp9",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        bytes
    };
    let (a, b) = (make("2"), make("3"));
    let mut h = harness();
    let mut settings = NodeCraftSettings::default();
    settings.media.ffmpeg = Some(ffmpeg.clone());
    settings.media.ffprobe = Some(ffprobe);
    let ctx = h.ctx.clone();
    h.state_mut().apply_settings(&ctx, &settings);
    with_timeline(&mut h, &a, &b);
    open(&mut h);
    let one = clip_label(&h, ONE);
    let mut measured = false;
    for _ in 0..500 {
        h.step();
        if h.query_by_label(&format!("Trim end of {one}")).is_some() {
            measured = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(measured, "the editor never learned the video's length");
    // Dragging the end past the video's end stops there, and plays to the end.
    let end = centre_of(&h, &format!("Trim end of {one}"));
    let to = end + Vec2::new(screen_x(&h, 1_000_000) - screen_x(&h, 2_000_000), 0.0);
    drag(&mut h, end, to);
    assert!(list(&mut h)[0]["out_us"].as_u64().is_some_and(|out| out.abs_diff(1_000_000) <= 50_000));
    let end = centre_of(&h, &format!("Trim end of {one}"));
    drag(&mut h, end, Pos2::new(end.x + 400.0, end.y));
    assert_eq!(list(&mut h)[0]["out_us"], serde_json::Value::Null);
    if let Ok(dir) = std::env::var("NODECRAFT_SCREENSHOT_DIR") {
        for _ in 0..50 {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let image = h.render().unwrap();
        image.save(std::path::Path::new(&dir).join("timeline-editor.png")).unwrap();
    }
}

#[test]
fn the_editor_exports_an_interchange_archive_where_the_dialog_says() {
    let mut h = opened();
    // The test videos can't be measured, so each clip says where it ends.
    for node in [ONE, TWO] {
        dispatch(&mut h, "timeline.update", json!({"clip": auto(node), "set": {"out_us": 1_000_000}}));
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cut.zip");
    let revision = h.state().session.revision();
    h.state_mut().exports.answer = Some(path.clone());
    h.get_by_label("Export for another editor…").click();
    h.run_steps(3);
    assert_eq!(h.state().notice.as_deref(), Some("Exported cut.zip."));
    let zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut names: Vec<String> = zip.file_names().map(|name| name.unwrap().into_owned()).collect();
    names.sort();
    assert_eq!(names.len(), 3, "the same video wired twice is one file: {names:?}");
    assert!(names.iter().any(|n| n.ends_with(".fcpxml")) && names.contains(&"report.txt".to_owned()), "{names:?}");
    assert_eq!(h.state().session.revision(), revision, "exporting doesn't edit the project");
}

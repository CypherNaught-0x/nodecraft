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

#[test]
fn regenerated_shorter_sources_warn_before_running_and_keep_the_rendered_version() {
    use nodecraft_graph::{Generation, SourceVersion};
    use nodecraft_types::GenerationId;
    let mut h = opened();
    let canvas = h.state().canvas;
    let source = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&ONE).unwrap().params.get("asset").cloned().unwrap();
    let Value::Media(mut asset) = source else { panic!("video source") };
    asset.meta.duration_ms = Some(2000);
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas":canvas,"id":ONE,"name":"asset","value":Value::Media(asset.clone())})).unwrap();
    dispatch(&mut h, "timeline.update", json!({"clip":auto(ONE),"set":{"out_us":1_500_000}}));
    let mut rendered = Generation::new(GenerationId::from_bytes([90; 16]), vec![]);
    rendered.source_versions.push(SourceVersion { role: auto(ONE), hash: asset.hash, item: None });
    let revision = h.state().session.revision();
    h.state_mut().session.record_derived(canvas, vec![(EDITOR, rendered)], &Default::default(), revision).unwrap();
    h.state_mut().timeline_editor.as_mut().unwrap().selected = Some(auto(ONE));
    h.run_steps(3);
    assert!(h.query_by_label("Trim exceeds source").is_none());
    if let Ok(dir) = std::env::var("NODECRAFT_SCREENSHOT_DIR") {
        h.render().unwrap().save(std::path::Path::new(&dir).join("timeline-source-before.png")).unwrap();
    }
    let old = asset.hash;
    let mut bytes = webm();
    *bytes.last_mut().unwrap() = 1;
    asset.hash = h.state_mut().session.import_asset(&bytes).unwrap();
    asset.meta.duration_ms = Some(1000);
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas":canvas,"id":ONE,"name":"asset","value":Value::Media(asset)})).unwrap();
    h.run_steps(3);
    assert!(h.query_by_label("A clip extends beyond its current source. Adjust its trim before rendering.").is_some());
    assert!(h.query_by_label("Trim exceeds source").is_some());
    assert!(h.query_by_label("Source changed since this clip was rendered.").is_some());
    let clip = list(&mut h).remove(0);
    assert_eq!((clip["id"].clone(), clip["out_us"].clone(), clip["rendered_sources"][0]["hash"].clone()), (json!(auto(ONE)), json!(1_500_000), json!(old)));
    assert!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&EDITOR).unwrap().history.len() == 1, "warning starts no render");
    if let Ok(dir) = std::env::var("NODECRAFT_SCREENSHOT_DIR") {
        h.render().unwrap().save(std::path::Path::new(&dir).join("timeline-source-after.png")).unwrap();
    }
    // Turning the clip off or losing its wire keeps it listed but removes the run warning.
    dispatch(&mut h, "timeline.update", json!({"clip":auto(ONE),"set":{"enabled":false}}));
    assert!(h.query_by_label("Trim exceeds source").is_none());
    h.state_mut().session.dispatch("edge.disconnect", &json!({"canvas":canvas,"id":EdgeId::from_bytes([61;16])})).unwrap();
    h.run_steps(2);
    assert_eq!(list(&mut h)[0]["wired"], json!(false));
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

/// Types `text` into the number field labelled `label` and lets it apply.
fn set_number(h: &mut Harness<'static, NodeCraftApp>, label: &str, text: &str) {
    h.get_by_role_and_label(egui::accesskit::Role::SpinButton, label).click();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::SpinButton, label).type_text(text);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
}

#[test]
fn a_clip_is_sped_up_placed_and_turned_and_the_render_quality_picked() {
    let mut h = opened();
    h.get_by_label(&clip_label(&h, TWO)).click();
    h.run_steps(2);
    for (label, text) in [("Speed (%)", "200"), ("Size (%)", "50"), ("Across (%)", "25"), ("Turn (°)", "90")] {
        set_number(&mut h, label, text);
    }
    let two = list(&mut h)[1].clone();
    let got = (two["speed"].clone(), two["scale"].clone(), two["pos_x"].clone(), two["rotation"].clone());
    assert_eq!(got, (json!(2000), json!(500), json!(250), json!(90)), "{:?}", h.state().notice);
    // Each field is one undo step; Fit to the frame puts it back in one.
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run_steps(2);
    assert_eq!((list(&mut h)[1]["rotation"].clone(), list(&mut h)[1]["scale"].clone()), (json!(0), json!(500)));
    h.get_by_label("Fit to the frame").click();
    h.run_steps(3);
    let two = list(&mut h)[1].clone();
    assert_eq!((two["scale"].clone(), two["pos_x"].clone(), two["speed"].clone()), (json!(1000), json!(500), json!(2000)));

    // With nothing selected, the output's quality.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label("Output frame").is_some());
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Render quality").click();
    h.run_steps(2);
    h.get_by_label("Draft (smallest file)").click();
    h.run_steps(3);
    let canvas = h.state().canvas;
    let node = h.state().session.document().canvases().get(&canvas).and_then(|c| c.nodes().get(&EDITOR)).cloned().unwrap();
    assert_eq!(node.params.get("quality"), Some(&Value::Text("draft".into())), "{:?}", h.state().notice);
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
    // Advertised metadata can outlive a regenerated source's real duration. A ready probe
    // must take precedence, or the pre-run warning misses this two-second source.
    let canvas = h.state().canvas;
    let Value::Media(mut asset) =
        h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&ONE).unwrap().params.get("asset").cloned().unwrap()
    else {
        panic!("video")
    };
    asset.meta.duration_ms = Some(4000);
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas":canvas,"id":ONE,"name":"asset","value":Value::Media(asset)})).unwrap();
    open(&mut h);
    dispatch(&mut h, "timeline.update", json!({"clip":auto(ONE),"set":{"out_us":2_500_000}}));
    let one = clip_label(&h, ONE);
    let mut measured = false;
    for _ in 0..500 {
        h.step();
        if h.query_by_label("Trim exceeds source").is_some() {
            measured = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(measured, "the editor never warned about the actual shorter video despite advertised metadata");
    assert_eq!(list(&mut h)[0]["out_us"], json!(2_500_000), "the warning preserves the stored trim");
    if let Ok(dir) = std::env::var("NODECRAFT_SCREENSHOT_DIR") {
        h.state_mut().timeline_editor.as_mut().unwrap().selected = Some(auto(ONE));
        for _ in 0..50 {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        h.render().unwrap().save(std::path::Path::new(&dir).join("timeline-source-probed.png")).unwrap();
    }
    dispatch(&mut h, "timeline.update", json!({"clip":auto(ONE),"set":{"out_us":null}}));
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
        // The first clip at double speed, shrunk, moved left and turned: its outline over the picture.
        let set = json!({"speed": 2000, "scale": 600, "pos_x": 350, "pos_y": 450, "rotation": 12, "opacity": 800, "crop_right": 100});
        dispatch(&mut h, "timeline.update", json!({"clip": auto(ONE), "set": set}));
        h.state_mut().timeline_editor.as_mut().unwrap().selected = Some(auto(ONE));
        h.run_steps(4);
        h.render().unwrap().save(std::path::Path::new(&dir).join("timeline-placement.png")).unwrap();
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        h.run_steps(2);
        // A caption over the picture, picked so its fields show.
        dispatch(
            &mut h,
            "timeline.caption_add",
            json!({"caption": "hello", "text": "Hello, timeline", "start_us": 0, "end_us": 2_000_000, "size": 120, "stroke_width": 6}),
        );
        let editor = h.state_mut().timeline_editor.as_mut().unwrap();
        (editor.selected, editor.caption) = (None, Some("hello".into()));
        h.run_steps(4);
        h.render().unwrap().save(std::path::Path::new(&dir).join("timeline-captions.png")).unwrap();
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

fn captions(h: &mut Harness<'static, NodeCraftApp>) -> Vec<serde_json::Value> {
    let canvas = h.state().canvas;
    let listed = h.state_mut().session.dispatch("timeline.list", &json!({"canvas": canvas, "id": EDITOR})).unwrap();
    listed["captions"].as_array().unwrap().clone()
}

#[test]
fn captions_are_added_at_the_playhead_edited_and_deleted_as_undo_steps() {
    let mut h = opened();
    h.state_mut().timeline_editor.as_mut().unwrap().playhead_us = 1_000_000;
    h.get_by_label("Add caption").click();
    h.run_steps(2);
    let added = captions(&mut h);
    assert_eq!(added.len(), 1, "{:?}", h.state().notice);
    assert_eq!((&added[0]["text"], &added[0]["start_us"], &added[0]["end_us"]), (&json!("Caption"), &json!(1_000_000), &json!(3_000_000)));
    // The new caption is picked, so the panel shows its fields.
    assert_eq!(h.state().timeline_editor.as_ref().unwrap().caption.as_deref(), Some("caption-1"));
    assert!(h.query_by_label("Delete caption").is_some());
    // Its fields fit the side panel: the editor doesn't grow past the window.
    assert!(h.get_by_label("Earlier").rect().min.x >= 0.0);

    h.get_by_label("Top").click();
    h.run_steps(2);
    assert_eq!(captions(&mut h)[0]["position"], json!("top"));
    h.get_by_label("Show caption Caption").click();
    h.run_steps(2);
    assert_eq!(captions(&mut h)[0]["enabled"], json!(false), "{:?}", h.state().notice);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run_steps(2);
    assert_eq!(captions(&mut h)[0]["enabled"], json!(true));

    // Picking a clip shows its fields instead; Delete with the caption picked deletes it.
    h.get_by_label(&clip_label(&h, TWO)).click();
    h.run_steps(2);
    assert!(h.query_by_label("Delete caption").is_none());
    assert_eq!(h.state().timeline_editor.as_ref().unwrap().caption, None);
    h.get_by_label("Caption").click();
    h.run_steps(2);
    assert_eq!(h.state().timeline_editor.as_ref().unwrap().selected, None);
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert!(captions(&mut h).is_empty());
    assert_eq!(ids(&mut h).len(), 2, "the clips stay");

    // An edit held for a caption that goes elsewhere is dropped, so another can be picked.
    dispatch(&mut h, "timeline.caption_add", json!({"caption": "a", "text": "First", "start_us": 0, "end_us": 10}));
    dispatch(&mut h, "timeline.caption_add", json!({"caption": "b", "text": "Second", "start_us": 0, "end_us": 10}));
    h.state_mut().timeline_editor.as_mut().unwrap().draft = Some(("caption:gone.text".into(), "typed".into()));
    h.get_by_label("Second").click();
    h.run_steps(3);
    assert!(h.state().timeline_editor.as_ref().unwrap().draft.is_none());
    assert!(h.query_by_label("Delete caption").is_some());
}

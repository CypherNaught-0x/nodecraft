use super::*;
use crate::Session;
use nodecraft_types::{EdgeId, ProjectId};

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const A: NodeId = NodeId::from_bytes([3; 16]);
const B: NodeId = NodeId::from_bytes([4; 16]);
const EDITOR: NodeId = NodeId::from_bytes([5; 16]);

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    for (id, kind, x) in [(A, "source.video", 0), (B, "source.video", 0), (EDITOR, ID, 400)] {
        s.dispatch("node.create", &json!({"canvas": CANVAS, "id": id, "kind": kind, "pos": {"x": x, "y": 0}})).unwrap();
    }
    for (n, from) in [(1u8, A), (2, B)] {
        let edge =
            json!({"id": EdgeId::from_bytes([n; 16]), "from": {"node": from, "port": "out"}, "to": {"node": EDITOR, "port": timeline::INPUT}, "order": n});
        s.dispatch("edge.connect", &json!({"canvas": CANVAS, "edge": edge})).unwrap();
    }
    s
}

fn list(s: &mut Session) -> Vec<Json> {
    s.dispatch("timeline.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap()["clips"].as_array().unwrap().clone()
}

fn ids(s: &mut Session) -> Vec<String> {
    list(s).iter().map(|c| c["id"].as_str().unwrap().to_owned()).collect()
}

fn node(s: &Session) -> Node {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&EDITOR).unwrap().clone()
}

fn on(clip: &str) -> Json {
    json!({"canvas": CANVAS, "id": EDITOR, "clip": clip})
}

fn with(mut base: Json, extra: Json) -> Json {
    if let (Json::Object(base), Json::Object(extra)) = (&mut base, extra) {
        base.extend(extra);
    }
    base
}

fn many_wired(count: u8) -> Session {
    let mut s = session();
    for n in 3..=count {
        let from = NodeId::from_bytes([n.saturating_add(10); 16]);
        s.dispatch("node.create", &json!({"canvas": CANVAS, "id": from, "kind": "source.video", "pos": {"x": 0, "y": 0}})).unwrap();
        let edge = json!({"id": EdgeId::from_bytes([n.saturating_add(10); 16]), "from": {"node": from, "port": "out"}, "to": {"node": EDITOR, "port": timeline::INPUT}, "order": n});
        s.dispatch("edge.connect", &json!({"canvas": CANVAS, "edge": edge})).unwrap();
    }
    s
}

#[test]
fn forty_wired_takes_edit_without_rendering_more_than_thirty_two() {
    let mut s = many_wired(40);
    let clips = ids(&mut s);
    assert_eq!(clips.len(), 40);
    // Even an initially over-limit document can be made runnable a disable at a time.
    for clip in clips.iter().skip(MAX_CLIPS) {
        s.dispatch("timeline.update", &with(on(clip), json!({"set": {"enabled": false}}))).unwrap();
    }
    assert_eq!(list(&mut s).iter().filter(|clip| clip["enabled"] == true).count(), MAX_CLIPS);
    let disabled = clips.get(32).unwrap();
    s.dispatch("timeline.update", &with(on(disabled), json!({"set": {"in_us": 100_000}}))).unwrap();
    s.dispatch("timeline.move", &with(on(disabled), json!({"index": 39}))).unwrap();
    s.dispatch("timeline.move", &with(on(disabled), json!({"index": 0}))).unwrap();
    let enabled = clips.first().unwrap();
    s.dispatch("timeline.update", &with(on(enabled), json!({"set": {"out_us": 1_000_000}}))).unwrap();
    assert_eq!(list(&mut s).iter().find(|clip| clip["id"] == disabled.as_str()).unwrap()["in_us"], json!(100_000));

    // An error creates no revision or changeset and does not discard the redo step.
    s.dispatch("edit.undo", &json!({})).unwrap();
    let before = (s.document().clone(), s.revision());
    assert!(s.can_redo());
    let error = s.dispatch("timeline.update", &with(on(disabled), json!({"set": {"enabled": true}}))).unwrap_err();
    assert_eq!(error.message, "a timeline renders at most 32 clips: disable another first");
    assert_eq!((s.document(), s.revision()), (&before.0, before.1));
    assert!(s.can_redo());
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(list(&mut s).iter().find(|clip| clip["id"] == enabled.as_str()).unwrap()["out_us"], json!(1_000_000));
    let revision = s.revision();
    let split = s.dispatch("timeline.split", &with(on(enabled), json!({"at_us": 500_000}))).unwrap_err();
    assert_eq!(split.message, "a timeline renders at most 32 clips: disable another first");
    assert_eq!(s.revision(), revision);

    // Disabled clips remain editable even when the enabled count is exactly 32.
    s.dispatch("timeline.update", &with(on(disabled), json!({"set": {"out_us": 2_000_000}}))).unwrap();
    assert_eq!(list(&mut s).iter().find(|clip| clip["id"] == disabled.as_str()).unwrap()["out_us"], json!(2_000_000));
    s.dispatch("timeline.update", &with(on(enabled), json!({"set": {"enabled": false}}))).unwrap();
    s.dispatch("timeline.update", &with(on(disabled), json!({"set": {"enabled": true}}))).unwrap();
}

#[test]
fn structural_clip_bound_reports_oversized_wires_instead_of_losing_them() {
    let mut s = many_wired(40);
    let stored_clips: Vec<Json> = (0..=MAX_STRUCTURAL_CLIPS).map(|n| json!({"id": format!("extra{n}"), "origin": "absent", "enabled": false})).collect();
    assert!(s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": stored_clips})).is_err());
    assert_eq!(ids(&mut s).len(), 40);

    // A previously saved spec can still be read. Its new wires must be reported,
    // never truncated and silently written over by an edit.
    let mut s = many_wired(32);
    let saved: Vec<Json> = (0..100).map(|n| json!({"id": format!("old{n}"), "origin": "absent", "enabled": false})).collect();
    s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": saved})).unwrap();
    assert_eq!(stored(&node(&s)).unwrap().clips.len(), 100);
    assert_eq!(s.dispatch("timeline.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap()["clips"].as_array().unwrap().len(), 132);
    let err = s.dispatch("timeline.update", &with(on("old0"), json!({"set": {"in_us": 100_000}}))).unwrap_err();
    assert_eq!(err.message, "a timeline stores at most 128 clips, one for each wired input included: delete some clips or disconnect some inputs");
    assert_eq!(stored(&node(&s)).unwrap().clips.len(), 100);

    // The advice can be followed: deleting old takes one by one shrinks the stored list (the wired
    // clips stay automatic) until edits fit again.
    for n in 0..3 {
        s.dispatch("timeline.remove", &on(&format!("old{n}"))).unwrap();
        assert_eq!(stored(&node(&s)).unwrap().clips.len(), 99 - n);
    }
    // The fourth removal fits: it stores the 96 old takes with the 32 automatic clips.
    s.dispatch("timeline.remove", &on("old3")).unwrap();
    assert_eq!(stored(&node(&s)).unwrap().clips.len(), MAX_STRUCTURAL_CLIPS);
    assert_eq!(ids(&mut s).len(), MAX_STRUCTURAL_CLIPS);
    s.dispatch("timeline.update", &with(on("old4"), json!({"set": {"in_us": 100_000}}))).unwrap();
    assert_eq!(stored(&node(&s)).unwrap().clips.len(), MAX_STRUCTURAL_CLIPS);
    assert_eq!(list(&mut s).iter().filter(|clip| clip["wired"] == true).count(), 32);
}

#[test]
fn set_is_refused_past_the_render_limit_only_when_it_renders_more_than_now() {
    let mut s = many_wired(40);
    let clips = ids(&mut s);
    // An edit stores all 40 automatic clips, every one enabled.
    s.dispatch("timeline.update", &with(on(clips.first().unwrap()), json!({"set": {"in_us": 100_000}}))).unwrap();
    assert_eq!(stored(&node(&s)).unwrap().clips.len(), 40);
    // Going back to automatic clips renders no more than now.
    s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": []})).unwrap();
    assert!(stored(&node(&s)).unwrap().clips.is_empty());
    assert_eq!(ids(&mut s), clips);
    // All 40 with one disabled renders fewer, as the same change through timeline.update would.
    let all: Vec<Json> =
        list(&mut s).iter().enumerate().map(|(i, clip)| json!({"id": clip["id"], "origin": clip["origin"], "nth": clip["nth"], "enabled": i != 0})).collect();
    s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": all})).unwrap();
    assert_eq!(list(&mut s).iter().filter(|clip| clip["enabled"] == true).count(), 39);
    // Enabling more than now past the limit is still refused, and changes nothing.
    let revision = s.revision();
    let error = s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": []})).unwrap_err();
    assert_eq!(error.message, "a timeline renders at most 32 clips: disable another first");
    assert_eq!(s.revision(), revision);
    assert_eq!(stored(&node(&s)).unwrap().clips.len(), 40);
}

#[test]
fn opened_project_with_oversized_stored_timeline_keeps_its_data_and_redo() {
    // Simulate a project saved before this bound existed: 129 stored clips, with no
    // automatic clips needed to exceed the limit. The format must not discard it.
    let mut s = session();
    let accepted: Vec<Json> = (0..128).map(|n| json!({"id": format!("ok{n}"), "origin": "absent", "enabled": false})).collect();
    s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": accepted})).unwrap();
    assert_eq!(stored(&node(&s)).unwrap().clips.len(), 128);
    let clips: Vec<Json> = (0..129).map(|n| json!({"id": format!("old{n}"), "origin": "absent", "enabled": false})).collect();
    let original = json!({"clips": clips}).to_string();
    let mut timeline_node = node(&s);
    timeline_node.config.insert(CONFIG.into(), Value::Text(original.clone()));
    let mut project = s.document().clone();
    Change::SetNode { canvas: CANVAS, node: timeline_node }.apply(&mut project).unwrap();
    let snapshot = nodecraft_format::Snapshot { project, assets: Default::default(), epoch: 0 };
    let saved = nodecraft_format::encode(&snapshot).unwrap();
    let opened = nodecraft_format::decode(&saved).unwrap();
    let mut s = Session::new(opened.project).unwrap();
    assert_eq!(node(&s).config.get(CONFIG), Some(&Value::Text(original.clone())));
    let list_error = s.dispatch("timeline.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap_err();
    assert!(list_error.message.contains("a timeline stores at most 128 clips"), "{}", list_error.message);
    assert!(list_error.message.contains("replace them with node.timeline_set"), "{}", list_error.message);

    // Establish a redo step in the reopened session. A refused edit must not clear it.
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": NodeId::from_bytes([90; 16]), "kind": "source.video", "pos": {"x": 0, "y": 300}})).unwrap();
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(s.can_redo());
    let before = (s.document().clone(), s.revision());
    let error = s.dispatch("timeline.update", &with(on("old0"), json!({"set": {"in_us": 100_000}}))).unwrap_err();
    assert!(error.message.contains("a timeline stores at most 128 clips"), "{}", error.message);
    assert_eq!((s.document(), s.revision()), (&before.0, before.1));
    assert!(s.can_redo());
    let saved_again = nodecraft_format::encode(&nodecraft_format::Snapshot { project: s.document().clone(), assets: Default::default(), epoch: 0 }).unwrap();
    let reopened = nodecraft_format::decode(&saved_again).unwrap();
    assert_eq!(reopened.project.canvases().get(&CANVAS).unwrap().nodes().get(&EDITOR).unwrap().config.get(CONFIG), Some(&Value::Text(original)));
    // As the message says, node.timeline_set replaces the unreadable clips.
    s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": []})).unwrap();
    assert!(stored(&node(&s)).unwrap().clips.is_empty());
}

#[test]
fn enormous_nested_result_stops_before_materializing_the_inner_list() {
    let mut source = node(&session());
    source.kind = "image.blur".into();
    source.outputs.first_mut().unwrap().name = "out".into();
    let id = nodecraft_types::GenerationId::from_bytes([88; 16]);
    source.active = Some(id);
    source.history.push(nodecraft_graph::Generation {
        id,
        outputs: vec![Value::List(vec![Value::List(vec![Value::Bool(false); 2_000_000])])],
        input_hash: None,
        items: vec![Default::default()],
        settings: Vec::new(),
        incomplete: None,
    });
    // A 2-million-item inner list previously made a 2-million-reference temporary Vec
    // before the outer take(129). Measure allocation so the old implementation fails
    // deterministically instead of depending on a machine's memory or timing.
    let mut result = Vec::new();
    let allocation = allocation_counter::measure(|| result = shown(&source, "out", Input::Clips));
    assert_eq!(result.len(), MAX_INPUTS + 1);
    assert!(allocation.count_total > 0, "allocation counter must observe the output vector");
    assert!(allocation.bytes_total <= 32 * 1024, "nested input allocation exceeded its bound: {allocation:?}");
}

#[test]
fn each_wired_video_is_listed_as_a_whole_clip_until_it_is_stored() {
    let mut s = session();
    let (a, b) = (timeline::auto_id(Input::Clips, &A.to_string(), 0), timeline::auto_id(Input::Clips, &B.to_string(), 0));
    assert_eq!(ids(&mut s), [a.clone(), b.clone()]);
    let listed = s.dispatch("timeline.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap();
    assert_eq!(
        listed["inputs"],
        json!([{"origin": A.to_string(), "input": "clips", "media": null}, {"origin": B.to_string(), "input": "clips", "media": null}])
    );
    let first = list(&mut s).remove(0);
    assert_eq!((&first["wired"], &first["media_us"], &first["volume"], &first["enabled"]), (&json!(true), &Json::Null, &json!(1000), &json!(true)));
    assert!(first.get("out_us").is_none());
    assert_eq!((&first["track"], &first["at_us"], &listed["tracks"]), (&json!(0), &json!(0), &json!([{"kind": "video"}])));
    assert_eq!(stored(&node(&s)).unwrap().clips.len(), 0);

    // Changing one stores both, in place; the video and id don't change.
    let trim = s.dispatch("timeline.update", &with(on(&b), json!({"set": {"in_us": 500_000, "out_us": 2_000_000, "fade_in_us": 250_000}}))).unwrap();
    assert_eq!(trim, json!({"changed": true}));
    assert_eq!(stored(&node(&s)).unwrap().clips.len(), 2);
    assert_eq!(list(&mut s)[1]["out_us"], json!(2_000_000));
    for bad in [json!({"origin": "x"}), json!({"id": "y"}), json!({"speed": 2}), json!({"out_us": 100_000}), json!({"transition_us": 5})] {
        assert!(s.dispatch("timeline.update", &with(on(&b), json!({"set": bad}))).is_err(), "{bad}");
    }
    // A null end plays to the video's end again.
    s.dispatch("timeline.update", &with(on(&b), json!({"set": {"out_us": null, "fade_in_us": 0}}))).unwrap();
    assert!(list(&mut s)[1].get("out_us").is_none());
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(list(&mut s)[1]["out_us"], json!(2_000_000));

    s.dispatch("timeline.move", &with(on(&b), json!({"index": 0}))).unwrap();
    assert_eq!(ids(&mut s), [b.clone(), a.clone()]);
    s.dispatch("timeline.move", &with(on(&b), json!({"index": 99}))).unwrap();
    assert_eq!(ids(&mut s), [a.clone(), b.clone()]);
    assert!(s.dispatch("timeline.move", &with(on("nope"), json!({"index": 0}))).is_err());
}

#[test]
fn a_split_plays_one_video_twice_and_a_wired_videos_last_clip_stays() {
    let mut s = session();
    let a = timeline::auto_id(Input::Clips, &A.to_string(), 0);
    // A clip whose end isn't known yet splits too: its second part plays to the video's end.
    s.dispatch("timeline.split", &with(on(&a), json!({"at_us": 1_000_000}))).unwrap();
    let clips = list(&mut s);
    assert_eq!((&clips[0]["out_us"], &clips[1]["in_us"], &clips[1]["out_us"]), (&json!(1_000_000), &json!(1_000_000), &Json::Null));
    s.dispatch("edit.undo", &json!({})).unwrap();
    s.dispatch(
        "timeline.update",
        &with(on(&a), json!({"set": {"out_us": 3_000_000, "fade_in_us": 100_000, "fade_out_us": 200_000, "transition_us": 300_000}})),
    )
    .unwrap();
    for at in [0, 3_000_000, 4_000_000] {
        assert!(s.dispatch("timeline.split", &with(on(&a), json!({"at_us": at}))).is_err(), "{at}");
    }
    let split = s.dispatch("timeline.split", &with(on(&a), json!({"at_us": 1_000_000}))).unwrap();
    let second = format!("{a}-2");
    assert_eq!(split, json!({"changed": true, "clip": second}));
    let clips = list(&mut s);
    assert_eq!(
        (&clips[0]["out_us"], &clips[0]["fade_out_us"], &clips[0]["transition_us"], &clips[0]["fade_in_us"]),
        (&json!(1_000_000), &json!(0), &json!(0), &json!(100_000))
    );
    assert_eq!(
        (&clips[1]["id"], &clips[1]["in_us"], &clips[1]["out_us"], &clips[1]["fade_in_us"]),
        (&json!(second), &json!(1_000_000), &json!(3_000_000), &json!(0))
    );
    assert_eq!((&clips[1]["fade_out_us"], &clips[1]["transition_us"]), (&json!(200_000), &json!(300_000)));
    assert_eq!(clips.len(), 3);

    // A cut inside a fade shortens it to the part it lands in.
    s.dispatch("timeline.update", &with(on(&second), json!({"set": {"fade_out_us": 1_500_000, "transition_us": 0}}))).unwrap();
    s.dispatch("timeline.update", &with(on(&a), json!({"set": {"fade_in_us": 900_000}}))).unwrap();
    let inner = s.dispatch("timeline.split", &with(on(&a), json!({"at_us": 500_000}))).unwrap()["clip"].as_str().unwrap().to_owned();
    let tail = s.dispatch("timeline.split", &with(on(&second), json!({"at_us": 2_500_000}))).unwrap()["clip"].as_str().unwrap().to_owned();
    let clips = list(&mut s);
    assert_eq!((&clips[0]["fade_in_us"], &clips[1]["id"], &clips[1]["fade_in_us"]), (&json!(500_000), &json!(inner), &json!(0)));
    assert_eq!((&clips[3]["id"], &clips[3]["fade_out_us"]), (&json!(tail), &json!(500_000)));
    for _ in 0..4 {
        s.dispatch("edit.undo", &json!({})).unwrap();
    }
    assert_eq!(list(&mut s).len(), 3);

    // One of the two clips can go; the last of a wired video can't, but can be disabled.
    s.dispatch("timeline.remove", &on(&second)).unwrap();
    let kept = s.dispatch("timeline.remove", &on(&a)).unwrap_err();
    assert!(kept.message.contains("disable"), "{}", kept.message);
    s.dispatch("timeline.update", &with(on(&a), json!({"set": {"enabled": false}}))).unwrap();
    assert_eq!(list(&mut s)[0]["enabled"], json!(false));

    // Once its wire is gone the clip is still listed, unwired, and can be deleted.
    s.dispatch("edge.disconnect", &json!({"canvas": CANVAS, "id": EdgeId::from_bytes([1; 16])})).unwrap();
    assert_eq!(list(&mut s)[0]["wired"], json!(false));
    s.dispatch("timeline.remove", &on(&a)).unwrap();
    assert_eq!(ids(&mut s), [timeline::auto_id(Input::Clips, &B.to_string(), 0)]);
}

#[test]
fn set_replaces_the_clips_and_refuses_bad_ones() {
    let mut s = session();
    let clips = json!([{"id": "intro", "origin": B.to_string(), "out_us": 1_000_000}, {"id": "gone", "origin": "elsewhere"}]);
    assert_eq!(s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": clips})).unwrap(), json!({"changed": true}));
    let listed = list(&mut s);
    let wired: Vec<(&str, bool)> = listed.iter().map(|c| (c["id"].as_str().unwrap(), c["wired"].as_bool().unwrap())).collect();
    let a = timeline::auto_id(Input::Clips, &A.to_string(), 0);
    assert_eq!(wired, [("intro", true), ("gone", false), (a.as_str(), true)]);
    assert_eq!(s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": clips})).unwrap(), json!({"changed": false}));
    let twice = json!([{"id": "x", "origin": "n"}, {"id": "x", "origin": "m"}]);
    let many: Vec<Json> = (0..=MAX_CLIPS).map(|i| json!({"id": format!("c{i}"), "origin": A.to_string()})).collect();
    assert_eq!(
        s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": many})).unwrap_err().message,
        "a timeline renders at most 32 clips: disable another first"
    );
    for bad in [twice, json!([{"id": "x", "origin": "n", "volume": 5000}])] {
        assert!(s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": bad})).is_err());
    }
    // Stored but disconnected takes do not consume a render slot.
    let offline: Vec<Json> = (0..40).map(|i| json!({"id": format!("offline{i}"), "origin": "absent"})).collect();
    s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": offline})).unwrap();
    assert_eq!(stored(&node(&s)).unwrap().clips.len(), 40);
    assert!(s.dispatch("timeline.list", &json!({"canvas": CANVAS, "id": A})).is_err(), "only a Timeline has clips");
}

const SOUND: NodeId = NodeId::from_bytes([6; 16]);
const PICTURE: NodeId = NodeId::from_bytes([7; 16]);

#[test]
fn sounds_and_pictures_get_clips_and_tracks_hold_them_at_their_own_time() {
    let mut s = session();
    for (n, id, kind, port) in [(3u8, SOUND, "source.audio", timeline::AUDIO_INPUT), (4, PICTURE, "source.image", timeline::STILLS_INPUT)] {
        s.dispatch("node.create", &json!({"canvas": CANVAS, "id": id, "kind": kind, "pos": {"x": 0, "y": 200}})).unwrap();
        let edge = json!({"id": EdgeId::from_bytes([n; 16]), "from": {"node": id, "port": "out"}, "to": {"node": EDITOR, "port": port}, "order": 0});
        s.dispatch("edge.connect", &json!({"canvas": CANVAS, "edge": edge})).unwrap();
    }
    let (a, b) = (timeline::auto_id(Input::Clips, &A.to_string(), 0), timeline::auto_id(Input::Clips, &B.to_string(), 0));
    let (sound, still) = (timeline::auto_id(Input::Audio, &SOUND.to_string(), 0), timeline::auto_id(Input::Stills, &PICTURE.to_string(), 0));
    assert_eq!(ids(&mut s), [a.clone(), b.clone(), sound.clone(), still.clone()]);
    let listed = s.dispatch("timeline.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap();
    // The sound gets an audio track of its own; the picture shows on the main track for 3 s.
    assert_eq!(listed["tracks"], json!([{"kind": "video"}, {"kind": "audio"}]));
    let clips = listed["clips"].as_array().unwrap();
    assert_eq!((&clips[2]["input"], &clips[2]["track"]), (&json!("audio"), &json!(1)));
    assert_eq!(
        (&clips[3]["input"], &clips[3]["track"], &clips[3]["out_us"], &clips[3]["media_us"]),
        (&json!("stills"), &json!(0), &json!(3_000_000), &Json::Null)
    );
    assert_eq!(listed["inputs"][2], json!({"origin": SOUND.to_string(), "input": "audio", "media": null}));

    // Within its track a clip moves among the clips on it.
    s.dispatch("timeline.move", &with(on(&still), json!({"index": 0}))).unwrap();
    assert_eq!(ids(&mut s), [still.clone(), a.clone(), b.clone(), sound.clone()]);

    // A video track over the rest takes the picture at its own time.
    let added = s.dispatch("timeline.track_add", &json!({"canvas": CANVAS, "id": EDITOR, "kind": "video"})).unwrap();
    assert_eq!(added, json!({"changed": true, "track": 2}));
    s.dispatch("timeline.update", &with(on(&still), json!({"set": {"track": 2, "at_us": 500_000}}))).unwrap();
    let placed = list(&mut s).remove(0);
    assert_eq!((&placed["track"], &placed["at_us"], &placed["transition_us"]), (&json!(2), &json!(500_000), &json!(0)));
    for bad in [json!({"track": 0, "at_us": 5}), json!({"track": 1}), json!({"track": 3}), json!({"input": "clips"})] {
        assert!(s.dispatch("timeline.update", &with(on(&still), json!({"set": bad}))).is_err(), "{bad}");
    }
    assert!(s.dispatch("timeline.update", &with(on(&sound), json!({"set": {"track": 0}}))).is_err(), "a sound goes on an audio track");
    assert!(s.dispatch("timeline.split", &with(on(&still), json!({"at_us": 1_000_000}))).is_err(), "a still doesn't split");
    // The sound splits on its track: the second part starts where the first ends.
    s.dispatch("timeline.update", &with(on(&sound), json!({"set": {"at_us": 1_000_000, "out_us": 2_000_000}}))).unwrap();
    let tail = s.dispatch("timeline.split", &with(on(&sound), json!({"at_us": 500_000}))).unwrap()["clip"].as_str().unwrap().to_owned();
    let tail = list(&mut s).into_iter().find(|c| c["id"] == json!(tail)).unwrap();
    assert_eq!((&tail["track"], &tail["at_us"], &tail["in_us"]), (&json!(1), &json!(1_500_000), &json!(500_000)));
    s.dispatch("edit.undo", &json!({})).unwrap();

    // Only an empty track goes, never the main one; the ones above it move down.
    let track = |n: u8| json!({"canvas": CANVAS, "id": EDITOR, "track": n});
    for n in [0, 2, 3] {
        assert!(s.dispatch("timeline.track_remove", &track(n)).is_err(), "{n}");
    }
    s.dispatch("timeline.update", &with(on(&still), json!({"set": {"track": 0}}))).unwrap();
    assert_eq!(list(&mut s).remove(0)["at_us"], json!(0), "back on the main track it plays in order");
    s.dispatch("timeline.track_add", &json!({"canvas": CANVAS, "id": EDITOR, "kind": "audio"})).unwrap();
    s.dispatch("timeline.update", &with(on(&sound), json!({"set": {"track": 3}}))).unwrap();
    s.dispatch("timeline.track_remove", &track(2)).unwrap();
    let timeline = stored(&node(&s)).unwrap();
    assert_eq!(timeline.tracks, [Track { kind: TrackKind::Audio }, Track { kind: TrackKind::Audio }]);
    assert_eq!(timeline.get(&sound).map(|c| c.track), Some(2));
    for _ in 0..MAX_TRACKS - 2 {
        s.dispatch("timeline.track_add", &json!({"canvas": CANVAS, "id": EDITOR, "kind": "video"})).unwrap();
    }
    assert!(s.dispatch("timeline.track_add", &json!({"canvas": CANVAS, "id": EDITOR, "kind": "video"})).is_err());
}

#[test]
fn a_card_made_before_sound_and_picture_inputs_runs_and_takes_them_when_wired() {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut legacy = kinds.get(ID).unwrap().instantiate_instance(EDITOR, Default::default(), &Default::default()).unwrap();
    legacy.inputs.truncate(1);
    Change::AddNode { canvas: CANVAS, node: legacy.clone() }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": A, "kind": "source.audio", "pos": {"x": 0, "y": 0}})).unwrap();
    assert!(s.dispatch("timeline.list", &json!({"canvas": CANVAS, "id": EDITOR})).is_ok());
    let edge =
        json!({"id": EdgeId::from_bytes([1; 16]), "from": {"node": A, "port": "out"}, "to": {"node": EDITOR, "port": timeline::AUDIO_INPUT}, "order": 0});
    s.dispatch("edge.connect", &json!({"canvas": CANVAS, "edge": edge})).unwrap();
    assert_eq!(node(&s).inputs, kinds.get(ID).unwrap().descriptor().inputs);
    assert_eq!(list(&mut s).len(), 1);
    // One edit: undo takes the wire and the upgrade back together.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(node(&s).inputs, legacy.inputs);
}

#[test]
fn a_clip_is_placed_and_sped_up_in_one_undo_step() {
    let mut s = session();
    let a = timeline::auto_id(Input::Clips, &A.to_string(), 0);
    let first = list(&mut s).remove(0);
    let defaults = ["pos_x", "pos_y", "scale", "crop_left", "crop_top", "crop_right", "crop_bottom", "rotation", "opacity", "speed"].map(|f| first[f].clone());
    assert_eq!(defaults, [500, 500, 1000, 0, 0, 0, 0, 0, 1000, 1000].map(|v| json!(v)), "every field is listed, defaults included");
    let place = json!({"pos_x": 250, "pos_y": -100, "scale": 500, "crop_left": 100, "crop_bottom": 200, "rotation": -45, "opacity": 700, "speed": 2000, "out_us": 4_000_000});
    s.dispatch("timeline.update", &with(on(&a), json!({"set": place}))).unwrap();
    let placed = stored(&node(&s)).unwrap().get(&a).cloned().unwrap();
    assert_eq!((placed.pos_x, placed.pos_y, placed.scale, placed.rotation, placed.opacity, placed.speed), (250, -100, 500, -45, 700, 2000));
    assert_eq!(placed.placement().crop, [100, 0, 0, 200]);
    assert_eq!(placed.played_us(), Some(2_000_000), "four seconds of video play in two");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(list(&mut s).remove(0)["speed"], json!(1000), "one undo step takes it all back");
    s.dispatch("edit.redo", &json!({})).unwrap();

    // Fades and transitions are in played time: two seconds now, so 1.5 s each is too long.
    assert!(s.dispatch("timeline.update", &with(on(&a), json!({"set": {"fade_in_us": 1_500_000, "fade_out_us": 1_500_000}}))).is_err());
    for bad in [
        json!({"speed": 249}),
        json!({"speed": 4001}),
        json!({"scale": 9}),
        json!({"pos_x": 2001}),
        json!({"rotation": 181}),
        json!({"opacity": 1001}),
        json!({"crop_left": 450, "crop_right": 450}),
        json!({"crop_top": 900}),
    ] {
        assert!(s.dispatch("timeline.update", &with(on(&a), json!({"set": bad}))).is_err(), "{bad}");
    }

    // A split keeps the place and speed, and cuts in media time: the second part starts on its
    // track where the first, played twice as fast, ends.
    s.dispatch("timeline.track_add", &json!({"canvas": CANVAS, "id": EDITOR, "kind": "video"})).unwrap();
    s.dispatch("timeline.update", &with(on(&a), json!({"set": {"track": 1, "at_us": 1_000_000, "fade_out_us": 2_000_000}}))).unwrap();
    let tail = s.dispatch("timeline.split", &with(on(&a), json!({"at_us": 3_000_000}))).unwrap()["clip"].as_str().unwrap().to_owned();
    let tail = stored(&node(&s)).unwrap().get(&tail).cloned().unwrap();
    assert_eq!((tail.at_us, tail.in_us, tail.speed, tail.rotation), (2_500_000, 3_000_000, 2000, -45));
    assert_eq!(tail.fade_out_us, 500_000, "the fade is cut to the half second the second part plays");
}

#[test]
fn sounds_take_no_place_and_stills_no_speed() {
    let mut s = session();
    for (n, id, kind, port) in [(3u8, SOUND, "source.audio", timeline::AUDIO_INPUT), (4, PICTURE, "source.image", timeline::STILLS_INPUT)] {
        s.dispatch("node.create", &json!({"canvas": CANVAS, "id": id, "kind": kind, "pos": {"x": 0, "y": 200}})).unwrap();
        let edge = json!({"id": EdgeId::from_bytes([n; 16]), "from": {"node": id, "port": "out"}, "to": {"node": EDITOR, "port": port}, "order": 0});
        s.dispatch("edge.connect", &json!({"canvas": CANVAS, "edge": edge})).unwrap();
    }
    let (sound, still) = (timeline::auto_id(Input::Audio, &SOUND.to_string(), 0), timeline::auto_id(Input::Stills, &PICTURE.to_string(), 0));
    assert!(s.dispatch("timeline.update", &with(on(&sound), json!({"set": {"pos_x": 100}}))).is_err(), "a sound has no picture");
    s.dispatch("timeline.update", &with(on(&sound), json!({"set": {"speed": 500}}))).unwrap();
    assert!(s.dispatch("timeline.update", &with(on(&still), json!({"set": {"speed": 500}}))).is_err(), "a still has a length, not a speed");
    s.dispatch("timeline.update", &with(on(&still), json!({"set": {"rotation": 90, "opacity": 0}}))).unwrap();
}

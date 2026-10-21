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
    let many: Vec<Json> = (0..=MAX_CLIPS).map(|i| json!({"id": format!("c{i}"), "origin": "n"})).collect();
    for bad in [twice, json!(many), json!([{"id": "x", "origin": "n", "volume": 5000}])] {
        assert!(s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": bad})).is_err());
    }
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

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
        let edge = json!({"id": EdgeId::from_bytes([n; 16]), "from": {"node": from, "port": "out"}, "to": {"node": EDITOR, "port": INPUT}, "order": n});
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
    let (a, b) = (timeline::auto_id(&A.to_string(), 0), timeline::auto_id(&B.to_string(), 0));
    assert_eq!(ids(&mut s), [a.clone(), b.clone()]);
    let listed = s.dispatch("timeline.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap();
    assert_eq!(listed["inputs"], json!([{"origin": A.to_string(), "video": null}, {"origin": B.to_string(), "video": null}]));
    let first = list(&mut s).remove(0);
    assert_eq!((&first["wired"], &first["video_us"], &first["volume"], &first["enabled"]), (&json!(true), &Json::Null, &json!(1000), &json!(true)));
    assert!(first.get("out_us").is_none());
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
    let a = timeline::auto_id(&A.to_string(), 0);
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
    assert_eq!(ids(&mut s), [timeline::auto_id(&B.to_string(), 0)]);
}

#[test]
fn set_replaces_the_clips_and_refuses_bad_ones() {
    let mut s = session();
    let clips = json!([{"id": "intro", "origin": B.to_string(), "out_us": 1_000_000}, {"id": "gone", "origin": "elsewhere"}]);
    assert_eq!(s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": clips})).unwrap(), json!({"changed": true}));
    let listed = list(&mut s);
    let wired: Vec<(&str, bool)> = listed.iter().map(|c| (c["id"].as_str().unwrap(), c["wired"].as_bool().unwrap())).collect();
    let a = timeline::auto_id(&A.to_string(), 0);
    assert_eq!(wired, [("intro", true), ("gone", false), (a.as_str(), true)]);
    assert_eq!(s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": clips})).unwrap(), json!({"changed": false}));
    let twice = json!([{"id": "x", "origin": "n"}, {"id": "x", "origin": "m"}]);
    let many: Vec<Json> = (0..=MAX_CLIPS).map(|i| json!({"id": format!("c{i}"), "origin": "n"})).collect();
    for bad in [twice, json!(many), json!([{"id": "x", "origin": "n", "volume": 5000}])] {
        assert!(s.dispatch("node.timeline_set", &json!({"canvas": CANVAS, "id": EDITOR, "clips": bad})).is_err());
    }
    assert!(s.dispatch("timeline.list", &json!({"canvas": CANVAS, "id": A})).is_err(), "only a Timeline has clips");
}

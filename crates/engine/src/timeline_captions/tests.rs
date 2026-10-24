use super::*;
use crate::Session;
use nodecraft_graph::{Change, Node};
use nodecraft_types::ProjectId;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const EDITOR: NodeId = NodeId::from_bytes([5; 16]);

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": EDITOR, "kind": nodecraft_nodes::timeline::ID, "pos": {"x": 0, "y": 0}})).unwrap();
    s
}

fn node(s: &Session) -> Node {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&EDITOR).unwrap().clone()
}

fn captions(s: &mut Session) -> Json {
    s.dispatch("timeline.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap()["captions"].clone()
}

fn with(extra: Json) -> Json {
    let mut base = json!({"canvas": CANVAS, "id": EDITOR});
    if let (Json::Object(base), Json::Object(extra)) = (&mut base, extra) {
        base.extend(extra);
    }
    base
}

#[test]
fn captions_are_added_changed_and_removed_as_undo_steps() {
    let mut s = session();
    assert_eq!(captions(&mut s), json!([]));
    let added = s.dispatch("timeline.caption_add", &with(json!({"text": "Hello", "start_us": 0, "end_us": 1_000_000}))).unwrap();
    assert_eq!(added, json!({"changed": true, "caption": "caption-1"}));
    let styled = with(
        json!({"text": "Two\nlines", "start_us": 500_000, "end_us": 2_000_000, "size": 64, "align": "left", "position": "top", "color": "#ffcc00", "stroke_width": 4}),
    );
    assert_eq!(s.dispatch("timeline.caption_add", &styled).unwrap()["caption"], json!("caption-2"));
    let listed = captions(&mut s);
    assert_eq!(listed[0]["font"], json!("lato"));
    assert_eq!(
        (&listed[0]["size"], &listed[0]["align"], &listed[0]["position"], &listed[0]["enabled"]),
        (&json!(48), &json!("center"), &json!("bottom"), &json!(true))
    );
    assert_eq!(
        (&listed[1]["size"], &listed[1]["align"], &listed[1]["color"], &listed[1]["stroke_width"]),
        (&json!(64), &json!("left"), &json!("#ffcc00"), &json!(4))
    );
    assert_eq!(stored(&node(&s)).unwrap().captions.len(), 2);

    s.dispatch("timeline.caption_update", &with(json!({"caption": "caption-1", "set": {"text": "Hi", "enabled": false}}))).unwrap();
    let listed = captions(&mut s);
    assert_eq!((&listed[0]["text"], &listed[0]["enabled"], &listed[0]["start_us"]), (&json!("Hi"), &json!(false), &json!(0)));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(captions(&mut s)[0]["text"], json!("Hello"));

    s.dispatch("timeline.caption_remove", &with(json!({"caption": "caption-1"}))).unwrap();
    assert_eq!(captions(&mut s).as_array().unwrap().len(), 1);
    // A fresh id skips the one still in use.
    assert_eq!(s.dispatch("timeline.caption_add", &with(json!({"text": "Back", "start_us": 0, "end_us": 10}))).unwrap()["caption"], json!("caption-1"));
    s.dispatch("edit.undo", &json!({})).unwrap();
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(captions(&mut s).as_array().unwrap().len(), 2);

    // Captions are stored with the timeline, so they save and load with it.
    let kept = stored(&node(&s)).unwrap();
    assert_eq!(kept.captions.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["Hello", "Two\nlines"]);
    let snapshot = nodecraft_format::Snapshot { project: s.document().clone(), assets: nodecraft_format::AssetStore::default(), epoch: 0 };
    let loaded = nodecraft_format::decode(&nodecraft_format::encode(&snapshot).unwrap()).unwrap();
    let node = loaded.project.canvases().get(&CANVAS).unwrap().nodes().get(&EDITOR).unwrap();
    assert_eq!(stored(node).unwrap().captions, kept.captions);
}

#[test]
fn bad_captions_are_refused_and_change_nothing() {
    let mut s = session();
    s.dispatch("timeline.caption_add", &with(json!({"caption": "intro", "text": "Hi", "start_us": 0, "end_us": 10}))).unwrap();
    let before = node(&s);
    for bad in [
        json!({"text": "", "start_us": 0, "end_us": 10}),
        json!({"text": " ", "start_us": 0, "end_us": 10}),
        json!({"text": "x", "start_us": 10, "end_us": 10}),
        json!({"text": "x", "start_us": 0, "end_us": 4_000_000_000_u64}),
        json!({"text": "x", "start_us": 0}),
        json!({"text": "x", "start_us": 0, "end_us": 10, "size": 2}),
        json!({"text": "x", "start_us": 0, "end_us": 10, "size": 1.5}),
        json!({"text": "x", "start_us": 0, "end_us": 10, "color": "red"}),
        json!({"text": "x", "start_us": 0, "end_us": 10, "stroke_width": 33}),
        json!({"text": "x", "start_us": 0, "end_us": 10, "align": "justify"}),
        json!({"text": "x", "start_us": 0, "end_us": 10, "speed": 2}),
        json!({"text": "x", "start_us": 0, "end_us": 10, "font": ""}),
        json!({"text": "a\nb\nc\nd\ne\nf\ng\nh\ni", "start_us": 0, "end_us": 10}),
        json!({"text": "\u{7}", "start_us": 0, "end_us": 10}),
        json!({"text": "x".repeat(401), "start_us": 0, "end_us": 10}),
        json!({"caption": "intro", "text": "x", "start_us": 0, "end_us": 10}),
        json!({"caption": "no spaces", "text": "x", "start_us": 0, "end_us": 10}),
    ] {
        assert!(s.dispatch("timeline.caption_add", &with(bad.clone())).is_err(), "{bad}");
    }
    for bad in [
        json!({"caption": "intro", "set": {"id": "other"}}),
        json!({"caption": "intro", "set": {"end_us": 0}}),
        json!({"caption": "intro", "set": {"nope": 1}}),
        json!({"caption": "missing", "set": {"text": "y"}}),
        json!({"caption": "intro"}),
    ] {
        assert!(s.dispatch("timeline.caption_update", &with(bad.clone())).is_err(), "{bad}");
    }
    assert!(s.dispatch("timeline.caption_remove", &with(json!({"caption": "missing"}))).is_err());
    assert!(s.dispatch("timeline.caption_remove", &json!({"canvas": CANVAS, "id": NodeId::from_bytes([9; 16]), "caption": "intro"})).is_err());
    assert_eq!(node(&s), before);

    // At most 64.
    for n in 1..MAX_CAPTIONS {
        s.dispatch("timeline.caption_add", &with(json!({"text": format!("{n}"), "start_us": 0, "end_us": 10}))).unwrap();
    }
    assert!(s.dispatch("timeline.caption_add", &with(json!({"text": "one more", "start_us": 0, "end_us": 10}))).is_err());
}

#[test]
fn the_stored_timeline_keeps_captions_through_timeline_set() {
    let mut s = session();
    s.dispatch("timeline.caption_add", &with(json!({"text": "Kept", "start_us": 0, "end_us": 10}))).unwrap();
    // A set without captions keeps them; one with an empty list clears them.
    s.dispatch("node.timeline_set", &with(json!({"clips": []}))).unwrap();
    assert_eq!(captions(&mut s)[0]["text"], json!("Kept"));
    s.dispatch("node.timeline_set", &with(json!({"clips": [], "captions": []}))).unwrap();
    assert_eq!(captions(&mut s), json!([]));
    let dup = json!({"id": "a", "text": "x", "start_us": 0, "end_us": 10});
    assert!(s.dispatch("node.timeline_set", &with(json!({"clips": [], "captions": [dup.clone(), dup]}))).is_err());
}

#[test]
fn timeline_set_without_captions_still_replaces_an_unreadable_timeline() {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    use nodecraft_nodes::NodeKind;
    let mut editor = nodecraft_nodes::timeline::kind().descriptor().instantiate(EDITOR, &std::collections::BTreeMap::new()).unwrap();
    editor.config.insert(nodecraft_nodes::timeline::CONFIG.into(), nodecraft_types::Value::Text("{not json".into()));
    Change::AddNode { canvas: CANVAS, node: editor }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    assert!(s.dispatch("timeline.caption_add", &with(json!({"text": "x", "start_us": 0, "end_us": 10}))).is_err());
    s.dispatch("node.timeline_set", &with(json!({"clips": []}))).unwrap();
    assert_eq!(captions(&mut s), json!([]));
}

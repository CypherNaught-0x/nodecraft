mod common;
use common::*;
use nodecraft_engine::Session;
use nodecraft_types::FieldId;
use serde_json::json;
fn field() -> FieldId {
    FieldId::from_bytes([9; 16])
}
fn create() -> serde_json::Value {
    json!({"canvas":canvas(),"field":{"id":field(),"title":"Group","rect":{"pos":{"x":0,"y":0},"width":320,"height":240},"color":"blue"},"nodes":[node_id(1),node_id(2)]})
}
#[test]
fn lifecycle_one_undo_noop_and_unrelated_payload_preservation() {
    let original = project();
    let mut s = Session::new(original.clone()).unwrap();
    let p = create();
    s.enabled("field.create", &p).unwrap();
    s.dispatch("field.create", &p).unwrap();
    let grouped = s.document().clone();
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &original);
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(s.document(), &grouped);
    let revision = s.revision();
    s.dispatch("field.move", &json!({"canvas":canvas(),"field":field(),"delta":{"x":0,"y":0}})).unwrap();
    assert_eq!(s.revision(), revision);
    s.dispatch("field.move", &json!({"canvas":canvas(),"field":field(),"delta":{"x":13.25,"y":-4.5}})).unwrap();
    let moved = s.document().clone();
    for n in grouped.canvases().get(&canvas()).unwrap().nodes().values() {
        let actual = moved.canvases().get(&canvas()).unwrap().nodes().get(&n.id).unwrap();
        let mut expected = n.clone();
        expected.pos.x += 13.25;
        expected.pos.y -= 4.5;
        assert_eq!(actual, &expected);
    }
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &grouped);
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(s.document(), &moved);
    s.dispatch("field.remove", &json!({"canvas":canvas(),"field":field()})).unwrap();
    assert!(s.document().canvases().get(&canvas()).unwrap().nodes().values().all(|n| n.field.is_none()));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &moved);
}
#[test]
fn strict_invalid_commands_and_failed_geometry_leave_session_unchanged() {
    let mut s = Session::new(project()).unwrap();
    s.dispatch("field.create", &create()).unwrap();
    let before = s.document().clone();
    let revision = s.revision();
    for (cmd, p) in [
        ("field.update", json!({"canvas":canvas(),"field":field(),"rect":null})),
        ("field.update", json!({"canvas":canvas(),"field":field(),"title":null})),
        ("field.update", json!({"canvas":canvas(),"field":field(),"rect":{"pos":{"x":1e7,"y":0},"width":1,"height":10}})),
        ("field.move", json!({"canvas":canvas(),"field":field(),"delta":{"x":1e30,"y":0}})),
        ("node.field_set", json!({"canvas":canvas(),"nodes":[node_id(1)]})),
        ("node.field_set", json!({"canvas":canvas(),"nodes":[node_id(1),node_id(1)],"field":null})),
        ("node.field_set", json!({"canvas":canvas(),"nodes":[node_id(99)],"field":null})),
    ] {
        assert!(s.enabled(cmd, &p).is_err());
        assert!(s.dispatch(cmd, &p).is_err());
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), revision);
    }
}
#[test]
fn collapse_resize_color_patch_and_empty_zone_are_persistent() {
    let mut s = Session::new(project()).unwrap();
    let mut p = create();
    p["nodes"] = json!([]);
    s.dispatch("field.create", &p).unwrap();
    let before = s.document().clone();
    s.dispatch("field.update", &json!({"canvas":canvas(),"field":field(),"collapsed":true,"color":null})).unwrap();
    let f = s.document().canvases().get(&canvas()).unwrap().fields().get(&field()).unwrap();
    assert!(f.collapsed);
    assert!(f.color.is_none());
    assert_eq!(f.rect.width, 320.0);
    let restored: nodecraft_graph::Project = serde_json::from_slice(&serde_json::to_vec(s.document()).unwrap()).unwrap();
    assert_eq!(&restored, s.document());
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
}

#[test]
fn nested_unknown_coordinates_are_rejected() {
    let mut s = Session::new(project()).unwrap();
    let mut p = create();
    p["field"]["rect"]["pos"]["typo"] = json!(1);
    assert!(s.dispatch("field.create", &p).is_err());
    s.dispatch("field.create", &create()).unwrap();
    let before = s.document().clone();
    for (cmd, p) in [
        ("field.move", json!({"canvas":canvas(),"field":field(),"delta":{"x":0,"y":0,"typo":1}})),
        ("field.update", json!({"canvas":canvas(),"field":field(),"rect":{"pos":{"x":0,"y":0,"typo":1},"width":10,"height":10}})),
    ] {
        assert!(s.enabled(cmd, &p).is_err());
        assert!(s.dispatch(cmd, &p).is_err());
        assert_eq!(s.document(), &before);
    }
}
#[test]
fn ten_thousand_members_move_remove_and_restore_in_one_step() {
    use nodecraft_graph::{Canvas, Change, Changeset, Node, Project};
    use nodecraft_types::{CanvasId, NodeId, ProjectId};
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Maximum").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut p).unwrap();
    let ids: Vec<_> = (0..10000u128).map(|n| NodeId::from_bytes(n.to_be_bytes())).collect();
    Changeset::new(ids.iter().map(|id| Change::AddNode { canvas, node: Node::new(*id, "test.node") }).collect()).apply(&mut p).unwrap();
    let mut s = Session::new(p).unwrap();
    s.dispatch(
        "field.create",
        &json!({"canvas":canvas,"field":{"id":field(),"title":"Maximum","rect":{"pos":{"x":0,"y":0},"width":320,"height":240}},"nodes":ids}),
    )
    .unwrap();
    let grouped = s.document().clone();
    s.dispatch("field.move", &json!({"canvas":canvas,"field":field(),"delta":{"x":7.25,"y":-2.5}})).unwrap();
    let moved = s.document().clone();
    assert!(moved.canvases().get(&canvas).unwrap().nodes().values().all(|n| n.pos.x == 7.25 && n.pos.y == -2.5));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &grouped);
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(s.document(), &moved);
    s.dispatch("field.remove", &json!({"canvas":canvas,"field":field()})).unwrap();
    assert!(s.document().canvases().get(&canvas).unwrap().nodes().values().all(|n| n.field.is_none()));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &moved);
}

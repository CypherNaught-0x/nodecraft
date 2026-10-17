mod common;
use nodecraft_engine::{Session, layout_cmds::LayoutContext};
use nodecraft_graph::{Change, Position};
use serde_json::json;
fn session() -> Session {
    Session::new(common::project()).unwrap()
}
fn host(s: &mut Session) {
    s.swap_resource(Some(common::layout_context(s)));
}
#[test]
fn both_modes_are_one_atomic_undo_step_preserving_unselected_data_and_noop_redo() {
    for mode in ["grid", "layered"] {
        let mut s = session();
        s.dispatch("node.add", &json!({"canvas":common::canvas(),"node":common::node(3)})).unwrap();
        let original = s.document().clone();
        let revision = s.revision();
        host(&mut s);
        let p = json!({"mode":mode});
        s.enabled("canvas.auto_format", &p).unwrap();
        s.dispatch("canvas.auto_format", &p).unwrap();
        assert_eq!(s.revision(), revision + 1);
        let after = s.document().clone();
        assert_ne!(after, original);
        assert_eq!(
            after.canvases().get(&common::canvas()).unwrap().nodes().get(&common::node_id(3)),
            original.canvases().get(&common::canvas()).unwrap().nodes().get(&common::node_id(3))
        );
        assert_eq!(after.canvases().get(&common::canvas()).unwrap().edges(), original.canvases().get(&common::canvas()).unwrap().edges());
        host(&mut s);
        let events = s.events_since(0).unwrap();
        assert_eq!(s.dispatch("canvas.auto_format", &p).unwrap(), json!({"moved":0}));
        assert_eq!(s.events_since(0).unwrap(), events);
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), &original);
        s.dispatch("edit.redo", &json!({})).unwrap();
        assert_eq!(s.document(), &after);
        // A no-op after undo of an unrelated edit must preserve redo.
        s.dispatch("node.title", &json!({"canvas":common::canvas(),"id":common::node_id(3),"title":"Temporary"})).unwrap();
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert!(s.can_redo());
        host(&mut s);
        s.dispatch("canvas.auto_format", &p).unwrap();
        assert!(s.can_redo());
    }
}
#[test]
fn stale_host_after_nonpositional_edit_and_invalid_contexts_are_atomic() {
    let mut s = session();
    assert!(s.dispatch("canvas.auto_format", &json!({"mode":"grid"})).is_err());
    host(&mut s);
    s.dispatch("node.title", &json!({"canvas":common::canvas(),"id":common::node_id(1),"title":"Changed geometry context"})).unwrap();
    let before = s.document().clone();
    let rev = s.revision();
    let events = s.events_since(0).unwrap();
    assert!(s.dispatch("canvas.auto_format", &json!({"mode":"grid"})).is_err());
    for n in 0..8 {
        let mut h = common::layout_context(&s);
        match n {
            0 => h.extents.get_mut(1).unwrap().width = f64::INFINITY,
            1 => h.selection.push(common::node_id(1)),
            2 => h.extents.get_mut(1).unwrap().id = common::node_id(3),
            3 => h.blocked = true,
            4 => h.gap = 0.0,
            5 => h.extents.get_mut(1).unwrap().left = 1e100,
            6 => {
                h.extents.pop();
            }
            _ => h.extents.get_mut(1).unwrap().height = f64::NAN,
        }
        s.swap_resource(Some(h));
        assert!(s.enabled("canvas.auto_format", &json!({"mode":"grid"})).is_err());
        assert!(s.dispatch("canvas.auto_format", &json!({"mode":"grid"})).is_err());
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), rev);
        assert_eq!(s.events_since(0).unwrap(), events);
    }
    host(&mut s);
    for p in [json!({"mode":"bad"}), json!({"mode":"layered","columns":2}), json!({"mode":"grid","columns":0}), json!({"mode":"grid","columns":3})] {
        assert!(s.dispatch("canvas.auto_format", &p).is_err());
    }
}
#[test]
fn late_range_failure_leaves_history_and_document_unchanged() {
    let mut p = common::project();
    for id in [common::node_id(1), common::node_id(2)] {
        Change::MoveNode { canvas: common::canvas(), id, pos: Position { x: 9_999_990.0, y: 0.0 } }.apply(&mut p).unwrap();
    }
    let mut s = Session::new(p.clone()).unwrap();
    host(&mut s);
    // Enabled is structural/advisory: final output-coordinate bounds are checked once by dispatch.
    assert!(s.enabled("canvas.auto_format", &json!({"mode":"grid"})).is_ok());
    assert!(s.dispatch("canvas.auto_format", &json!({"mode":"grid"})).is_err());
    assert_eq!(s.document(), &p);
    assert_eq!(s.revision(), 0);
    assert!(!s.can_undo());
    let h = s.swap_resource::<LayoutContext>(None).unwrap();
    assert_eq!(h.revision, 0);
}

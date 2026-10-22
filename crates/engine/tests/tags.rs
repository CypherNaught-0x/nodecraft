use crate::common;
use common::*;
use nodecraft_engine::Session;
use nodecraft_types::{ErrorKind, TagId};
use serde_json::json;
fn tag() -> TagId {
    TagId::from_bytes([9; 16])
}
#[test]
fn create_assign_remove_are_atomic_single_undo_and_preserve_payloads() {
    let original = project();
    let mut s = Session::new(original.clone()).unwrap();
    let p = json!({"canvas":canvas(),"nodes":[node_id(2),node_id(1),node_id(1)],"tag":tag(),"create":{"id":tag(),"title":"Blue","color":"blue"}});
    s.enabled("node.tag_set", &p).unwrap();
    assert_eq!(s.dispatch("node.tag_set", &p).unwrap(), json!({"affected":2}));
    let tagged = s.document().clone();
    assert_eq!(s.revision(), 1);
    s.dispatch("node.tag_set", &json!({"canvas":canvas(),"nodes":[node_id(1)],"tag":tag()})).unwrap();
    assert_eq!(s.revision(), 1);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &original);
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(s.document(), &tagged);
    s.dispatch("tag.remove", &json!({"canvas":canvas(),"tag":tag()})).unwrap();
    assert_eq!(s.document(), &original);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &tagged);
}
#[test]
fn malformed_or_missing_tag_requests_preserve_session() {
    let mut s = Session::new(project()).unwrap();
    let original = s.document().clone();
    for p in [
        json!({"canvas":canvas(),"nodes":[node_id(1)]}),
        json!({"canvas":canvas(),"nodes":[],"tag":null}),
        json!({"canvas":canvas(),"nodes":[node_id(99)],"tag":null}),
        json!({"canvas":canvas(),"nodes":[node_id(1)],"tag":tag()}),
        json!({"canvas":canvas(),"nodes":[node_id(1)],"tag":tag(),"create":{"id":tag(),"title":"Blue","secret":"must-not-echo"}}),
        json!({"canvas":canvas(),"nodes":[node_id(1)],"tag":tag(),"create":{"id":tag(),"title":"Blue","color":"x".repeat(65)}}),
        json!({"canvas":canvas(),"nodes":vec![node_id(1);10001],"tag":null}),
    ] {
        assert!(s.enabled("node.tag_set", &p).is_err());
        let e = s.dispatch("node.tag_set", &p).unwrap_err();
        assert!(!e.to_string().contains("must-not-echo"));
        assert_eq!(s.document(), &original);
        assert_eq!(s.revision(), 0);
    }
    assert_eq!(s.dispatch("tag.remove", &json!({"canvas":canvas(),"tag":tag()})).unwrap_err().kind, ErrorKind::NotFound);
}
#[test]
fn unknown_colors_round_trip_and_metadata_edits_undo() {
    let mut s = Session::new(project()).unwrap();
    let p = json!({"canvas":canvas(),"tag":{"id":tag(),"title":"Custom","color":"future-palette"}});
    s.dispatch("tag.put", &p).unwrap();
    let before = s.document().clone();
    s.dispatch("tag.put", &json!({"canvas":canvas(),"tag":{"id":tag(),"title":"Renamed"}})).unwrap();
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
    let bytes = serde_json::to_vec(s.document()).unwrap();
    let restored: nodecraft_graph::Project = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(&restored, s.document());
}

#[test]
fn varied_previous_tags_restore_in_one_step_and_metadata_byte_boundaries_are_exact() {
    let mut s = Session::new(project()).unwrap();
    let other = TagId::from_bytes([8; 16]);
    for (id, color, title) in [(tag(), "x".repeat(64), "界".repeat(1365) + "x"), (other, "custom".into(), "Other".into())] {
        s.dispatch("tag.put", &json!({"canvas":canvas(),"tag":{"id":id,"title":title,"color":color}})).unwrap();
    }
    s.dispatch("node.tag_set", &json!({"canvas":canvas(),"nodes":[node_id(1)],"tag":tag()})).unwrap();
    s.dispatch("node.tag_set", &json!({"canvas":canvas(),"nodes":[node_id(2)],"tag":other})).unwrap();
    let before = s.document().clone();
    s.dispatch("node.tag_set", &json!({"canvas":canvas(),"nodes":[node_id(1),node_id(2)],"tag":null})).unwrap();
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
    for (color, title) in [
        ("x".repeat(65), "ok".into()),
        ("界".repeat(21), "ok".into()),
        ("".into(), "ok".into()),
        ("Upper".into(), "ok".into()),
        ("red".into(), "界".repeat(1366)),
    ] {
        assert!(s.dispatch("tag.put", &json!({"canvas":canvas(),"tag":{"id":tag(),"title":title,"color":color}})).is_err());
        assert_eq!(s.document(), &before);
    }
}
#[test]
fn view_commands_have_strict_parameters_and_no_headless_side_effects() {
    use nodecraft_engine::tag_filter_cmds::{SERVICE, TagFilterContext};
    let mut s = Session::new(project()).unwrap();
    assert_eq!(s.dispatch("canvas.tag_filter_open", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
    s.register_host_service(SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
    s.swap_resource(Some(TagFilterContext {
        canvas: canvas(),
        viewport_available: true,
        blocked: false,
        open: true,
        active: true,
        scanning: false,
        matches: 1,
    }));
    for p in [json!({"tags":[],"untagged":false,"hidden":1}), json!({"tags":[tag()],"untagged":false}), json!({"tags":vec![tag();10001],"untagged":false})] {
        assert!(s.dispatch("canvas.tag_filter_set", &p).is_err());
    }
    s.dispatch("canvas.tag_filter_set", &json!({"tags":[],"untagged":true})).unwrap();
    s.dispatch("canvas.tag_filter_next", &json!({})).unwrap();
    assert_eq!(s.revision(), 0);
    assert!(!s.can_undo());
}

#[test]
fn command_colours_survive_real_v3_archive_and_journal_with_comments() {
    use nodecraft_format::{MemoryStorage, ProjectStorage, Snapshot, StorageLocation, journal};
    use nodecraft_types::{CommentId, Error};
    use std::{cell::RefCell, rc::Rc};
    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<MemoryStorage>>);
    impl ProjectStorage for Shared {
        fn load(&mut self, p: &StorageLocation) -> Result<Vec<u8>, Error> {
            self.0.borrow_mut().load(p)
        }
        fn save(&mut self, p: &StorageLocation, b: &[u8]) -> Result<(), Error> {
            self.0.borrow_mut().save(p, b)
        }
        fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
            self.0.borrow_mut().checkpoint(b)
        }
        fn append(&mut self, b: &[u8]) -> Result<(), Error> {
            self.0.borrow_mut().append(b)
        }
    }
    let storage = Shared::default();
    let mut s = Session::with_storage(Snapshot { project: project(), assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    s.dispatch("comment.create", &json!({"canvas":canvas(),"id":CommentId::from_bytes([20;16]),"node":node_id(1),"pos":{"x":4,"y":8},"text":"Keep lighting"}))
        .unwrap();
    s.dispatch("node.tag_set", &json!({"canvas":canvas(),"nodes":[node_id(1),node_id(2)],"tag":tag(),"create":{"id":tag(),"title":"Blue","color":"blue"}}))
        .unwrap();
    s.dispatch("tag.put", &json!({"canvas":canvas(),"tag":{"id":tag(),"title":"Lighting","color":"future-blue"}})).unwrap();
    let accepted = s.document().clone();
    let recovered = journal::recover(&storage.0.borrow().recovery).unwrap();
    assert_eq!(recovered.snapshot.project, accepted);
    assert!(!recovered.legacy);
    assert_eq!(&storage.0.borrow().recovery[..8], b"NCRJ0003");
    s.dispatch("edit.undo", &json!({})).unwrap();
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(s.document(), &accepted);
    s.dispatch("file.save_as", &json!({"location":"public-tags.ncraft"})).unwrap();
    let path = StorageLocation::from_command("public-tags.ncraft".into()).unwrap();
    assert_eq!(nodecraft_format::decode(storage.0.borrow().files.get(&path).unwrap()).unwrap().project, accepted);
    assert_eq!(journal::recover(&storage.0.borrow().recovery).unwrap().snapshot.project, accepted);
}

#[test]
fn tag_put_replaces_color_and_undo_restores_it() {
    let mut s = Session::new(project()).unwrap();
    s.dispatch("tag.put", &json!({"canvas":canvas(),"tag":{"id":tag(),"title":"Blue","color":"blue"}})).unwrap();
    s.dispatch("tag.put", &json!({"canvas":canvas(),"tag":{"id":tag(),"title":"Renamed"}})).unwrap();
    assert_eq!(s.document().canvases().get(&canvas()).unwrap().tags().get(&tag()).unwrap().color, None);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document().canvases().get(&canvas()).unwrap().tags().get(&tag()).unwrap().color.as_deref(), Some("blue"));
}

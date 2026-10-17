mod common;
use nodecraft_engine::Session;
use nodecraft_types::{CommentId, NodeId};
use serde_json::json;
fn id(n: u8) -> CommentId {
    CommentId::from_bytes([n; 16])
}
fn create(n: u8, text: &str) -> serde_json::Value {
    json!({"canvas":common::canvas(),"id":id(n),"text":text,"pos":{"x":100,"y":200}})
}
#[test]
fn discussion_commands_are_local_invertible_and_revision_checked() {
    let mut s = Session::new(common::project()).unwrap();
    let before = s.document().clone();
    s.dispatch("comment.create", &create(10, "Root")).unwrap();
    let created = s.document().clone();
    let revision = s.revision();
    s.dispatch("comment.create", &create(10, "Root")).unwrap();
    assert_eq!(s.revision(), revision);
    s.dispatch("comment.reply", &json!({"canvas":common::canvas(),"id":id(10),"reply":id(11),"text":"Reply","expected_revision":revision})).unwrap();
    let replied = s.document().clone();
    s.dispatch("comment.resolve", &json!({"canvas":common::canvas(),"id":id(10),"resolved":true})).unwrap();
    assert!(s.dispatch("comment.reply", &json!({"canvas":common::canvas(),"id":id(10),"reply":id(12),"text":"blocked"})).is_err());
    assert!(s.dispatch("comment.edit", &json!({"canvas":common::canvas(),"id":id(10),"text":"stale","expected_revision":revision})).is_err());
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &replied);
    s.dispatch("comment.reply_remove", &json!({"canvas":common::canvas(),"id":id(10),"reply":id(11)})).unwrap();
    assert_eq!(s.document(), &created);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &replied);
    s.dispatch("comment.remove", &json!({"canvas":common::canvas(),"id":id(10)})).unwrap();
    assert_eq!(s.document(), &before);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &replied);
    let unchanged = s.document().clone();
    let revision = s.revision();
    for bad in [json!({"actor":"admin"}), json!({"role":"owner"}), json!({"text":"   "})] {
        let mut v = create(20, "Good");
        for (k, value) in bad.as_object().unwrap() {
            v[k] = value.clone();
        }
        assert!(s.dispatch("comment.create", &v).is_err());
        assert_eq!(s.document(), &unchanged);
        assert_eq!(s.revision(), revision);
    }
    assert!(s.dispatch("comment.open", &json!({"id":id(10)})).is_err());
}
#[test]
fn exact_one_mebibyte_root_and_note_are_fully_editable_but_content_overflow_rejects() {
    let mut s = Session::new(common::project()).unwrap();
    let max = "x".repeat(1024 * 1024);
    s.dispatch("comment.create", &create(10, &max)).unwrap();
    let edited = "y".repeat(1024 * 1024);
    s.dispatch("comment.edit", &json!({"canvas":common::canvas(),"id":id(10),"text":edited})).unwrap();
    assert!(s.dispatch("comment.edit", &json!({"canvas":common::canvas(),"id":id(10),"text":format!("{max}x")})).is_err());
    let node = NodeId::from_bytes([30; 16]);
    s.dispatch("node.add", &json!({"canvas":common::canvas(),"node":{"id":node,"kind":"structure.note"}})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas":common::canvas(),"id":node,"name":"text","value":{"type":"text","value":max}})).unwrap();
    assert!(
        s.dispatch("node.param_set", &json!({"canvas":common::canvas(),"id":node,"name":"text","value":{"type":"text","value":"x".repeat(1024*1024+1)}}))
            .is_err()
    );
    assert!(s.dispatch("comment.create", &create(20, &"x".repeat(1024 * 1024 + 1024))).is_err());
}

#[test]
fn legacy_open_conversion_checkpoint_failure_preserves_live_state_then_v3_edits_recover() {
    use nodecraft_format::{MemoryStorage, ProjectStorage, Snapshot, StorageLocation, journal};
    use nodecraft_types::{Error, ErrorKind};
    use std::{cell::RefCell, rc::Rc};
    #[derive(Default)]
    struct State {
        memory: MemoryStorage,
        fail: bool,
    }
    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<State>>);
    impl ProjectStorage for Shared {
        fn load(&mut self, p: &StorageLocation) -> Result<Vec<u8>, Error> {
            self.0.borrow_mut().memory.load(p)
        }
        fn save(&mut self, p: &StorageLocation, b: &[u8]) -> Result<(), Error> {
            self.0.borrow_mut().memory.save(p, b)
        }
        fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
            let mut s = self.0.borrow_mut();
            if s.fail {
                return Err(Error::new(ErrorKind::Conflict, "injected full disk"));
            }
            s.memory.checkpoint(b)
        }
        fn append(&mut self, b: &[u8]) -> Result<(), Error> {
            self.0.borrow_mut().memory.append(b)
        }
    }
    for bytes in [
        include_bytes!("../../format/tests/fixtures/v1/comments-v1.ncraft").as_slice(),
        include_bytes!("../../format/tests/fixtures/v2/comments-v2.ncraft").as_slice(),
    ] {
        let storage = Shared::default();
        storage.0.borrow_mut().memory.files.insert(StorageLocation::from_command("legacy.ncraft".into()).unwrap(), bytes.to_vec());
        let mut s = Session::with_storage(Snapshot { project: common::project(), assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
        let before = s.document().clone();
        let old_journal = storage.0.borrow().memory.recovery.clone();
        let old_status = s.file_status();
        storage.0.borrow_mut().fail = true;
        assert!(s.dispatch("file.open", &json!({"location":"legacy.ncraft","discard_dirty":true})).is_err());
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), 0);
        assert!(!s.can_undo() && !s.can_redo());
        assert!(s.events_since(0).unwrap().is_empty());
        assert_eq!(storage.0.borrow().memory.recovery, old_journal);
        assert_eq!(s.file_status().location, old_status.location);
        assert_eq!(storage.0.borrow().memory.files.values().next().unwrap(), bytes);
        storage.0.borrow_mut().fail = false;
        s.dispatch("file.open", &json!({"location":"legacy.ncraft","discard_dirty":true})).unwrap();
        assert_eq!(&storage.0.borrow().memory.recovery[..8], b"NCRJ0003");
        s.dispatch("comment.create", &create(12, "Migrated root")).unwrap();
        let recovered = journal::recover(&storage.0.borrow().memory.recovery).unwrap();
        assert!(!recovered.legacy);
        assert_eq!(recovered.applied, 1);
        assert_eq!(&recovered.snapshot.project, s.document());
        assert_eq!(storage.0.borrow().memory.files.values().next().unwrap(), bytes, "opening never rewrites the source archive");
    }
}
#[test]
fn compose_rejects_out_of_range_offsets_even_when_the_world_sum_is_valid() {
    use nodecraft_engine::comment_cmds::{CommentContext, SERVICE};
    let mut project = common::project();
    nodecraft_graph::Change::MoveNode { canvas: common::canvas(), id: common::node_id(1), pos: nodecraft_graph::Position { x: -1_000_000.0, y: 0.0 } }
        .apply(&mut project)
        .unwrap();
    let mut s = Session::new(project).unwrap();
    s.register_host_service(SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
    s.swap_resource(Some(CommentContext { project: s.document().id(), canvas: common::canvas(), revision: s.revision(), blocked: false }));
    let request = json!({"node":common::node_id(1),"pos":{"x":11_000_000,"y":0}});
    assert!(s.enabled("comment.compose", &request).is_err());
    assert!(s.dispatch("comment.compose", &request).is_err());
    assert_eq!(s.revision(), 0);
}

#[test]
fn position_command_objects_reject_unknown_keys_in_all_three_paths() {
    use nodecraft_engine::comment_cmds::{CommentContext, SERVICE};
    let mut s = Session::new(common::project()).unwrap();
    let mut request = create(10, "Root");
    request["pos"]["typo"] = json!(1);
    assert!(s.dispatch("comment.create", &request).is_err());
    s.dispatch("comment.create", &create(10, "Root")).unwrap();
    assert!(s.dispatch("comment.move", &json!({"canvas":common::canvas(),"id":id(10),"pos":{"x":0,"y":0,"typo":1}})).is_err());
    s.register_host_service(SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
    s.swap_resource(Some(CommentContext { project: s.document().id(), canvas: common::canvas(), revision: s.revision(), blocked: false }));
    assert!(s.dispatch("comment.compose", &json!({"pos":{"x":0,"y":0,"typo":1}})).is_err());
}

#[test]
fn optional_revision_and_view_keys_match_their_schemas_even_for_null() {
    use nodecraft_engine::comment_cmds::{CommentContext, SERVICE};
    let mut s = Session::new(common::project()).unwrap();
    let mut request = create(10, "Root");
    request["expected_revision"] = json!(null);
    assert!(s.dispatch("comment.create", &request).is_err());
    s.register_host_service(SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
    s.swap_resource(Some(CommentContext { project: s.document().id(), canvas: common::canvas(), revision: s.revision(), blocked: false }));
    for (command, params) in [
        ("comment.compose", json!({"pos":{"x":0,"y":0},"id":null})),
        ("canvas.comments_toggle", json!({"node":null})),
        ("comment.open", json!({"id":id(10),"pos":null})),
    ] {
        assert!(s.dispatch(command, &params).is_err());
    }
}

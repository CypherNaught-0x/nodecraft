mod common;
use nodecraft_engine::Session;
use nodecraft_format::{AssetStore, MemoryStorage, Snapshot};
use nodecraft_types::{AssetRef, MediaMeta, Value};
use serde_json::json;
#[test]
fn save_gc_retains_config_media_in_undo_and_roundtrips_restored_config() {
    let mut session =
        Session::with_storage(Snapshot { project: common::project(), assets: AssetStore::default(), epoch: 0 }, Box::new(MemoryStorage::default())).unwrap();
    let hash = session.import_asset(b"config media retained by undo").unwrap();
    let mut node = common::node(3);
    node.config
        .insert("test.v1.asset".into(), Value::List(vec![Value::Media(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() })]));
    session.dispatch("node.add", &json!({"canvas":common::canvas(),"node":node})).unwrap();
    session.dispatch("node.remove", &json!({"canvas":common::canvas(),"id":common::node_id(3)})).unwrap();
    session.dispatch("file.save_as", &json!({"location":"config.ncraft"})).unwrap();
    assert!(session.assets().get(&hash).is_some(), "only undo references this config asset");
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document().canvases().get(&common::canvas()).unwrap().nodes().get(&node.id), Some(&node));
    session.dispatch("file.save", &json!({})).unwrap();
    session.dispatch("file.open", &json!({"location":"config.ncraft"})).unwrap();
    assert_eq!(session.document().canvases().get(&common::canvas()).unwrap().nodes().get(&node.id), Some(&node));
    assert!(session.assets().get(&hash).is_some());
}

#[test]
fn recovery_latch_disables_all_builtin_edit_surfaces_but_not_live_view_commands() {
    use nodecraft_format::{ProjectStorage, StorageLocation};
    use nodecraft_graph::{Canvas, Change, Project};
    use nodecraft_nodes::NodeKind;
    use nodecraft_types::{Error, ErrorKind, ProjectId};
    use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
    #[derive(Default)]
    struct State {
        memory: MemoryStorage,
        append_fail: bool,
        checkpoint_fail: bool,
    }
    #[derive(Clone)]
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
            if s.checkpoint_fail {
                return Err(Error::new(ErrorKind::Conflict, "injected checkpoint failure"));
            }
            s.memory.checkpoint(b)
        }
        fn append(&mut self, b: &[u8]) -> Result<(), Error> {
            let mut s = self.0.borrow_mut();
            if s.append_fail {
                return Err(Error::new(ErrorKind::Conflict, "injected append failure"));
            }
            s.memory.append(b)
        }
        fn discard(&mut self) -> Result<(), Error> {
            self.0.borrow_mut().memory.discard()
        }
    }
    for failure in ["append", "checkpoint", "closed"] {
        let canvas = common::canvas();
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Latch").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        for n in [1, 2] {
            let node = nodecraft_nodes::text::kind().descriptor().instantiate(common::node_id(n), &BTreeMap::new()).unwrap();
            Change::AddNode { canvas, node }.apply(&mut project).unwrap();
        }
        let storage = Shared(Rc::new(RefCell::new(State::default())));
        let mut session = Session::with_storage(Snapshot { project, assets: AssetStore::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
        session.dispatch("node.title", &json!({"canvas":canvas,"id":common::node_id(1),"title":"Before"})).unwrap();
        session.dispatch("node.move", &json!({"canvas":canvas,"id":common::node_id(1),"pos":{"x":200,"y":10}})).unwrap();
        session.dispatch("edit.undo", &json!({})).unwrap();
        match failure {
            "append" => {
                storage.0.borrow_mut().append_fail = true;
                assert!(session.dispatch("node.title", &json!({"canvas":canvas,"id":common::node_id(1),"title":"Fail"})).is_err());
            }
            "checkpoint" => {
                storage.0.borrow_mut().checkpoint_fail = true;
                let saved = session.dispatch("file.save_as", &json!({"location":"latch.ncraft"})).unwrap();
                assert_eq!(saved["recovery_ready"], false);
            }
            _ => session.close_recovery(true).unwrap(),
        }
        assert!(session.file_status().recovery_required);
        session.swap_resource(Some(common::layout_context(&session)));
        let cases = [
            ("node.title", json!({"canvas":canvas,"id":common::node_id(1),"title":"After"})),
            ("node.param_set", json!({"canvas":canvas,"id":common::node_id(1),"name":"text","value":{"type":"text","value":"After"}})),
            ("canvas.auto_format", json!({"mode":"grid"})),
        ];
        let before = session.document().clone();
        let revision = session.revision();
        let events = session.events_since(0).unwrap();
        let history = (session.can_undo(), session.can_redo());
        let journal = storage.0.borrow().memory.recovery.clone();
        for (id, p) in &cases {
            assert_eq!(session.enabled(id, p).unwrap_err().kind, ErrorKind::Conflict);
            assert_eq!(session.dispatch(id, p).unwrap_err().kind, ErrorKind::Conflict);
            assert_eq!(session.document(), &before);
            assert_eq!(session.revision(), revision);
            assert_eq!(session.events_since(0).unwrap(), events);
            assert_eq!((session.can_undo(), session.can_redo()), history);
            assert_eq!(storage.0.borrow().memory.recovery, journal);
        }
        session.swap_resource(Some(nodecraft_engine::navigation_cmds::NavigationContext {
            canvas,
            viewport_available: true,
            blocked: false,
            fit_all: true,
            fit_selection: true,
        }));
        session.register_host_service(nodecraft_engine::navigation_cmds::SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
        session.enabled("canvas.minimap_toggle", &json!({})).unwrap();
        session.dispatch("canvas.minimap_toggle", &json!({})).unwrap();
        assert_eq!(session.document(), &before);
        assert_eq!(session.revision(), revision);
        assert_eq!(session.events_since(0).unwrap(), events);
        assert_eq!(storage.0.borrow().memory.recovery, journal);
        if failure != "closed" {
            let mut s = storage.0.borrow_mut();
            s.append_fail = false;
            s.checkpoint_fail = false;
            drop(s);
            session.dispatch("file.save_as", &json!({"location":"repaired.ncraft"})).unwrap();
            for (id, p) in &cases {
                session.enabled(id, p).unwrap();
            }
            assert!(!session.file_status().recovery_required);
        }
    }
}

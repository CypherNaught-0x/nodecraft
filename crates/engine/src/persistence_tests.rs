//! Current-v2 admission and frozen-v1 reads preserve atomic document/history publication.
use super::*;
use crate::commands::CommandSpec;
use nodecraft_format::{MemoryStorage, StorageLocation, journal};
use nodecraft_graph::{Canvas, Generation, Node};
use nodecraft_types::{GenerationId, ProjectId, Value as Wire};
use serde_json::json;
use std::{cell::RefCell, rc::Rc};
#[derive(Default)]
struct Counters {
    memory: MemoryStorage,
    loads: usize,
    saves: usize,
    checkpoints: usize,
    appends: usize,
    flushes: usize,
    fail_append: bool,
    fail_checkpoint: bool,
}
#[derive(Clone, Default)]
struct Storage(Rc<RefCell<Counters>>);
impl ProjectStorage for Storage {
    fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
        let mut s = self.0.borrow_mut();
        s.loads += 1;
        s.memory.load(location)
    }
    fn save(&mut self, location: &StorageLocation, bytes: &[u8]) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.saves += 1;
        s.memory.save(location, bytes)
    }
    fn checkpoint(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.checkpoints += 1;
        if s.fail_checkpoint {
            return Err(invalid("injected checkpoint error"));
        }
        s.memory.checkpoint(bytes)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.appends += 1;
        if s.fail_append {
            return Err(invalid("injected append error"));
        }
        s.memory.append(bytes)
    }
    fn flush(&mut self) -> Result<(), Error> {
        self.0.borrow_mut().flushes += 1;
        Ok(())
    }
}
fn canvas() -> CanvasId {
    CanvasId::from_bytes([1; 16])
}
fn node() -> Node {
    Node::new(NodeId::from_bytes([2; 16]), "source.text")
}
fn project() -> Project {
    let mut p = Project::new(ProjectId::from_bytes([3; 16]), "Admission").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas(), "C") }.apply(&mut p).unwrap();
    Change::AddNode { canvas: canvas(), node: node() }.apply(&mut p).unwrap();
    p
}
fn register(s: &mut Session) {
    fn enabled(_: &Context<'_>, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn edit(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
        Ok(Effect::Edit { changes: crate::commands::params(value)?, result: Value::Null, gesture: None })
    }
    s.registry
        .register(CommandSpec {
            id: "test.edit",
            label: "Test",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || json!({}),
            params_doc: "changeset",
            enabled,
            run: edit,
        })
        .unwrap();
}
fn counts(s: &Storage) -> (usize, usize, usize, usize, usize) {
    let s = s.0.borrow();
    (s.loads, s.saves, s.checkpoints, s.appends, s.flushes)
}
fn v2_additions() -> Vec<Change> {
    let mut result = vec![];
    for value in [
        json!({"type":"bool","value":true}),
        json!({"type":"number","value":1.5}),
        json!({"type":"integer","value":"-1"}),
        json!({"type":"unsigned","value":"1"}),
    ] {
        let value: Wire = serde_json::from_value(value).unwrap();
        result.push(Change::SetParam { canvas: canvas(), id: node().id, name: "x".into(), value: Some(Wire::List(vec![value.clone()])) });
        result.push(Change::AddGeneration {
            canvas: canvas(),
            id: node().id,
            index: 0,
            generation: Generation {
                id: GenerationId::from_bytes([8; 16]),
                outputs: vec![value],
                input_hash: None,
                items: Vec::new(),
                settings: Vec::new(),
                incomplete: None,
            },
        });
    }
    result.push(Change::AddGeneration {
        canvas: canvas(),
        id: node().id,
        index: 0,
        generation: Generation {
            id: GenerationId::from_bytes([9; 16]),
            outputs: vec![Wire::Text("legacy".into())],
            input_hash: Some(nodecraft_types::Blake3::from_bytes([7; 32])),
            items: Vec::new(),
            settings: Vec::new(),
            incomplete: None,
        },
    });
    let mut n = node();
    n.config.insert("x".into(), Wire::Text("PRIVATE".into()));
    result.push(Change::SetNode { canvas: canvas(), node: n });
    result
}
#[test]
fn scalar_config_and_stamped_edits_persist_in_v2_with_exact_undo_redo() {
    for change in v2_additions() {
        let storage = Storage::default();
        let mut persistent = Session::with_storage(Snapshot { project: project(), assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
        register(&mut persistent);
        let params = serde_json::to_value(Changeset::new(vec![change])).unwrap();
        let mut memory = Session::new(project()).unwrap();
        register(&mut memory);
        memory.enabled("test.edit", &params).unwrap();
        memory.dispatch("test.edit", &params).unwrap();
        let edited = memory.document().clone();
        assert!(nodecraft_format::validate_v1_project(&edited).is_err(), "new semantics must not acquire frozen-v1 meaning");
        memory.dispatch("edit.undo", &Value::Null).unwrap();
        assert_eq!(memory.document(), &project());
        memory.dispatch("edit.redo", &Value::Null).unwrap();
        assert_eq!(memory.document(), &edited);
        let before = (
            persistent.document().clone(),
            persistent.revision,
            persistent.state,
            persistent.status_revision,
            persistent.file_status(),
            persistent.events.clone(),
            persistent.undo.bytes,
            persistent.redo.bytes,
            persistent.assets.clone(),
        );
        let journal = storage.0.borrow().memory.recovery.clone();
        let count = counts(&storage);
        persistent.enabled("test.edit", &params).unwrap();
        assert_eq!(
            (
                persistent.document().clone(),
                persistent.revision,
                persistent.state,
                persistent.status_revision,
                persistent.file_status(),
                persistent.events.clone(),
                persistent.undo.bytes,
                persistent.redo.bytes,
                persistent.assets.clone()
            ),
            before
        );
        assert_eq!(storage.0.borrow().memory.recovery, journal);
        assert_eq!(counts(&storage), count);
        persistent.dispatch("test.edit", &params).unwrap();
        assert_eq!(persistent.document(), &edited);
        assert_eq!(journal::recover(&storage.0.borrow().memory.recovery).unwrap().snapshot.project, edited);
        persistent.dispatch("edit.undo", &Value::Null).unwrap();
        assert_eq!(persistent.document(), &project());
        persistent.dispatch("edit.redo", &Value::Null).unwrap();
        assert_eq!(persistent.document(), &edited);
        assert!(!persistent.file_status().recovery_required);
        persistent.dispatch("file.save_as", &json!({"location":"current-v2"})).unwrap();
        assert_eq!(journal::recover(&storage.0.borrow().memory.recovery).unwrap().snapshot.project, edited);
    }
}
#[test]
fn constructors_saves_and_failed_opens_reject_before_checkpoint_or_durability_and_keep_state() {
    use std::io::{Cursor, Write};
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
    let mut incompatible = project();
    // Current encoding must fail before I/O for an otherwise graph-valid oversized manifest.
    // Frozen-v1 schema refusals are tested below against an actual v1 archive header.
    for id in 3..=9 {
        let mut node = Node::new(NodeId::from_bytes([id; 16]), "source.text");
        node.config.insert("large".into(), Wire::Text("x".repeat(700 * 1024)));
        Change::AddNode { canvas: canvas(), node }.apply(&mut incompatible).unwrap();
    }
    let storage = Storage::default();
    assert_eq!(
        Session::with_storage(Snapshot { project: incompatible.clone(), assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).err().unwrap().kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(counts(&storage), (0, 0, 0, 0, 0));
    let mut s = Session::with_storage(Snapshot { project: project(), assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    register(&mut s);
    let good = s.document.clone();
    s.document = incompatible;
    let count = counts(&storage);
    assert_eq!(s.enabled("file.save_as", &json!({"location":"bad"})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.dispatch("file.save_as", &json!({"location":"bad"})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(counts(&storage), count);
    s.document = good;
    s.dispatch("node.move", &json!({"canvas":canvas(),"id":node().id,"pos":{"x":3,"y":4},"gesture":"active"})).unwrap();
    let before = (s.document.clone(), s.revision, s.state, s.file_status(), s.status_revision, s.undo.bytes, s.redo.bytes, s.gesture.clone(), s.events.clone());
    let journal = storage.0.borrow().memory.recovery.clone();
    let mut raw = serde_json::to_value(project()).unwrap();
    raw["canvases"][canvas().to_string()]["nodes"][node().id.to_string()]["config"] = json!({});
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("project.json", SimpleFileOptions::default().compression_method(CompressionMethod::Stored)).unwrap();
    zip.write_all(&serde_json::to_vec(&json!({"version":1,"project":raw})).unwrap()).unwrap();
    storage.0.borrow_mut().memory.files.insert(StorageLocation::from_command("bad".into()).unwrap(), zip.finish().unwrap().into_inner());
    let count = counts(&storage);
    assert_eq!(s.dispatch("file.open", &json!({"location":"bad","discard_dirty":true})).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(
        (s.document.clone(), s.revision, s.state, s.file_status(), s.status_revision, s.undo.bytes, s.redo.bytes, s.gesture.clone(), s.events.clone()),
        before
    );
    assert_eq!(storage.0.borrow().memory.recovery, journal);
    let after = counts(&storage);
    assert_eq!(after, (count.0 + 1, count.1, count.2, count.3, count.4));
}
#[test]
fn storage_latch_blocks_custom_mutation_enabled_dispatch_and_preserves_services() {
    let storage = Storage::default();
    let mut s = Session::with_storage(Snapshot { project: project(), assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    register(&mut s);
    storage.0.borrow_mut().fail_append = true;
    let edit = serde_json::to_value(Changeset::new(vec![Change::SetNodeTitle { canvas: canvas(), id: node().id, title: "title".into() }])).unwrap();
    assert!(s.dispatch("test.edit", &edit).is_err());
    assert!(s.file_status().recovery_required);
    let count = counts(&storage);
    assert!(s.enabled("test.edit", &edit).is_err());
    assert!(s.dispatch("test.edit", &edit).is_err());
    assert_eq!(counts(&storage), count);
    // Service command admission does not acquire the graph edit latch (missing key service is its own error).
    assert!(s.enabled("keys.status", &json!({"account":"openai"})).is_ok());
    let err = s.dispatch("keys.status", &json!({"account":"openai"})).unwrap_err();
    assert!(!err.to_string().contains("checkpoint"));
}

#[test]
fn config_only_media_survives_history_root_collection_and_asset_gc() {
    use nodecraft_types::{AssetRef, MediaMeta};
    let mut s = Session::new(project()).unwrap();
    register(&mut s);
    let hash = s.import_asset(b"config-only history media").unwrap();
    let mut configured = node();
    configured
        .config
        .insert("nested".into(), Wire::List(vec![Wire::Media(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() })]));
    for node in [configured, node()] {
        let edit = serde_json::to_value(Changeset::new(vec![Change::SetNode { canvas: canvas(), node }])).unwrap();
        s.dispatch("test.edit", &edit).unwrap();
    }
    assert!(s.document().canvases().get(&canvas()).unwrap().nodes().get(&node().id).unwrap().config.is_empty());
    let roots = s.retained_assets().unwrap();
    assert!(roots.contains(&hash));
    s.assets.retain(&roots);
    assert!(s.assets.get(&hash).is_some());
    s.dispatch("edit.undo", &Value::Null).unwrap();
    assert!(s.assets.validate_project(s.document()).unwrap().contains(&hash));
    s.dispatch("edit.redo", &Value::Null).unwrap();
    assert!(s.retained_assets().unwrap().contains(&hash));
}

#[test]
fn recovered_work_replaces_the_document_unsaved_at_its_file_and_checkpointed() {
    // A session edits, then "crashes": its journal holds the edits.
    let crashed = Storage::default();
    let mut s = Session::with_storage(Snapshot { project: project(), assets: Default::default(), epoch: 0 }, Box::new(crashed.clone())).unwrap();
    let mut extra = node();
    extra.id = nodecraft_types::NodeId::from_bytes([0x5a; 16]);
    s.dispatch("node.add", &serde_json::json!({"canvas": canvas(), "node": extra})).unwrap();
    let edited = s.document().clone();
    let recovered = journal::recover(&crashed.0.borrow().memory.recovery).unwrap();
    assert_eq!(recovered.applied, 1);
    // A new session offers it, and the person chooses Recover.
    let fresh = Storage::default();
    let start = || Snapshot { project: project(), assets: Default::default(), epoch: 0 };
    let mut next = Session::with_storage(start(), Box::new(fresh.clone())).unwrap();
    let revision = next.revision();
    // A checkpoint that fails changes nothing.
    fresh.0.borrow_mut().fail_checkpoint = true;
    let before = (next.document().clone(), next.file_status());
    assert!(next.recover(recovered.snapshot.clone(), None).is_err());
    assert_eq!((next.document().clone(), next.file_status()), before);
    assert_eq!(next.revision(), revision);
    fresh.0.borrow_mut().fail_checkpoint = false;
    let file = StorageLocation::Path("/projects/a.ncraft".into());
    next.recover(recovered.snapshot.clone(), Some(file.clone())).unwrap();
    assert_eq!(next.document(), &edited);
    assert!(next.revision() > revision);
    let status = next.file_status();
    assert!(status.dirty && status.location == Some(file) && !status.recovery_required, "unsaved at its file, checkpointed");
    assert!(!next.can_undo(), "history starts fresh");
    // Its own journal now starts from the recovered project.
    assert_eq!(journal::recover(&fresh.0.borrow().memory.recovery).unwrap().snapshot.project, edited);
    // A session with edits of its own refuses: they'd be lost.
    let mut busy = Session::with_storage(start(), Box::new(Storage::default())).unwrap();
    busy.dispatch("node.add", &serde_json::json!({"canvas": canvas(), "node": Node::new(NodeId::from_bytes([0x5b; 16]), "source.text")})).unwrap();
    assert_eq!(busy.recover(recovered.snapshot.clone(), None).unwrap_err().kind, ErrorKind::Conflict);
    // Without storage, there is nothing to recover into.
    let mut plain = Session::new(project()).unwrap();
    assert_eq!(plain.recover(recovered.snapshot, None).unwrap_err().kind, ErrorKind::Unsupported);
}

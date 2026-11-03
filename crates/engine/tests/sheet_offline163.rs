//! A draft Batch Generate sheet (#163) is edited, named, saved, reopened and recovered with no
//! generation backend at all: no provider is set up, no run host is registered, and nothing
//! here reaches a network. Running refuses with an error and leaves the draft as it was.
use nodecraft_engine::Session;
use nodecraft_format::{AssetStore, MemoryStorage, ProjectStorage, Snapshot, StorageLocation, decode, journal};
use nodecraft_graph::{Canvas, CanvasKind, Change, Project};
use nodecraft_types::{CanvasId, Error, NodeId, ProjectId, Value};
use serde_json::{Value as Json, json};
use std::{cell::RefCell, rc::Rc};

const SHEET: CanvasId = CanvasId::from_bytes([0x63; 16]);
const PLAIN: CanvasId = CanvasId::from_bytes([0x64; 16]);

/// Memory storage the test can look into after the session has written to it.
#[derive(Clone, Default)]
struct Shared(Rc<RefCell<MemoryStorage>>);
impl ProjectStorage for Shared {
    fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.borrow_mut().load(location)
    }
    fn save(&mut self, location: &StorageLocation, archive: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().save(location, archive)
    }
    fn checkpoint(&mut self, recovery: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().checkpoint(recovery)
    }
    fn append(&mut self, frame: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().append(frame)
    }
    fn discard(&mut self) -> Result<(), Error> {
        self.0.borrow_mut().discard()
    }
}

fn seed(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn offline() -> (Session, Shared) {
    let mut project = Project::new(ProjectId::from_bytes([0x65; 16]), "Offline batch").unwrap();
    Change::AddCanvas { canvas: Canvas::new(PLAIN, "Canvas") }.apply(&mut project).unwrap();
    let storage = Shared::default();
    let session = Session::with_storage(Snapshot { project, assets: AssetStore::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    (session, storage)
}

fn rows(s: &mut Session) -> Json {
    s.dispatch("sheet.rows", &json!({"canvas": SHEET})).unwrap()
}

fn prompts(listed: &Json) -> Vec<String> {
    listed["rows"].as_array().unwrap().iter().map(|row| row["prompt"].as_str().unwrap_or_default().to_owned()).collect()
}

/// A draft: a named sheet, rows typed in and made from a template, one edited, and a reference.
fn draft(s: &mut Session) {
    s.dispatch("sheet.create", &json!({"canvas": SHEET, "title": "Mugs"})).unwrap();
    s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(1), "prompts": ["a plain mug"]})).unwrap();
    s.dispatch(
        "sheet.expand",
        &json!({"canvas": SHEET, "seed": seed(2), "template": "a {{colour}} mug", "variables": [{"name": "colour", "text": ["red", "blue"]}], "copies": 1}),
    )
    .unwrap();
    let listed = rows(s);
    let first: NodeId = serde_json::from_value(listed["rows"][0]["id"].clone()).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": SHEET, "id": first, "name": "prompt", "value": Value::Text("a plain white mug".into())})).unwrap();
    let mut note = s.kinds().get("source.text").unwrap().descriptor().instantiate(seed(3), &Default::default()).unwrap();
    note.params.insert("text".into(), Value::Text("studio light".into()));
    note.pos = nodecraft_graph::Position { x: -480.0, y: 0.0 };
    s.dispatch("node.add", &json!({"canvas": SHEET, "node": note})).unwrap();
    s.dispatch("sheet.rename", &json!({"canvas": SHEET, "title": "Mugs, autumn"})).unwrap();
}

#[test]
fn a_draft_sheet_is_edited_saved_and_reopened_with_no_backend() {
    let (mut s, storage) = offline();
    draft(&mut s);
    let before = rows(&mut s);
    assert_eq!(prompts(&before), ["a plain white mug", "a red mug", "a blue mug"]);
    s.dispatch("file.save_as", &json!({"location": "mugs.ncraft"})).unwrap();
    assert!(!s.file_status().dirty);

    // The file holds the sheet as a sheet, with its title, rows and provenance.
    let bytes = storage.0.borrow().files.get(&StorageLocation::from_command("mugs.ncraft".into()).unwrap()).unwrap().clone();
    let saved = decode(&bytes).unwrap();
    let sheet = saved.project.canvases().get(&SHEET).unwrap();
    assert_eq!((sheet.kind(), sheet.title()), (CanvasKind::Sheet, "Mugs, autumn"));

    // Reopened in a new session, the batch is listed by its name and its rows are the same.
    let mut reopened = Session::with_storage(saved, Box::new(Shared::default())).unwrap();
    let listed = reopened.dispatch("sheet.list", &json!({})).unwrap();
    assert_eq!(listed["sheets"], json!([{"canvas": SHEET, "title": "Mugs, autumn", "rows": 3}]));
    let after = rows(&mut reopened);
    assert_eq!(after["rows"], before["rows"], "rows, settings and expansion provenance survive the file");
    assert_eq!(after["rows"][1]["expansion"]["values"], json!({"colour": "red"}));

    // And it reopens through the file command too, in the session that saved it.
    s.dispatch("file.open", &json!({"location": "mugs.ncraft"})).unwrap();
    assert_eq!(rows(&mut s)["rows"], before["rows"]);
}

#[test]
fn every_draft_edit_is_in_the_recovery_journal() {
    let (mut s, storage) = offline();
    draft(&mut s);
    let recovered = journal::recover(&storage.0.borrow().recovery).unwrap();
    assert_eq!(recovered.snapshot.project, *s.document(), "autosave has every edit");
    // Undoing an edit is recorded too, and a recovered session is the draft, unsaved.
    s.dispatch("edit.undo", &json!({})).unwrap();
    let recovered = journal::recover(&storage.0.borrow().recovery).unwrap();
    assert_eq!(recovered.snapshot.project, *s.document());
    assert_eq!(recovered.snapshot.project.canvases().get(&SHEET).unwrap().title(), "Mugs", "the rename was undone");
    let mut back = Session::with_storage(recovered.snapshot, Box::<MemoryStorage>::default()).unwrap();
    assert!(back.file_status().dirty, "recovered work is unsaved until saved");
    assert_eq!(prompts(&rows(&mut back)), ["a plain white mug", "a red mug", "a blue mug"]);
}

#[test]
fn running_with_no_backend_refuses_and_leaves_the_draft_editable() {
    let (mut s, _storage) = offline();
    draft(&mut s);
    let listed = rows(&mut s);
    let ids: Vec<Json> = listed["rows"].as_array().unwrap().iter().map(|row| row["id"].clone()).collect();
    let revision = s.revision();
    let request = json!({"canvas": SHEET, "mode": "selected", "nodes": ids});
    let quoted = s.dispatch("run.quote", &request);
    let token = quoted.as_ref().ok().map(|quoted| quoted["token"].clone()).unwrap_or(Json::Null);
    let mut start = request.clone();
    start["accept"] = json!({"token": token, "partial": false, "unpriced_jobs": 0, "amounts": {}});
    let refused = s.dispatch("run.start", &start).unwrap_err();
    assert!(!refused.message.is_empty());
    assert_eq!(s.revision(), revision, "a refused run changes nothing");
    // Still a draft that edits and saves.
    s.dispatch("sheet.rows_add", &json!({"canvas": SHEET, "seed": seed(9), "prompts": ["a tall mug"]})).unwrap();
    s.dispatch("file.save_as", &json!({"location": "mugs.ncraft"})).unwrap();
    assert_eq!(rows(&mut s)["rows"].as_array().unwrap().len(), 4);
}

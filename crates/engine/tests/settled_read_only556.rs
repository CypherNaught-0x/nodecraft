//! `live_actions::Settled` on a project that has turned read-only (#556): a settled canvas means
//! "there is nothing to recompute", which is not something to promise when a result could not be
//! recorded even if there were.
use nodecraft_engine::{Session, live_actions::Settled};
use nodecraft_format::{AssetStore, MemoryStorage, ProjectAccess, ProjectStorage, Snapshot, StorageLocation};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, Error, ProjectId};
use serde_json::json;
use std::{cell::RefCell, rc::Rc};

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);

/// Memory storage that publishes every save as read-only, as a destination that can't be
/// overwritten reports itself.
#[derive(Clone)]
struct ReadOnlyAfterSave(Rc<RefCell<MemoryStorage>>);

impl ProjectStorage for ReadOnlyAfterSave {
    fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.borrow_mut().load(location)
    }
    fn save(&mut self, location: &StorageLocation, bytes: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().save(location, bytes)
    }
    fn save_with_access(&mut self, location: &StorageLocation, bytes: &[u8]) -> Result<ProjectAccess, Error> {
        self.0.borrow_mut().save(location, bytes)?;
        Ok(ProjectAccess::ReadOnly)
    }
    fn checkpoint(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().checkpoint(bytes)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().append(bytes)
    }
}

#[test]
fn a_settled_canvas_stops_holding_once_the_project_is_read_only() {
    let mut project = Project::new(ProjectId::from_bytes([3; 16]), "P").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let storage = ReadOnlyAfterSave(Rc::new(RefCell::new(MemoryStorage::default())));
    let mut session = Session::with_storage(Snapshot { project, assets: AssetStore::default(), epoch: 0 }, Box::new(storage)).unwrap();

    let settled = Settled::now(&session, CANVAS).expect("writable, with nothing to redo");
    let (document, revision) = (session.document().clone(), session.revision());
    assert!(settled.holds(&session, CANVAS), "nothing to do, and results could be recorded");

    session.dispatch("file.save_as", &json!({"location": "p.ncraft"})).unwrap();
    assert!(session.file_status().read_only, "the destination published the project read-only");
    assert!(!settled.holds(&session, CANVAS), "with nothing recordable, settled is not a promise");
    assert!(Settled::now(&session, CANVAS).is_none(), "and there is nothing to settle from now on");

    // Asking was a read: the document, its revision and its history are what they were.
    assert_eq!(session.document(), &document);
    assert_eq!(session.revision(), revision, "a save is not an edit");
    assert!(!session.can_redo());
}

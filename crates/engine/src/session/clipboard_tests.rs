use super::*;
use nodecraft_graph::{Canvas, Node};
use nodecraft_types::{ProjectId, Value as GraphValue};
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const A: NodeId = NodeId::from_bytes([2; 16]);
const B: NodeId = NodeId::from_bytes([3; 16]);
fn project() -> Project {
    let mut p = Project::new(ProjectId::from_bytes([4; 16]), "Cut").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    Change::AddNode { canvas: C, node: Node::new(A, "test.unknown") }.apply(&mut p).unwrap();
    p
}
fn params() -> (Value, Value) {
    (json!({"canvas":C,"nodes":[A]}), json!({"canvas":C,"nodes":[Node::new(B,"test.unknown")]}))
}
#[test]
fn cut_roundtrip_uses_postdelete_capacity_and_one_history_entry() {
    let mut p = project();
    let changes = (0u32..9999)
        .map(|n| {
            let mut id = [0u8; 16];
            id[..4].copy_from_slice(&n.to_le_bytes());
            Change::AddNode { canvas: C, node: Node::new(NodeId::from_bytes(id), "test.unknown") }
        })
        .collect();
    nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
    let mut s = Session::new(p.clone()).unwrap();
    let (delete, paste) = params();
    assert!(s.enabled("canvas.paste", &paste).is_err(), "original canvas is full");
    s.dispatch_clipboard_cut(&delete, &paste).unwrap();
    assert_eq!(s.revision(), 1);
    assert_eq!(s.document().canvases().get(&C).unwrap().nodes().len(), 9999);
    assert_eq!(s.events_since(0).unwrap()[0].command, "canvas.delete");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &p);
    assert!(!s.can_undo());
    s.dispatch("edit.redo", &json!({})).unwrap();
    s.dispatch("canvas.paste", &paste).unwrap();
    assert_eq!(s.document().canvases().get(&C).unwrap().nodes().len(), 10000);
}
thread_local! {static DELETE_CALLS:Cell<usize>=const{Cell::new(0)};static PASTE_CALLS:Cell<usize>=const{Cell::new(0)};}
fn enabled(_: &Context<'_>, _: &Value) -> Result<(), Error> {
    Ok(())
}
fn delete(c: &Context<'_>, _: &Value) -> Result<Effect, Error> {
    if c.phase != Phase::Dispatch {
        return Err(invalid("wrong phase"));
    }
    DELETE_CALLS.with(|n| n.set(n.get() + 1));
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::RemoveNode { canvas: C, id: A }]), result: json!({"deleted":true}), gesture: None })
}
fn paste(c: &Context<'_>, _: &Value) -> Result<Effect, Error> {
    PASTE_CALLS.with(|n| n.set(n.get() + 1));
    if c.phase != Phase::Dispatch
        || c.revision != 1
        || !c.can_undo
        || c.can_redo
        || !c.file.dirty
        || c.document.canvases().get(&C).unwrap().nodes().contains_key(&A)
    {
        return Err(invalid("wrong prospective context"));
    }
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::AddNode { canvas: C, node: Node::new(B, "test.unknown") }]), result: Value::Null, gesture: None })
}
fn registry(paste_run: fn(&Context<'_>, &Value) -> Result<Effect, Error>) -> Registry {
    let mut registry = Registry::default();
    for (id, run) in [("canvas.delete", delete as fn(&Context<'_>, &Value) -> Result<Effect, Error>), ("canvas.paste", paste_run)] {
        registry
            .register(crate::commands::CommandSpec {
                id,
                label: id,
                shortcut: None,
                alternate_shortcuts: &[],
                input_schema: || json!({}),
                params_doc: "test",
                enabled,
                run,
            })
            .unwrap();
    }
    registry
}
#[test]
fn cut_callbacks_use_dispatch_once_and_postdelete_metadata() {
    DELETE_CALLS.with(|n| n.set(0));
    PASTE_CALLS.with(|n| n.set(0));
    let mut s = Session::with_registry(project(), registry(paste)).unwrap();
    s.dispatch_clipboard_cut(&json!({}), &json!({})).unwrap();
    assert_eq!(DELETE_CALLS.with(|n| n.get()), 1);
    assert_eq!(PASTE_CALLS.with(|n| n.get()), 1);
    assert_eq!(s.revision(), 1);
    assert!(!s.document().canvases().get(&C).unwrap().nodes().contains_key(&B));
}
#[test]
fn cut_never_invokes_a_service_effect_or_publishes_on_hypothetical_failure() {
    fn service(_: &Context<'_>, _: &Value) -> Result<Effect, Error> {
        Ok(Effect::Service { capability: "test.external", request: json!({}) })
    }
    let mut s = Session::with_registry(project(), registry(service)).unwrap();
    let called = Rc::new(Cell::new(0));
    let other = Rc::clone(&called);
    s.register_host_service("test.external", move |_| {
        other.set(other.get() + 1);
        Ok(Value::Null)
    })
    .unwrap();
    let before = s.document().clone();
    assert!(s.dispatch_clipboard_cut(&json!({}), &json!({})).is_err());
    assert_eq!(called.get(), 0);
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), 0);
    assert!(!s.can_undo());
    assert!(s.events_since(0).unwrap().is_empty());
}
struct Storage {
    inner: Rc<RefCell<nodecraft_format::MemoryStorage>>,
    fail: Rc<Cell<bool>>,
}
impl ProjectStorage for Storage {
    fn load(&mut self, l: &nodecraft_format::StorageLocation) -> Result<Vec<u8>, Error> {
        self.inner.borrow_mut().load(l)
    }
    fn save(&mut self, l: &nodecraft_format::StorageLocation, b: &[u8]) -> Result<(), Error> {
        self.inner.borrow_mut().save(l, b)
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
        self.inner.borrow_mut().checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        if self.fail.get() {
            return Err(Error::new(ErrorKind::Internal, "injected append failure"));
        }
        self.inner.borrow_mut().append(b)
    }
}
#[test]
fn cut_append_failure_retains_document_history_and_reports_recovery_status() {
    let inner = Rc::new(RefCell::new(nodecraft_format::MemoryStorage::default()));
    let fail = Rc::new(Cell::new(false));
    let mut s = Session::with_storage(
        nodecraft_format::Snapshot { project: project(), assets: Default::default(), epoch: 0 },
        Box::new(Storage { inner: Rc::clone(&inner), fail: Rc::clone(&fail) }),
    )
    .unwrap();
    let before = s.document().clone();
    let recovery = inner.borrow().recovery.clone();
    let status = s.status_revision();
    fail.set(true);
    let (d, p) = params();
    assert!(s.dispatch_clipboard_cut(&d, &p).is_err());
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), 0);
    assert!(!s.can_undo() && !s.can_redo());
    assert!(s.events_since(0).unwrap().is_empty());
    assert_eq!(inner.borrow().recovery, recovery);
    assert!(s.file_status().recovery_required);
    assert_eq!(s.status_revision(), status + 1);
}
#[test]
fn cut_rejects_future_persistent_representation_before_append() {
    fn unsupported(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
        let mut effect = paste(c, v)?;
        if let Effect::Edit { changes, .. } = &mut effect
            && let Some(Change::AddNode { node, .. }) = changes.changes.first_mut()
        {
            node.params.insert("scalar".into(), GraphValue::Bool(true));
        }
        Ok(effect)
    }
    let inner = Rc::new(RefCell::new(nodecraft_format::MemoryStorage::default()));
    let mut s = Session::with_storage(
        nodecraft_format::Snapshot { project: project(), assets: Default::default(), epoch: 0 },
        Box::new(Storage { inner: Rc::clone(&inner), fail: Rc::new(Cell::new(false)) }),
    )
    .unwrap();
    s.registry = registry(unsupported);
    let before = s.document().clone();
    let recovery = inner.borrow().recovery.clone();
    assert_eq!(s.dispatch_clipboard_cut(&json!({}), &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(s.document(), &before);
    assert_eq!(inner.borrow().recovery, recovery);
    assert_eq!(s.revision(), 0);
    assert!(!s.file_status().recovery_required);
}

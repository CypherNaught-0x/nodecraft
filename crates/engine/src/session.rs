use std::{
    collections::VecDeque,
    io::Write,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use nodecraft_format::{AssetStore, ProjectStorage, Snapshot};
use nodecraft_graph::{Change, Changeset, Project};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde_json::Value;

use crate::commands::{Context, Effect, Phase, Registry, Resources, Services, invalid, validate_json};

/// Enough to undo building a 1,000-node graph (about 3,000 steps, #65); the byte budget below is
/// what bounds memory.
pub const MAX_HISTORY_STEPS: usize = 4096;
pub const MAX_HISTORY_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_EVENTS: usize = 256;

/// Notification log entry. Each consumer keeps its own revision cursor and reads the current
/// document after a notification. A lagging consumer receives a resync error, not partial history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocChanged {
    pub revision: u64,
    pub command: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MoveKey {
    canvas: CanvasId,
    node: NodeId,
    gesture: String,
}

struct Entry {
    inverse: Changeset,
    bytes: usize,
    state: u64,
}

#[derive(Default)]
struct History {
    entries: VecDeque<Entry>,
    bytes: usize,
}

impl History {
    fn push(&mut self, entry: Entry) {
        self.bytes = self.bytes.saturating_add(entry.bytes);
        self.entries.push_back(entry);
        while self.entries.len() > MAX_HISTORY_STEPS || self.bytes > MAX_HISTORY_BYTES {
            if let Some(old) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.bytes);
            } else {
                break;
            }
        }
    }
    fn pop(&mut self) {
        if let Some(old) = self.entries.pop_back() {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
    }
}

enum Direction {
    Edit,
    Undo,
    Redo,
}
struct Prepared {
    document: Project,
    inverse: Entry,
    direction: Direction,
    gesture: Option<MoveKey>,
    result: Value,
    admission: Option<PersistentAdmission>,
    source: SourceStamp,
    state: u64,
}
struct SourceStamp {
    identity: Arc<()>,
    revision: u64,
}
impl SourceStamp {
    fn matches(&self, session: &Session) -> bool {
        self.revision == session.revision && Arc::ptr_eq(&self.identity, &session.admission_identity)
    }
}
/// Only Session::prepare can construct this private document-admission capability.
/// The owning Prepared bundle pairs it with the exact graph-staged prospective document.
pub(crate) struct PersistentAdmission {
    replay: nodecraft_format::journal::AdmittedReplay,
}
impl PersistentAdmission {
    pub(crate) fn into_replay(self) -> nodecraft_format::journal::AdmittedReplay {
        self.replay
    }
}

/// Owns the document and is its only command mutation path. Read access is immutable.
/// The panic guard stages edits before publication; native unwinding preserves the live state.
/// Abort-only targets (including the default wasm target) still rely on fallible validation.
pub struct Session {
    document: Project,
    registry: Registry,
    kinds: nodecraft_nodes::Registry,
    resources: Resources,
    services: Services,
    undo: History,
    redo: History,
    gesture: Option<MoveKey>,
    revision: u64,
    events: VecDeque<DocChanged>,
    state: u64,
    assets: AssetStore,
    persistence: Option<crate::persistence::Persistence>,
    status_revision: u64,
    admission_identity: Arc<()>,
}

impl Session {
    pub fn new(document: Project) -> Result<Self, Error> {
        Self::with_registry(document, Registry::builtins()?)
    }

    pub fn with_registry(document: Project, registry: Registry) -> Result<Self, Error> {
        Self::with_registries(document, registry, nodecraft_nodes::Registry::builtins()?)
    }

    pub fn with_registries(document: Project, registry: Registry, kinds: nodecraft_nodes::Registry) -> Result<Self, Error> {
        document.validate()?;
        Ok(Self {
            document,
            registry,
            kinds,
            resources: Resources::default(),
            services: Services::default(),
            undo: History::default(),
            redo: History::default(),
            gesture: None,
            revision: 0,
            events: VecDeque::new(),
            state: 0,
            assets: AssetStore::default(),
            persistence: None,
            status_revision: 0,
            admission_identity: Arc::new(()),
        })
    }

    pub fn document(&self) -> &Project {
        &self.document
    }
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
    pub fn kinds(&self) -> &nodecraft_nodes::Registry {
        &self.kinds
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.entries.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.entries.is_empty()
    }

    /// Replace a host-owned capability without discarding other injected resources.
    /// A host may scope a live view snapshot to one enabled/dispatch call and restore it after.
    pub fn swap_resource<T: std::any::Any>(&mut self, value: Option<T>) -> Option<T> {
        self.resources.swap(value)
    }
    /// Add a host callback without replacing services supplied by another frontend.
    pub fn register_host_service(&mut self, id: &'static str, handler: impl FnMut(&Value) -> Result<Value, Error> + 'static) -> Result<(), Error> {
        self.services.register(id, handler)
    }
    pub fn with_storage(snapshot: Snapshot, storage: Box<dyn ProjectStorage>) -> Result<Self, Error> {
        let persistence = crate::persistence::Persistence::new(storage, &snapshot)?;
        let mut session = Self::new(snapshot.project)?;
        session.assets = snapshot.assets;
        session.persistence = Some(persistence);
        Ok(session)
    }
    pub fn assets(&self) -> &AssetStore {
        &self.assets
    }
    pub fn import_asset(&mut self, bytes: &[u8]) -> Result<nodecraft_types::Blake3, Error> {
        let hash = self.assets.insert(bytes)?;
        self.admission_identity = Arc::new(());
        Ok(hash)
    }
    pub fn file_status(&self) -> crate::file_cmds::FileStatus {
        self.persistence.as_ref().map(|p| p.status(self.state)).unwrap_or(crate::file_cmds::FileStatus { dirty: true, ..Default::default() })
    }
    pub fn status_revision(&self) -> u64 {
        self.status_revision
    }
    /// Host lifecycle boundary: flush and discard recovery after a clean close or explicit discard.
    pub fn close_recovery(&mut self, discard_dirty: bool) -> Result<(), Error> {
        let before = self.file_status();
        let result =
            self.persistence.as_mut().ok_or_else(|| Error::new(ErrorKind::Unsupported, "project storage is not configured"))?.close(self.state, discard_dirty);
        if self.file_status() != before {
            self.status_revision = self.status_revision.saturating_add(1);
        }
        result
    }
    fn retained_assets(&self) -> Result<std::collections::BTreeSet<nodecraft_types::Blake3>, Error> {
        let mut roots = self.assets.validate_project(&self.document)?;
        for entry in self.undo.entries.iter().chain(&self.redo.entries) {
            roots.extend(nodecraft_format::assets_in_changes(&entry.inverse)?);
        }
        Ok(roots)
    }

    pub fn with_resources(mut self, resources: Resources) -> Self {
        self.resources = resources;
        self
    }

    pub fn with_services(mut self, services: Services) -> Self {
        self.services = services;
        self
    }

    fn context(&self, phase: Phase) -> Context<'_> {
        Context {
            revision: self.revision,
            document: &self.document,
            kinds: &self.kinds,
            can_undo: self.can_undo(),
            can_redo: self.can_redo(),
            resources: &self.resources,
            services: &self.services,
            phase,
            file: self.file_status(),
        }
    }

    /// Returns an actionable reason when disabled; never mutates the session.
    pub fn enabled(&self, id: &str, params: &Value) -> Result<(), Error> {
        guard(|| {
            validate_json(params)?;
            let spec = self.registry.get(id)?;
            let context = self.context(Phase::Preview);
            (spec.enabled)(&context, params)?;
            // Handlers describe effects without I/O. Preview custom edits through the same
            // admission boundary as dispatch; services and file I/O are never invoked here.
            match (spec.run)(&context, params)? {
                effect @ (Effect::Edit { .. } | Effect::Undo | Effect::Redo) => self.prepare(effect).map(|_| ()),
                _ => Ok(()),
            }
        })
    }

    pub fn dispatch(&mut self, id: &str, params: &Value) -> Result<Value, Error> {
        let before = self.file_status();
        let result = self.dispatch_inner(id, params);
        if self.file_status() != before {
            self.status_revision = self.status_revision.saturating_add(1);
        }
        result
    }
    fn dispatch_inner(&mut self, id: &str, params: &Value) -> Result<Value, Error> {
        let spec = *self.registry.get(id)?;
        let effect = guard(|| {
            validate_json(params)?;
            let context = self.context(Phase::Dispatch);
            (spec.enabled)(&context, params)?;
            (spec.run)(&context, params)
        })?;
        let effect = match effect {
            Effect::None { result } => {
                validate_json(&result)?;
                return Ok(result);
            }
            Effect::Service { capability, request } => {
                validate_json(&request)?;
                return guard(|| {
                    let result = self.services.invoke(capability, &request)?;
                    validate_json(&result)?;
                    Ok(result)
                });
            }
            Effect::Save { location } => {
                let roots = self.retained_assets()?;
                let storage = self.persistence.as_mut().ok_or_else(|| Error::new(ErrorKind::Unsupported, "project storage is not configured"))?;
                let ready = storage.save(&self.document, &self.assets, self.state, location)?;
                self.assets.retain(&roots);
                self.admission_identity = Arc::new(());
                self.gesture = None;
                return Ok(serde_json::json!({"saved":true,"recovery_ready":ready,"read_only":self.file_status().read_only}));
            }
            Effect::Open { location, discard_dirty } => {
                if self.file_status().dirty && !discard_dirty {
                    return Err(Error::new(ErrorKind::Conflict, "opening would discard unsaved changes"));
                }
                let revision = self.revision.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "document revision overflow"))?;
                let storage = self.persistence.as_mut().ok_or_else(|| Error::new(ErrorKind::Unsupported, "project storage is not configured"))?;
                let snapshot = storage.open(location, revision)?;
                self.document = snapshot.project;
                self.assets = snapshot.assets;
                self.undo = History::default();
                self.redo = History::default();
                self.gesture = None;
                self.state = revision;
                self.publish(revision, spec.id);
                return Ok(serde_json::json!({"opened":true,"read_only":self.file_status().read_only}));
            }
            document_effect => document_effect,
        };
        let prepared = guard(|| self.prepare(effect))?;
        self.commit_prepared(prepared, spec.id)
    }
    fn commit_prepared(&mut self, mut prepared: Prepared, command: &'static str) -> Result<Value, Error> {
        if !prepared.source.matches(self) {
            return Err(Error::new(ErrorKind::Conflict, "prepared edit no longer matches the session document, assets or revision"));
        }
        let revision = self.revision.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "document revision overflow"))?;
        if let Some(admission) = prepared.admission.take() {
            let storage = self.persistence.as_mut().ok_or_else(|| Error::new(ErrorKind::Conflict, "prepared persistent edit requires its original storage"))?;
            storage.append(admission)?;
        }
        // No handler or fallible graph operation runs after this point.
        match prepared.direction {
            Direction::Edit => {
                if prepared.gesture.is_none() || prepared.gesture != self.gesture || self.undo.entries.is_empty() {
                    self.undo.push(prepared.inverse);
                }
                self.redo = History::default();
                self.gesture = prepared.gesture;
            }
            Direction::Undo => {
                self.undo.pop();
                self.redo.push(prepared.inverse);
                self.gesture = None;
            }
            Direction::Redo => {
                self.redo.pop();
                self.undo.push(prepared.inverse);
                self.gesture = None;
            }
        }
        self.document = prepared.document;
        self.state = prepared.state;
        self.publish(revision, command);
        Ok(prepared.result)
    }
    fn publish(&mut self, revision: u64, command: &'static str) {
        self.revision = revision;
        self.admission_identity = Arc::new(());
        self.events.push_back(DocChanged { revision, command });
        if self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
    }

    fn prepare(&self, effect: Effect) -> Result<Prepared, Error> {
        self.context(Phase::Dispatch).editable()?;
        let (document, inverse, forward, state, direction, gesture, result) = match effect {
            Effect::Edit { changes, result, gesture } => {
                validate_json(&result)?;
                if changes.changes.is_empty() {
                    return Err(invalid("command must return a nonempty changeset"));
                }
                let gesture = move_key(&changes, gesture)?;
                let (document, inverse) = changes.stage(&self.document)?;
                // Graph canonical sizing includes the operation array. History stores the full
                // {"changes": ...} envelope too (12 bytes), including reserved AddNode replay.
                changes.validate_replay_budget(MAX_HISTORY_BYTES.saturating_sub(12))?;
                (
                    document,
                    inverse,
                    changes,
                    self.revision.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "document revision overflow"))?,
                    Direction::Edit,
                    gesture,
                    result,
                )
            }
            Effect::Undo => {
                let entry = self.undo.entries.back().ok_or_else(|| Error::new(ErrorKind::Conflict, "nothing to undo"))?;
                let (document, inverse) = entry.inverse.stage(&self.document)?;
                (document, inverse, entry.inverse.clone(), entry.state, Direction::Undo, None, Value::Null)
            }
            Effect::Redo => {
                let entry = self.redo.entries.back().ok_or_else(|| Error::new(ErrorKind::Conflict, "nothing to redo"))?;
                let (document, inverse) = entry.inverse.stage(&self.document)?;
                (document, inverse, entry.inverse.clone(), entry.state, Direction::Redo, None, Value::Null)
            }
            Effect::None { .. } | Effect::Service { .. } | Effect::Open { .. } | Effect::Save { .. } => {
                return Err(invalid("non-document effects must bypass history staging"));
            }
        };
        let bytes = history_bytes(&inverse)?;
        let admission = if self.persistence.is_some() {
            // The checkpoint admitted the v1 root. Complete immutable forward/inverse
            // payload guards preserve that representation without another project walk.
            let replay = nodecraft_format::journal::AdmittedReplay::new(forward, &self.assets)?;
            nodecraft_format::journal::validate_replay(&inverse, &self.assets)?;
            replay.validate_changed_assets(&document, &self.assets)?;
            Some(PersistentAdmission { replay })
        } else {
            None
        };
        Ok(Prepared {
            document,
            inverse: Entry { inverse, bytes, state: self.state },
            direction,
            gesture,
            result,
            admission,
            source: SourceStamp { identity: Arc::clone(&self.admission_identity), revision: self.revision },
            state,
        })
    }

    /// Each frontend/subscriber owns a cursor. On `Conflict`, reread `document()` and reset
    /// the cursor to `revision()`. Cursor zero subscribes from session creation.
    pub fn events_since(&self, cursor: u64) -> Result<Vec<DocChanged>, Error> {
        if cursor > self.revision {
            return Err(invalid("event cursor is ahead of the session"));
        }
        if self.events.front().is_some_and(|first| cursor < first.revision.saturating_sub(1)) {
            return Err(Error::new(ErrorKind::Conflict, "event history expired; reload the document and reset the revision cursor"));
        }
        Ok(self.events.iter().filter(|event| event.revision > cursor).cloned().collect())
    }
}

pub(crate) fn validate_gesture(gesture: Option<&str>) -> Result<(), Error> {
    if gesture.is_some_and(|token| token.is_empty() || token.len() > 128) {
        return Err(invalid("gesture must contain 1 to 128 bytes"));
    }
    Ok(())
}

fn move_key(changes: &Changeset, gesture: Option<String>) -> Result<Option<MoveKey>, Error> {
    let Some(gesture) = gesture else { return Ok(None) };
    validate_gesture(Some(&gesture))?;
    match changes.changes.as_slice() {
        [Change::MoveNode { canvas, id, .. }] => Ok(Some(MoveKey { canvas: *canvas, node: *id, gesture })),
        _ => Err(invalid("gesture coalescing requires exactly one node move")),
    }
}

pub(crate) fn guard<T>(operation: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    catch_unwind(AssertUnwindSafe(operation))
        .map_err(|_| Error::new(ErrorKind::Internal, "command failed unexpectedly; document and undo history were preserved"))?
}

fn history_bytes(changes: &Changeset) -> Result<usize, Error> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > MAX_HISTORY_BYTES {
                return Err(std::io::Error::other("undo step exceeds history budget"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, changes).map_err(|_| Error::new(ErrorKind::InvalidInput, "undo step exceeds history budget"))?;
    Ok(counter.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Canvas, Generation, Node};
    use nodecraft_types::{GenerationId, ProjectId};
    use serde_json::json;

    #[test]
    fn non_document_effect_works_at_exhausted_document_revision() {
        fn enabled(_: &Context<'_>, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn service(_: &Context<'_>, _: &Value) -> Result<Effect, Error> {
            Ok(Effect::None { result: json!("ok") })
        }
        let mut session = Session::new(Project::new(ProjectId::from_bytes([1; 16]), "Service").unwrap()).unwrap();
        session
            .registry
            .register(crate::commands::CommandSpec {
                id: "test.service",
                label: "Service",
                shortcut: None,
                alternate_shortcuts: &[],
                input_schema: || json!({}),
                params_doc: "{}",
                enabled,
                run: service,
            })
            .unwrap();
        session.revision = u64::MAX;
        assert_eq!(session.dispatch("test.service", &Value::Null).unwrap(), json!("ok"));
        assert_eq!(session.revision(), u64::MAX);
        assert!(!session.can_undo());
        assert!(session.events.is_empty());
    }

    #[test]
    fn canonical_redo_budget_includes_full_changeset_envelope() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let node = Node::new(NodeId::from_bytes([2; 16]), "test.text");
        let changes = Changeset::new(vec![Change::AddNode { canvas, node: node.clone() }]);
        let restore = Changeset::new(vec![Change::RestoreNode { canvas, node, edges: vec![] }]);
        let full = history_bytes(&restore).unwrap();
        assert!(changes.validate_replay_budget(full - 12).is_ok());
        assert!(changes.validate_replay_budget(full - 13).is_err());
        // The original AddNode spelling is smaller; counting it alone loses the redo boundary.
        assert!(history_bytes(&changes).unwrap() < full);
    }

    #[test]
    fn history_byte_counter_accepts_exact_budget_and_rejects_next_byte() {
        let mut changes = Changeset::new(vec![Change::SetProjectTitle { title: String::new() }]);
        let overhead = history_bytes(&changes).unwrap();
        changes.changes = vec![Change::SetProjectTitle { title: "x".repeat(MAX_HISTORY_BYTES - overhead) }];
        assert_eq!(history_bytes(&changes).unwrap(), MAX_HISTORY_BYTES);
        changes.changes = vec![Change::SetProjectTitle { title: "x".repeat(MAX_HISTORY_BYTES - overhead + 1) }];
        assert!(history_bytes(&changes).is_err());
    }

    #[test]
    fn generation_history_records_only_affected_entry_in_both_directions() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let id = NodeId::from_bytes([2; 16]);
        let mut document = Project::new(ProjectId::from_bytes([3; 16]), "Compact history").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
        let mut node = Node::new(id, "test.text");
        for n in 0..6 {
            node.history.push(Generation {
                id: GenerationId::from_bytes([n; 16]),
                outputs: vec![nodecraft_types::Value::Text("x".repeat(512 * 1024))],
                input_hash: None,
            });
        }
        Change::AddNode { canvas, node }.apply(&mut document).unwrap();
        let session = Session::new(document).unwrap();
        let generation = Generation { id: GenerationId::from_bytes([9; 16]), outputs: vec![nodecraft_types::Value::Text("new".into())], input_hash: None };
        let changes = Changeset::new(vec![Change::AddGeneration { canvas, id, index: 3, generation }]);
        let prepared = session.prepare(Effect::Edit { changes, result: json!(null), gesture: None }).unwrap();
        assert!(prepared.inverse.bytes < 512);
        let mut edited = prepared.document;
        let redo = prepared.inverse.inverse.apply(&mut edited).unwrap();
        assert!(history_bytes(&redo).unwrap() < 512);
        assert_eq!(edited, *session.document());
        let removal = session
            .prepare(Effect::Edit {
                changes: Changeset::new(vec![Change::RemoveGeneration { canvas, id, generation: GenerationId::from_bytes([2; 16]) }]),
                result: json!(null),
                gesture: None,
            })
            .unwrap();
        assert!(removal.inverse.bytes < 513 * 1024);
    }
    #[test]
    fn generation_transactions_recover_identically_through_undo_redo_and_atomic_failures() {
        use nodecraft_format::{MemoryStorage, StorageLocation, journal};
        use std::{cell::RefCell, rc::Rc};
        #[derive(Clone)]
        struct Shared(Rc<RefCell<MemoryStorage>>);
        impl ProjectStorage for Shared {
            fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
                self.0.borrow_mut().load(l)
            }
            fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().save(l, b)
            }
            fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().checkpoint(b)
            }
            fn append(&mut self, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().append(b)
            }
        }
        fn enabled(_: &Context<'_>, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn edit(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
            Ok(Effect::Edit { changes: crate::commands::params(value)?, result: Value::Null, gesture: None })
        }
        let canvas = CanvasId::from_bytes([1; 16]);
        let id = NodeId::from_bytes([2; 16]);
        let mut project = Project::new(ProjectId::from_bytes([3; 16]), "Generations").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let mut node = Node::new(id, "test.text");
        for n in [5, 7] {
            node.history.push(Generation {
                id: GenerationId::from_bytes([n; 16]),
                outputs: vec![nodecraft_types::Value::Text(format!("output{n}"))],
                input_hash: None,
            });
        }
        node.active = Some(GenerationId::from_bytes([5; 16]));
        Change::AddNode { canvas, node }.apply(&mut project).unwrap();
        let shared = Shared(Rc::new(RefCell::new(MemoryStorage::default())));
        let mut session = Session::with_storage(Snapshot { project: project.clone(), assets: Default::default(), epoch: 0 }, Box::new(shared.clone())).unwrap();
        session
            .registry
            .register(crate::commands::CommandSpec {
                id: "test.generations",
                label: "Generation edits",
                shortcut: None,
                alternate_shortcuts: &[],
                input_schema: || json!({}),
                params_doc: "Changeset",
                enabled,
                run: edit,
            })
            .unwrap();
        let hash = session.import_asset(b"generation asset").unwrap();
        let generation = Generation {
            input_hash: None,
            id: GenerationId::from_bytes([6; 16]),
            outputs: vec![nodecraft_types::Value::List(vec![nodecraft_types::Value::Media(nodecraft_types::AssetRef {
                hash,
                mime: "text/plain".parse().unwrap(),
                meta: Default::default(),
            })])],
        };
        let changes = Changeset::new(vec![
            Change::AddGeneration { canvas, id, index: 1, generation },
            Change::SetActiveGeneration { canvas, id, generation: Some(GenerationId::from_bytes([6; 16])) },
            Change::RemoveGeneration { canvas, id, generation: GenerationId::from_bytes([5; 16]) },
        ]);
        session.dispatch("test.generations", &serde_json::to_value(changes).unwrap()).unwrap();
        let edited = session.document().clone();
        for command in ["edit.undo", "edit.redo"] {
            let recovered = journal::recover(&shared.0.borrow().recovery).unwrap();
            assert_eq!(&recovered.snapshot.project, session.document());
            assert_eq!(recovered.snapshot.assets.get(&hash), Some(b"generation asset".as_slice()));
            session.dispatch(command, &Value::Null).unwrap();
            if command == "edit.undo" {
                assert_eq!(session.document(), &project);
            } else {
                assert_eq!(session.document(), &edited);
            }
        }
        assert_eq!(journal::recover(&shared.0.borrow().recovery).unwrap().snapshot.project, edited);
        let bytes = shared.0.borrow().recovery.clone();
        let events = session.events_since(0).unwrap();
        let revision = session.revision();
        let changes = Changeset::new(vec![
            Change::SetProjectTitle { title: "Never published".into() },
            Change::RemoveGeneration { canvas, id, generation: GenerationId::from_bytes([6; 16]) },
        ]);
        assert!(session.dispatch("test.generations", &serde_json::to_value(changes).unwrap()).is_err());
        assert_eq!(session.document(), &edited);
        assert_eq!(session.revision(), revision);
        assert_eq!(session.events_since(0).unwrap(), events);
        assert_eq!(shared.0.borrow().recovery, bytes);
        assert!(!session.file_status().recovery_required);
    }
    #[test]
    fn incremental_large_graph_delete_reserves_replay_after_empty_checkpoint() {
        use nodecraft_format::{MemoryStorage, StorageLocation, journal};
        use nodecraft_graph::Comment;
        use nodecraft_types::{AssetRef, CommentId, MediaMeta};
        use std::{cell::RefCell, rc::Rc};
        #[derive(Clone)]
        struct Shared(Rc<RefCell<MemoryStorage>>);
        impl ProjectStorage for Shared {
            fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
                self.0.borrow_mut().load(l)
            }
            fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().save(l, b)
            }
            fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().checkpoint(b)
            }
            fn append(&mut self, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().append(b)
            }
        }
        fn enabled(_: &Context<'_>, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn edit(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
            let canvas = CanvasId::from_bytes([1; 16]);
            let change = match value["op"].as_str().unwrap() {
                "comment" => Change::PutComment {
                    canvas,
                    comment: Comment {
                        id: CommentId::from_bytes([value["n"].as_u64().unwrap() as u8; 16]),
                        text: "x".repeat(usize::try_from(value["len"].as_u64().unwrap()).unwrap()),
                        pos: Default::default(),
                    },
                },
                "node" => {
                    let mut node = Node::new(NodeId::from_bytes([value["n"].as_u64().unwrap() as u8; 16]), "test.media");
                    node.params.insert(
                        "asset".into(),
                        nodecraft_types::Value::Media(AssetRef {
                            hash: serde_json::from_value(value["hash"].clone()).unwrap(),
                            mime: "text/plain".parse().unwrap(),
                            meta: MediaMeta::default(),
                        }),
                    );
                    Change::AddNode { canvas, node }
                }
                "delete" => Change::RemoveCanvas { id: canvas },
                _ => panic!("unexpected fixture operation"),
            };
            Ok(Effect::Edit { changes: Changeset::new(vec![change]), result: Value::Null, gesture: None })
        }
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Large boundary").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Large") }.apply(&mut project).unwrap();
        let storage = Shared(Rc::new(RefCell::new(MemoryStorage::default())));
        let mut session = Session::with_storage(Snapshot { project, assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
        session
            .registry
            .register(crate::commands::CommandSpec {
                id: "test.large",
                label: "Boundary edits",
                shortcut: None,
                alternate_shortcuts: &[],
                input_schema: || json!({}),
                params_doc: "fixture",
                enabled,
                run: edit,
            })
            .unwrap();
        for n in 0..3_u8 {
            let hash = session.import_asset(&vec![b'a' + n; nodecraft_format::MAX_ASSET_BYTES]).unwrap();
            session.dispatch("test.large", &json!({"op":"node","n":10+n,"hash":hash})).unwrap();
        }
        for n in 0..16_u8 {
            session.dispatch("test.large", &json!({"op":"comment","n":50+n,"len":if n==15 {0} else {1024*1024}})).unwrap();
        }
        let delete = Changeset::new(vec![Change::RemoveCanvas { id: canvas }]);
        let base_size = {
            let (_, inverse) = delete.stage(session.document()).unwrap();
            history_bytes(&inverse).unwrap()
        };
        let last_len = MAX_HISTORY_BYTES.checked_sub(64).unwrap().checked_sub(base_size).unwrap();
        assert!(last_len < 1024 * 1024);
        session.dispatch("test.large", &json!({"op":"comment","n":65,"len":last_len})).unwrap();
        let revision = session.revision();
        let events = session.events_since(0).unwrap();
        let recovery_len = storage.0.borrow().recovery.len();
        let undo = session.undo.entries.len();
        // The changeset fits engine history, but max-sequence Record+48MiB assets+headers do not.
        assert!(session.dispatch("test.large", &json!({"op":"delete"})).is_err());
        assert_eq!(session.revision(), revision);
        assert_eq!(session.events_since(0).unwrap(), events);
        assert_eq!(storage.0.borrow().recovery.len(), recovery_len);
        assert_eq!(session.undo.entries.len(), undo);
        assert!(!session.file_status().recovery_required);
        // Move safely below both record and binary-frame limits, then exercise the actual future
        // checkpoint shape that omits every retained asset and must re-add them during undo.
        session.dispatch("test.large", &json!({"op":"comment","n":65,"len":last_len-4096})).unwrap();
        session.dispatch("test.large", &json!({"op":"delete"})).unwrap();
        assert!(session.document().canvases().is_empty());
        session.dispatch("file.save_as", &json!({"location":"empty.ncraft"})).unwrap();
        assert_eq!(session.assets().byte_len(), nodecraft_format::MAX_TOTAL_ASSET_BYTES);
        session.dispatch("edit.undo", &Value::Null).unwrap();
        assert_eq!(session.document().canvases().get(&canvas).unwrap().nodes().len(), 3);
        let recovered = journal::recover(&storage.0.borrow().recovery).unwrap();
        assert_eq!(&recovered.snapshot.project, session.document());
        assert_eq!(recovered.snapshot.assets.byte_len(), nodecraft_format::MAX_TOTAL_ASSET_BYTES);
        session.dispatch("edit.redo", &Value::Null).unwrap();
        assert!(session.document().canvases().is_empty());
    }
}

#[cfg(test)]
#[path = "persistence_tests.rs"]
mod persistence_tests;

#[cfg(test)]
#[path = "admission_tests.rs"]
mod admission_tests;

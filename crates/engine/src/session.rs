use std::{
    collections::VecDeque,
    io::Write,
    panic::{AssertUnwindSafe, catch_unwind},
};

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
            document: &self.document,
            kinds: &self.kinds,
            can_undo: self.can_undo(),
            can_redo: self.can_redo(),
            resources: &self.resources,
            services: &self.services,
            phase,
        }
    }

    /// Returns an actionable reason when disabled; never mutates the session.
    pub fn enabled(&self, id: &str, params: &Value) -> Result<(), Error> {
        guard(|| {
            validate_json(params)?;
            (self.registry.get(id)?.enabled)(&self.context(Phase::Preview), params)
        })
    }

    pub fn dispatch(&mut self, id: &str, params: &Value) -> Result<Value, Error> {
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
            document_effect => document_effect,
        };
        let revision = self.revision.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "document revision overflow"))?;
        let prepared = guard(|| self.prepare(effect))?;
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
        self.revision = revision;
        self.events.push_back(DocChanged { revision, command: spec.id });
        if self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        Ok(prepared.result)
    }

    fn prepare(&self, effect: Effect) -> Result<Prepared, Error> {
        let (document, inverse, direction, gesture, result) = match effect {
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
                (document, inverse, Direction::Edit, gesture, result)
            }
            Effect::Undo => {
                let entry = self.undo.entries.back().ok_or_else(|| Error::new(ErrorKind::Conflict, "nothing to undo"))?;
                let (document, inverse) = entry.inverse.stage(&self.document)?;
                (document, inverse, Direction::Undo, None, Value::Null)
            }
            Effect::Redo => {
                let entry = self.redo.entries.back().ok_or_else(|| Error::new(ErrorKind::Conflict, "nothing to redo"))?;
                let (document, inverse) = entry.inverse.stage(&self.document)?;
                (document, inverse, Direction::Redo, None, Value::Null)
            }
            Effect::None { .. } | Effect::Service { .. } => return Err(invalid("non-document effects must bypass history staging")),
        };
        let bytes = history_bytes(&inverse)?;
        Ok(Prepared { document, inverse: Entry { inverse, bytes }, direction, gesture, result })
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

fn guard<T>(operation: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
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
}

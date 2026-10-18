//! Shared command contract. Handlers inspect immutable state and return changesets; only
//! `Session` commits them, records undo and publishes events. Handlers must not perform I/O.

use std::{
    any::{Any, TypeId},
    collections::{BTreeMap, HashMap},
};

use nodecraft_graph::{Changeset, Project};
use nodecraft_types::{Error, ErrorKind};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Host-owned immutable snapshots/capabilities. No settings persistence or job queue is implied.
/// Inject resources when constructing a session; handlers return effects rather than mutating them.
#[derive(Default)]
pub struct Resources {
    values: HashMap<TypeId, Box<dyn Any>>,
}
impl Resources {
    pub fn insert<T: Any>(&mut self, value: T) {
        self.values.insert(TypeId::of::<T>(), Box::new(value));
    }
    pub fn swap<T: Any>(&mut self, value: Option<T>) -> Option<T> {
        let old = match value {
            Some(value) => self.values.insert(TypeId::of::<T>(), Box::new(value)),
            None => self.values.remove(&TypeId::of::<T>()),
        };
        old.and_then(|value| value.downcast::<T>().ok()).map(|value| *value)
    }
    pub fn get<T: Any>(&self) -> Option<&T> {
        self.values.get(&TypeId::of::<T>()).and_then(|value| value.downcast_ref())
    }
    pub fn require<T: Any>(&self) -> Result<&T, Error> {
        self.get().ok_or_else(|| Error::new(ErrorKind::Unsupported, "required host capability is unavailable"))
    }
}

type ServiceHandler = Box<dyn FnMut(&Value) -> Result<Value, Error>>;

/// Host services run only after a handler returns a Service effect. The session guards and
/// bounds requests/results and invokes at most once without automatic retries. External effects
/// may already be committed on an error, invalid response or panic: hosts own atomicity,
/// idempotency and reconciliation. Only document/history preservation is the session's guarantee.
#[derive(Default)]
pub struct Services {
    handlers: BTreeMap<&'static str, ServiceHandler>,
}
impl Services {
    pub fn register(&mut self, id: &'static str, handler: impl FnMut(&Value) -> Result<Value, Error> + 'static) -> Result<(), Error> {
        validate_id(id)?;
        if self.handlers.contains_key(id) {
            return Err(Error::new(ErrorKind::Conflict, "host service is already registered"));
        }
        self.handlers.insert(id, Box::new(handler));
        Ok(())
    }
    pub fn contains(&self, id: &str) -> bool {
        self.handlers.contains_key(id)
    }
    pub(crate) fn invoke(&mut self, id: &str, request: &Value) -> Result<Value, Error> {
        let handler = self.handlers.get_mut(id).ok_or_else(|| Error::new(ErrorKind::Unsupported, "required host service is unavailable"))?;
        handler(request)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Describe prospective effects for availability without I/O. Read-only queries may
    /// skip computing their result after validating parameters and required capabilities.
    Preview,
    Dispatch,
}

pub struct Context<'a> {
    pub document: &'a Project,
    /// The document's revision: bumped by every edit, undo and redo.
    pub revision: u64,
    pub kinds: &'a nodecraft_nodes::Registry,
    pub can_undo: bool,
    pub can_redo: bool,
    pub resources: &'a Resources,
    pub services: &'a Services,
    pub phase: Phase,
    pub file: crate::file_cmds::FileStatus,
}
impl Context<'_> {
    /// Session::enabled/dispatch graph-stage every returned document effect once,
    /// including custom handlers. Handler callbacks only check editability/parameters.
    pub fn editable(&self) -> Result<(), Error> {
        if self.file.recovery_required { Err(Error::new(ErrorKind::Conflict, "recovery journal needs a checkpoint; save before editing")) } else { Ok(()) }
    }
    pub fn preview_changes(&self, _changes: &Changeset) -> Result<(), Error> {
        self.editable()
    }
}

pub enum Effect {
    Open { location: nodecraft_format::StorageLocation, discard_dirty: bool },
    Save { location: Option<nodecraft_format::StorageLocation> },
    Service { capability: &'static str, request: Value },
    None { result: Value },
    Edit { changes: Changeset, result: Value, gesture: Option<String> },
    Undo,
    Redo,
}

#[derive(Clone, Copy)]
pub struct CommandSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// Platform-independent shortcut spelling; frontends map Command to their primary modifier.
    pub shortcut: Option<&'static str>,
    pub alternate_shortcuts: &'static [&'static str],
    /// JSON Schema returned on demand for forms and MCP inputSchema.
    pub input_schema: fn() -> Value,
    pub params_doc: &'static str,
    pub enabled: fn(&Context<'_>, &Value) -> Result<(), Error>,
    pub run: fn(&Context<'_>, &Value) -> Result<Effect, Error>,
}

#[derive(Default)]
pub struct Registry {
    commands: BTreeMap<&'static str, CommandSpec>,
}

impl Registry {
    pub fn builtins() -> Result<Self, Error> {
        let mut registry = Self::default();
        for spec in crate::graph_cmds::specs()
            .into_iter()
            .chain(crate::node_cmds::specs())
            .chain(crate::edge_cmds::specs())
            .chain(crate::keys_cmds::specs())
            .chain(crate::stale_cmds::specs())
            .chain(crate::navigation_cmds::specs())
            .chain(crate::create_cmds::specs())
            .chain(crate::clipboard_cmds::specs())
            .chain(crate::search_cmds::specs())
            .chain(crate::file_cmds::specs())

            .chain(crate::run_cmds::specs())
        {
            registry.register(spec)?;
        }
        Ok(registry)
    }

    pub fn register(&mut self, spec: CommandSpec) -> Result<(), Error> {
        validate_id(spec.id)?;
        if self.commands.contains_key(spec.id) {
            return Err(Error::new(ErrorKind::Conflict, "command id is already registered"));
        }
        self.commands.insert(spec.id, spec);
        Ok(())
    }

    pub fn specs(&self) -> impl Iterator<Item = &CommandSpec> {
        self.commands.values()
    }

    pub fn get(&self, id: &str) -> Result<&CommandSpec, Error> {
        self.commands.get(id).ok_or_else(|| Error::new(ErrorKind::NotFound, "unknown command id"))
    }
}

fn validate_id(id: &str) -> Result<(), Error> {
    if id.len() > 128
        || !id.contains('.')
        || id.split('.').any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
    {
        return Err(invalid("command/service id must contain lowercase dotted names (letters, digits and underscores)"));
    }
    Ok(())
}

pub(crate) fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

enum JsonItems<'a> {
    Root(std::option::IntoIter<&'a Value>),
    Array(std::slice::Iter<'a, Value>),
    Object(serde_json::map::Values<'a>),
}

impl<'a> JsonItems<'a> {
    fn next(&mut self) -> Option<&'a Value> {
        match self {
            Self::Root(iter) => iter.next(),
            Self::Array(iter) => iter.next(),
            Self::Object(iter) => iter.next(),
        }
    }
}

/// Also protects callers that construct JSON values programmatically instead of using a
/// text decoder. The iterator stack is bounded by nesting depth, not array width. Public so a
/// frontend can check parameters it will send later (e.g. a copy that must paste back).
pub fn validate_json(root: &Value) -> Result<(), Error> {
    const MAX_ITEMS: usize = 16_384;
    const MAX_BYTES: usize = 1024 * 1024;
    let mut stack = vec![JsonItems::Root(Some(root).into_iter())];
    let mut items = 0_usize;
    let mut bytes = 0_usize;
    while let Some(iter) = stack.last_mut() {
        let Some(value) = iter.next() else {
            stack.pop();
            continue;
        };
        items = items.saturating_add(1);
        if items > MAX_ITEMS || stack.len() > 64 {
            return Err(invalid("command JSON exceeds item or depth limit"));
        }
        match value {
            Value::String(text) => bytes = bytes.saturating_add(text.len()),
            Value::Array(values) => {
                if values.len() > MAX_ITEMS {
                    return Err(invalid("command JSON exceeds item limit"));
                }
                stack.push(JsonItems::Array(values.iter()));
            }
            Value::Object(values) => {
                if values.len() > MAX_ITEMS {
                    return Err(invalid("command JSON exceeds item limit"));
                }
                for key in values.keys() {
                    bytes = bytes.saturating_add(key.len());
                }
                stack.push(JsonItems::Object(values.values()));
            }
            _ => {}
        }
        if bytes > MAX_BYTES {
            return Err(invalid("command JSON exceeds text limit"));
        }
    }
    Ok(())
}

pub(crate) fn params<T: DeserializeOwned>(value: &Value) -> Result<T, Error> {
    validate_json(value)?;
    serde_path_to_error::deserialize(value).map_err(|error| {
        // Never echo raw serde diagnostics, enum variants or caller-controlled map keys.
        let diagnostic = error.inner().to_string();
        let category = if diagnostic.starts_with("missing field") {
            "missing field"
        } else if diagnostic.starts_with("unknown field") {
            "unknown field"
        } else if diagnostic.starts_with("invalid type") {
            "wrong type"
        } else {
            "invalid value"
        };
        let mut path = String::from("params");
        for segment in error.path().iter().take(64) {
            match segment {
                serde_path_to_error::Segment::Seq { index } => path.push_str(&format!("[{index}]")),
                serde_path_to_error::Segment::Map { key } if safe_field(key) => {
                    path.push('.');
                    path.push_str(key);
                }
                _ => path.push_str(".<field>"),
            }
        }
        if category == "missing field"
            && let Some(field) = diagnostic.strip_prefix("missing field `").and_then(|s| s.split('`').next()).filter(|s| safe_field(s))
        {
            path.push('.');
            path.push_str(field);
        }
        Error::new(ErrorKind::InvalidInput, format!("{path}: {category}; consult the command input schema"))
    })
}
fn safe_field(field: &str) -> bool {
    matches!(
        field,
        "canvas"
            | "node"
            | "id"
            | "kind"
            | "title"
            | "pos"
            | "x"
            | "y"
            | "params"
            | "inputs"
            | "outputs"
            | "history"
            | "active"
            | "tag"
            | "field"
            | "name"
            | "port_type"
            | "modality"
            | "many"
            | "edge"
            | "from"
            | "to"
            | "port"
            | "order"
            | "gesture"
            | "type"
            | "value"
            | "location"
            | "discard_dirty"
    )
}

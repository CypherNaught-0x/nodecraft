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

type RunHandler = Box<dyn FnMut(crate::run_cmds::PreparedRun) -> Result<Value, Error>>;

type ServiceHandler = Box<dyn FnMut(&Value) -> Result<Value, Error>>;

/// Host services run only after a handler returns a Service effect. The session guards and
/// bounds requests/results and invokes at most once without automatic retries. External effects
/// may already be committed on an error, invalid response or panic: hosts own atomicity,
/// idempotency and reconciliation. Only document/history preservation is the session's guarantee.
#[derive(Default)]
pub struct Services {
    run_host: Option<RunHandler>,
    handlers: BTreeMap<&'static str, ServiceHandler>,
}
impl Services {
    pub fn register(&mut self, id: &'static str, handler: impl FnMut(&Value) -> Result<Value, Error> + 'static) -> Result<(), Error> {
        if id == crate::run_cmds::RUN_SERVICE {
            return Err(invalid("use the typed run host registration"));
        }
        validate_id(id)?;
        if self.handlers.contains_key(id) {
            return Err(Error::new(ErrorKind::Conflict, "host service is already registered"));
        }
        self.handlers.insert(id, Box::new(handler));
        Ok(())
    }
    pub fn register_run_host(&mut self, handler: impl FnMut(crate::run_cmds::PreparedRun) -> Result<Value, Error> + 'static) -> Result<(), Error> {
        if self.run_host.is_some() {
            return Err(Error::new(ErrorKind::Conflict, "run host already registered"));
        }
        self.run_host = Some(Box::new(handler));
        Ok(())
    }
    pub fn contains_run_host(&self) -> bool {
        self.run_host.is_some()
    }
    pub(crate) fn invoke_run_host(&mut self, prepared: crate::run_cmds::PreparedRun) -> Result<Value, Error> {
        self.run_host.as_mut().ok_or_else(|| Error::new(ErrorKind::Unsupported, "run host is unavailable"))?(prepared)
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
    pub(crate) assets: &'a nodecraft_format::AssetStore,
    pub(crate) routing: &'a crate::provider_routing::RoutingAuthority,
    pub(crate) execution: &'a crate::execution_identity::ExecutionSource,
    pub document: &'a Project,
    /// The document's revision: bumped by every edit, undo and redo.
    pub revision: u64,
    pub kinds: &'a nodecraft_nodes::Registry,
    /// The session's commands, so a command can say which others exist (`skill.list`).
    pub commands: &'a Registry,
    pub can_undo: bool,
    pub can_redo: bool,
    pub resources: &'a Resources,
    pub services: &'a Services,
    pub phase: Phase,
    pub file: crate::file_cmds::FileStatus,
    /// The session's performance recorder, for `perf.report`.
    pub perf: &'a crate::perf::Recorder,
}
impl Context<'_> {
    /// Validates against the session's immutable destination bytes, never a project UUID.
    pub fn validate_asset(&self, asset: &nodecraft_types::AssetRef) -> Result<(), Error> {
        self.assets.validate_reference(asset)
    }
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
    Open {
        location: nodecraft_format::StorageLocation,
        discard_dirty: bool,
        /// Read on the host's save runner, when it has one, instead of inline.
        background: bool,
    },
    /// Ask the background save or open to stop (`file.cancel`); never waits for it.
    CancelFile,
    Save {
        location: Option<nodecraft_format::StorageLocation>,
        /// Write on the host's save runner, when it has one, instead of inline.
        background: bool,
    },
    PreparedRun {
        prepared: Box<crate::run_cmds::PreparedRun>,
    },
    Service {
        capability: &'static str,
        request: Value,
    },
    None {
        result: Value,
    },
    Edit {
        changes: Changeset,
        result: Value,
        gesture: Option<String>,
    },
    Undo,
    Redo,
    /// Write one asset's bytes (`history.export`) or an interchange archive to a file; never
    /// over one unless `overwrite`.
    Export {
        what: crate::export::Exported,
        path: std::path::PathBuf,
        overwrite: bool,
    },
    /// Start, stop or clear the session's performance recorder (`perf_cmds`).
    Perf {
        action: crate::perf_cmds::PerfAction,
    },
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
            .chain(crate::assist_cmds::specs())
            .chain(crate::bulk_cmds::specs())
            .chain(crate::model_cmds::specs())
            .chain(crate::edge_cmds::specs())
            .chain(crate::keys_cmds::specs())
            .chain(crate::stale_cmds::specs())
            .chain(crate::sheet_cmds::specs())
            .chain(crate::technique_cmds::specs())
            .chain(crate::technique_node_cmds::specs())
            .chain(crate::app_cmds::specs())
            .chain(crate::library_cmds::specs())
            .chain(crate::describe_cmds::specs())
            .chain(crate::option_cmds::specs())
            .chain(crate::option_fetch::specs())
            .chain(crate::update_check::specs())
            .chain(crate::update_install::specs())
            .chain(crate::navigation_cmds::specs())
            .chain(crate::create_cmds::specs())
            .chain(crate::clipboard_cmds::specs())
            .chain(crate::search_cmds::specs())
            .chain(crate::file_cmds::specs())
            .chain(crate::layout_cmds::specs())
            .chain(crate::comment_cmds::specs())
            .chain(crate::run_cmds::specs())
            .chain(crate::perf_cmds::specs())
            .chain(crate::cache_cmds::specs())
            .chain(crate::shader_cmds::specs())
            .chain(crate::mask_cmds::specs())
            .chain(crate::mesh_cmds::specs())
            .chain(crate::document_cmds::specs())
            .chain(crate::layer_cmds::specs())
            .chain(crate::timeline_cmds::specs())
            .chain(crate::interchange_cmds::specs())
            .chain(crate::timeline_captions::specs())
            .chain(crate::element_cmds::specs())
            .chain(crate::style_cmds::specs())
            .chain(crate::plugin_cmds::specs())
            .chain(crate::ownership_cmds::specs())
            .chain(crate::resolve_cmds::specs())
            .chain(crate::held_cmds::specs())
            .chain(crate::order_cmds::specs())
            .chain(crate::history_cmds::specs())
            .chain(crate::export_cmds::specs())
            .chain(crate::turn_into_cmds::specs())
            .chain(crate::tag_cmds::specs())
            .chain(crate::tag_filter_cmds::specs())
            .chain(crate::skill_cmds::specs())
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

    pub fn has(&self, id: &str) -> bool {
        self.commands.contains_key(id)
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
/// text decoder. The iterator stack is bounded by nesting depth, not array width.
pub fn validate_json(root: &Value) -> Result<(), Error> {
    validate_json_limit(root, 1024 * 1024)
}

/// Input content retains its per-field limits; this small allowance holds IDs and field names.
pub(crate) fn validate_input_json(root: &Value) -> Result<(), Error> {
    validate_json_limit(root, 1024 * 1024 + 1024)
}

fn validate_json_limit(root: &Value, max_bytes: usize) -> Result<(), Error> {
    const MAX_ITEMS: usize = 16_384;
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
        if bytes > max_bytes {
            return Err(invalid("command JSON exceeds text limit"));
        }
    }
    Ok(())
}

pub(crate) fn params<T: DeserializeOwned>(value: &Value) -> Result<T, Error> {
    validate_input_json(value)?;
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

#[cfg(test)]
mod plugin_ids {
    use nodecraft_plugin::id::{LocalId, PackageId, RegisteredId};

    /// The plugin contract's registered IDs are command IDs as they are, longest included
    /// (#171): nothing is rewritten to fit.
    #[test]
    fn registered_plugin_ids_are_valid_command_ids() {
        let longest = RegisteredId::new(
            &PackageId::parse(&format!("{}.{}.{}.{}", "a".repeat(24), "b".repeat(24), "c".repeat(24), "d".repeat(5))).unwrap(),
            &LocalId::parse(&format!("{}.{}", "e".repeat(15), "f".repeat(15))).unwrap(),
        );
        for id in [longest, RegisteredId::new(&PackageId::parse("io.example.textpack").unwrap(), &LocalId::parse("normalize").unwrap())] {
            super::validate_id(id.as_str()).unwrap();
        }
    }
}

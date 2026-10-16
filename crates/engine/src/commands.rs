//! Shared command contract. Handlers inspect immutable state and return changesets; only
//! `Session` commits them, records undo and publishes events. Handlers must not perform I/O.

use std::collections::BTreeMap;

use nodecraft_graph::{Changeset, Project};
use nodecraft_types::{Error, ErrorKind};
use serde::de::DeserializeOwned;
use serde_json::Value;

pub struct Context<'a> {
    pub document: &'a Project,
    pub can_undo: bool,
    pub can_redo: bool,
}

pub enum Effect {
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
        for spec in crate::graph_cmds::specs() {
            registry.register(spec)?;
        }
        Ok(registry)
    }

    pub fn register(&mut self, spec: CommandSpec) -> Result<(), Error> {
        if spec.id.len() > 128
            || !spec.id.contains('.')
            || spec.id.split('.').any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
        {
            return Err(invalid("command id must contain lowercase dotted names"));
        }
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
pub(crate) fn validate_json(root: &Value) -> Result<(), Error> {
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
    // Avoid echoing values from serde diagnostics; command input may contain secrets.
    serde_json::from_value(value.clone()).map_err(|_| invalid("invalid command parameters; consult the command params documentation"))
}

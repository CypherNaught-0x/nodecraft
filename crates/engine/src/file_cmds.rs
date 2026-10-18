//! File commands are platform requests. Session performs I/O; handlers never open dialogs.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_format::StorageLocation;
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileStatus {
    pub available: bool,
    pub location: Option<StorageLocation>,
    pub dirty: bool,
    pub recovery_required: bool,
    /// The source destination cannot be overwritten; local edits and Save As remain available.
    pub read_only: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    location: String,
    #[serde(default)]
    discard_dirty: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveAs {
    location: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn available(c: &Context<'_>) -> Result<(), Error> {
    if c.file.available { Ok(()) } else { Err(Error::new(ErrorKind::Unsupported, "project storage is not configured for this session")) }
}
fn open(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: Open = params(value)?;
    available(c)?;
    if c.file.dirty && !p.discard_dirty {
        return Err(Error::new(ErrorKind::Conflict, "save your changes or explicitly set discard_dirty before opening another project"));
    }
    Ok(Effect::Open { location: StorageLocation::from_command(p.location)?, discard_dirty: p.discard_dirty })
}
fn save(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    if !value.is_null() {
        let _: Empty = params(value)?;
    }
    available(c)?;
    if c.file.read_only {
        return Err(Error::new(ErrorKind::Conflict, "source is read-only; use Save As to a writable destination"));
    }
    if c.file.location.is_none() {
        return Err(Error::new(ErrorKind::Conflict, "untitled project needs file.save_as with an explicit location"));
    }
    nodecraft_format::validate_v1_project(c.document)?;
    Ok(Effect::Save { location: None })
}
fn save_as(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: SaveAs = params(value)?;
    available(c)?;
    nodecraft_format::validate_v1_project(c.document)?;
    Ok(Effect::Save { location: Some(StorageLocation::from_command(p.location)?) })
}
fn enabled_open(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    open(c, v).map(|_| ())
}
fn enabled_save(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    save(c, v).map(|_| ())
}
fn enabled_save_as(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    save_as(c, v).map(|_| ())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "file.open",
            alternate_shortcuts: &[],
            input_schema: open_schema,
            label: "Open project",
            shortcut: Some("Command+O"),
            params_doc: "{location: string, discard_dirty?: bool}; UTF8 filesystem path on native, storage key on web. Returns {opened: true, read_only: bool}. Never opens a dialog.",
            enabled: enabled_open,
            run: open,
        },
        CommandSpec {
            id: "file.save",
            alternate_shortcuts: &[],
            input_schema: save_schema,
            label: "Save project",
            shortcut: Some("Command+S"),
            params_doc: "{}; save to the current location. Returns {saved: true, recovery_ready: bool, read_only: bool}.",
            enabled: enabled_save,
            run: save,
        },
        CommandSpec {
            id: "file.save_as",
            alternate_shortcuts: &[],
            input_schema: save_as_schema,
            label: "Save project as",
            shortcut: Some("Command+Shift+S"),
            params_doc: "{location: string}; UTF8 filesystem path on native, storage key on web. Returns {saved: true, recovery_ready: bool, read_only: bool}.",
            enabled: enabled_save_as,
            run: save_as,
        },
    ]
}

fn location_schema() -> Value {
    serde_json::json!({"type":"string","minLength":1,"maxLength":4096})
}
fn open_schema() -> Value {
    serde_json::json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","required":["location"],"properties":{
    "location":location_schema(),"discard_dirty":{"type":"boolean","default":false}},"additionalProperties":false})
}
fn save_schema() -> Value {
    crate::command_schemas::empty_schema()
}
fn save_as_schema() -> Value {
    serde_json::json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","required":["location"],"properties":{
    "location":location_schema()},"additionalProperties":false})
}

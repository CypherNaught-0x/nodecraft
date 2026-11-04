//! File commands are platform requests. Session performs I/O; handlers never open dialogs.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_format::StorageLocation;
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileStatus {
    pub available: bool,
    /// Why this host cannot open/save, including how a deliberately closed session reopens.
    pub unavailable_reason: Option<&'static str>,
    pub location: Option<StorageLocation>,
    pub dirty: bool,
    pub recovery_required: bool,
    /// The source destination cannot be overwritten; local edits and Save As remain available.
    pub read_only: bool,
    /// A background save is still writing (#461); `dirty` stays set until it has published.
    pub saving: bool,
    /// The background save or open still running, if any (#367). Its phase moves while the
    /// host isn't looking: read it each frame rather than waiting for a status change.
    pub operation: Option<FileOperation>,
}

/// How a finished file operation ended: its kind, and the error it failed with (actionable, and
/// like every engine error free of private paths), `None` when it succeeded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileOutcome {
    pub kind: FileOperationKind,
    pub error: Option<Error>,
}

/// A background file operation's progress, as `file.status`-style readers see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileOperation {
    pub kind: FileOperationKind,
    pub phase: FilePhase,
    /// `file.cancel` asked it to stop. It may still finish: a cancel that arrives after the
    /// destination was published can't undo it, and the outcome says which happened.
    pub cancel_requested: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileOperationKind {
    Save,
    Open,
    /// Adopting work recovered from an earlier session's journal (#367): its media are copied
    /// into this session's workspace and its recovery checkpoint written.
    Recover,
}
/// Where a background file operation is. Phases only move forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePhase {
    /// Snapshotting the document and sealing its assets into the archive plan (save), or copying
    /// recovered media into this session's workspace and sealing them (recover).
    Preparing,
    /// Reading and verifying the project file into a private copy (open).
    Reading,
    /// Writing the destination; nothing is replaced until it is complete (save).
    Writing,
    /// Writing the recovery checkpoint. A save's file is already published by now, so it can no
    /// longer be cancelled. An open or a recovery still can, until its replacement checkpoint is
    /// published.
    Checkpointing,
}
impl FilePhase {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Reading => "reading",
            Self::Writing => "writing",
            Self::Checkpointing => "checkpointing",
        }
    }
}
impl FileOperationKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::Open => "open",
            Self::Recover => "recover",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    location: String,
    #[serde(default)]
    discard_dirty: bool,
    #[serde(default)]
    background: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveAs {
    location: String,
    #[serde(default)]
    background: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Save {
    #[serde(default)]
    background: bool,
}

fn available(c: &Context<'_>) -> Result<(), Error> {
    if c.file.available {
        Ok(())
    } else {
        Err(Error::new(ErrorKind::Unsupported, c.file.unavailable_reason.unwrap_or("Project storage is not configured for this session.")))
    }
}
fn open(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: Open = params(value)?;
    available(c)?;
    if c.file.dirty && !p.discard_dirty {
        return Err(Error::new(ErrorKind::Conflict, "save your changes or explicitly set discard_dirty before opening another project"));
    }
    Ok(Effect::Open { location: StorageLocation::from_command(p.location)?, discard_dirty: p.discard_dirty, background: p.background })
}
fn cancel(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    if !value.is_null() {
        params::<Empty>(value)?;
    }
    available(c)?;
    Ok(Effect::CancelFile)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
fn save(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let background = if value.is_null() { false } else { params::<Save>(value)?.background };
    available(c)?;
    if c.file.read_only {
        return Err(Error::new(ErrorKind::Conflict, "source is read-only; use Save As to a writable destination"));
    }
    if c.file.location.is_none() {
        return Err(Error::new(ErrorKind::Conflict, "untitled project needs file.save_as with an explicit location"));
    }
    nodecraft_format::validate_current_project(c.document)?;
    Ok(Effect::Save { location: None, background })
}
fn save_as(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: SaveAs = params(value)?;
    available(c)?;
    nodecraft_format::validate_current_project(c.document)?;
    Ok(Effect::Save { location: Some(StorageLocation::from_command(p.location)?), background: p.background })
}
fn enabled_open(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    open(c, v).map(|_| ())
}
/// `file.status`: what a client needs to follow a save or open it asked for (#367). Pure read;
/// it names no path (a client knows the location it gave, and the person's own file isn't the
/// agent's to learn), only whether the project has one.
fn status(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    if !value.is_null() {
        params::<Empty>(value)?;
    }
    let f = &c.file;
    let operation = f.operation.map(|o| serde_json::json!({"kind": o.kind.name(), "phase": o.phase.name(), "cancel_requested": o.cancel_requested}));
    let last = c.file_last.as_ref().map(|last| match &last.error {
        None => serde_json::json!({"kind": last.kind.name(), "ok": true}),
        Some(error) => serde_json::json!({"kind": last.kind.name(), "ok": false, "error": {"kind": error.kind, "message": error.message}}),
    });
    let result = serde_json::json!({
        "available": f.available,
        "dirty": f.dirty,
        "titled": f.location.is_some(),
        "read_only": f.read_only,
        "recovery_required": f.recovery_required,
        "operation": operation,
        "last": last,
    });
    Ok(Effect::None { result })
}
fn enabled_status(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    status(c, v).map(|_| ())
}
fn enabled_cancel(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    cancel(c, v).map(|_| ())
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
            params_doc: "{location: string, discard_dirty?: bool, background?: bool}; UTF8 filesystem path on native, storage key on web. Returns {opened: true, read_only: bool}. A legacy starter project receives a fresh identity in one undoable change, returns identity_migrated: {from, to}, and stays dirty until saved; its recovery checkpoint already has the new identity. Never opens a dialog. With background, a host that reads off its UI thread returns {opened: false, opening: true} at once; until it finishes, reads still run and edits are refused as busy.",
            enabled: enabled_open,
            run: open,
        },
        CommandSpec {
            id: "file.cancel",
            alternate_shortcuts: &[],
            input_schema: cancel_schema,
            label: "Cancel saving or opening",
            shortcut: None,
            params_doc: "{}; asks the background save, open or recovery to stop. Returns {cancelling: bool}, false when none is running. The operation's outcome says whether it stopped in time: a stopped save leaves the file unchanged, a stopped open or recovery keeps the current project.",
            enabled: enabled_cancel,
            run: cancel,
        },
        CommandSpec {
            id: "file.status",
            alternate_shortcuts: &[],
            input_schema: cancel_schema,
            label: "Project file status",
            shortcut: None,
            params_doc: "{}; read-only. Returns {available, dirty, titled, read_only, recovery_required: bool, operation: null or {kind: save|open|recover, phase, cancel_requested}, last: null or {kind, ok: bool, error?: {kind, message}}}: the save, open or recovery still running, and how the last one in this session ended. Names no path. A client whose save or open replied saving or opening at once reads here when it has finished and why it failed.",
            enabled: enabled_status,
            run: status,
        },
        CommandSpec {
            id: "file.save",
            alternate_shortcuts: &[],
            input_schema: save_schema,
            label: "Save project",
            shortcut: Some("Command+S"),
            params_doc: "{background?: bool}; save to the current location. Returns {saved: true, recovery_ready: bool, read_only: bool}. With background, a host that writes off its UI thread returns {saved: false, saving: true} at once and finishes the save before any later edit.",
            enabled: enabled_save,
            run: save,
        },
        CommandSpec {
            id: "file.save_as",
            alternate_shortcuts: &[],
            input_schema: save_as_schema,
            label: "Save project as",
            shortcut: Some("Command+Shift+S"),
            params_doc: "{location: string, background?: bool}; UTF8 filesystem path on native, storage key on web. Preserves the project's identity, so Save As copies share its project spending total and saved-job association. Returns {saved: true, recovery_ready: bool, read_only: bool}; background as for file.save.",
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
    "location":location_schema(),"discard_dirty":{"type":"boolean","default":false},"background":background_schema()},"additionalProperties":false})
}
fn cancel_schema() -> Value {
    serde_json::json!({"$schema":"https://json-schema.org/draft/2020-12/schema", "anyOf":[{"type":"null"},
        {"type":"object","properties":{},"additionalProperties":false}]})
}
fn background_schema() -> Value {
    serde_json::json!({"type":"boolean","default":false})
}
fn save_schema() -> Value {
    serde_json::json!({"$schema":"https://json-schema.org/draft/2020-12/schema", "anyOf":[{"type":"null"},
        {"type":"object","properties":{"background":background_schema()},"additionalProperties":false}]})
}
fn save_as_schema() -> Value {
    serde_json::json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","required":["location"],"properties":{
    "location":location_schema(),"background":background_schema()},"additionalProperties":false})
}

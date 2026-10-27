//! Versioned original starter projects (#952). Workflows are ordinary .ncraft data, with assets.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_format::Snapshot;
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};
pub mod catalog;
pub mod fetch;
mod validation;
pub use validation::{Entry, MAX_BYTES, MAX_HISTORY_BYTES, MAX_HISTORY_ENTRIES, MAX_INDEX_BYTES, parse_history, parse_index, validate_archive, validate_nodes};

/// The immutable offline snapshot compiled into both native and browser builds.
pub fn entries() -> Result<Vec<Entry>, Error> {
    let entries = parse_index(nodecraft_template_data::INDEX.as_bytes())?;
    if entries.len() != nodecraft_template_data::FILES.len() {
        return Err(invalid());
    }
    Ok(entries)
}
fn invalid() -> Error {
    Error::new(ErrorKind::InvalidInput, "The template index or graph is damaged or unsupported")
}
pub fn load(id: &str) -> Result<Snapshot, Error> {
    let entry = entries()?.into_iter().find(|entry| entry.id == id).ok_or_else(|| Error::new(ErrorKind::NotFound, "No bundled template has that id"))?;
    let bytes = nodecraft_template_data::FILES.iter().find(|(name, _)| *name == entry.file).map(|(_, bytes)| *bytes).ok_or_else(invalid)?;
    validate_archive(&entry, bytes)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Add {
    id: String,
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    blake3: Option<String>,
}
fn list(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    if !value.is_null() && !value.as_object().is_some_and(serde_json::Map::is_empty) {
        return Err(Error::new(ErrorKind::InvalidInput, "template.list takes no parameters"));
    }
    Ok(Effect::None { result: serde_json::to_value(catalog::listing(c.resources)?).map_err(|_| invalid())? })
}
/// An admitted immutable selection carried into the existing atomic import boundary.
pub struct PreparedTemplate {
    pub(crate) entry: Entry,
    pub(crate) snapshot: Snapshot,
}
fn add(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    c.editable()?;
    let p: Add = params(value)?;
    if p.id.len() > 64 {
        return Err(invalid());
    }
    if p.version == Some(0) || p.blake3.as_ref().is_some_and(|digest| digest.len() != 64) {
        return Err(invalid());
    }
    let template = catalog::resolve(c.resources, &p.id, p.version, p.blake3.as_deref())?;
    let snapshot = &template.snapshot;
    for canvas in snapshot.project.canvases().values() {
        if c.document.canvases().contains_key(&canvas.id()) {
            return Err(Error::new(ErrorKind::Conflict, "This template version is already in the project; open its canvas or duplicate its nodes"));
        }
    }
    validate_nodes(snapshot, c.kinds)?;
    Ok(Effect::TemplateAdd { template: Box::new(template) })
}
fn enabled_add(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: Add = params(value)?;
    c.editable()
}
fn enabled_list(_: &Context<'_>, _: &Value) -> Result<(), Error> {
    Ok(())
}
fn list_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","required":[],"properties":{},"additionalProperties":false})
}
fn add_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","required":["id"],"properties":{"id":{"type":"string","minLength":1,"maxLength":64},"version":{"type":"integer","minimum":1,"maximum":4294967295u64},"blake3":{"type":"string","minLength":64,"maxLength":64}},"additionalProperties":false})
}
pub fn specs() -> Vec<CommandSpec> {
    let mut specs = vec![
        CommandSpec {
            id: "template.list",
            label: "List starter templates",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{} -> {templates:[{id,title,description,version,file,blake3,features,canvas}],available:[{id,version,blake3}],stamp:u64}; lists bundled, indexed and cached versions with available identities and a catalog stamp, without network or library writes.",
            enabled: enabled_list,
            run: list,
        },
        CommandSpec {
            id: "template.add",
            label: "Add starter template",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: add_schema,
            params_doc: "{id:string,version?:u32,blake3?:string} -> {canvas,template,version}; pins an available template's canvas(es) and verified assets in one undoable edit, preserving current work. Each template version may be added once to a project; duplicate existing nodes for another copy. Never starts a run or retrieves credentials.",
            enabled: enabled_add,
            run: add,
        },
    ];
    specs.extend(fetch::specs());
    specs
}

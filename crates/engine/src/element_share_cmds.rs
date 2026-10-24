//! Sharing Elements (#765). `library.share_element` marks a library entry meant for others;
//! `library.export_element` writes one as an Element bundle (its settings and images, nothing
//! else) and `library.import_element` adds a bundle someone sent to this library. The commands
//! never open a dialog: frontends ask where first, then dispatch them with the path.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::element_library::Request;
use crate::element_library_cmds::{name, schema, shelf};
use nodecraft_nodes::element::valid_name;
use nodecraft_types::Error;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShareParams {
    name: String,
    shared: bool,
}

fn share(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ShareParams = params(value)?;
    shelf(c)?;
    Ok(Effect::Elements { request: Request::Share { name: p.name, shared: p.shared } })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportParams {
    name: String,
    path: PathBuf,
    /// Replace a file already at `path`; without it the export refuses to.
    #[serde(default)]
    overwrite: bool,
}

fn export(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ExportParams = params(value)?;
    shelf(c)?;
    crate::export::check(&p.path)?;
    Ok(Effect::Elements { request: Request::Export { name: p.name, path: p.path, overwrite: p.overwrite } })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportParams {
    path: PathBuf,
    /// The name it takes in this library, when the bundle's is taken here.
    #[serde(default, rename = "as")]
    rename: Option<String>,
    /// Replace the entry of that name; without it the import refuses to.
    #[serde(default)]
    replace: bool,
}

fn import(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ImportParams = params(value)?;
    shelf(c)?;
    crate::export::check(&p.path).map_err(|e| Error::new(e.kind, e.message.replace("the export path", "the bundle's path")))?;
    if p.rename.as_deref().is_some_and(|rename| !valid_name(rename)) {
        return Err(invalid("an Element's name takes 1 to 40 letters, digits, - or _"));
    }
    Ok(Effect::Elements { request: Request::Import { path: p.path, rename: p.rename, replace: p.replace } })
}

fn enabled_share(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    share(c, value).map(|_| ())
}
fn enabled_export(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    export(c, value).map(|_| ())
}
fn enabled_import(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    import(c, value).map(|_| ())
}

fn path() -> Json {
    json!({"type": "string", "minLength": 1, "maxLength": crate::export::MAX_PATH_BYTES})
}

fn share_schema() -> Json {
    schema(&["name", "shared"], json!({"name": name(), "shared": {"type": "boolean"}}))
}
fn export_schema() -> Json {
    schema(&["name", "path"], json!({"name": name(), "path": path(), "overwrite": {"type": "boolean"}}))
}
fn import_schema() -> Json {
    schema(&["path"], json!({"path": path(), "as": name(), "replace": {"type": "boolean"}}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "library.share_element",
            label: "Share library Element",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: share_schema,
            params_doc: "{name, shared: bool} -> {shared: {name, category, guidelines, images, sheet, shared}}; marks the library's entry meant for others, or private again.",
            enabled: enabled_share,
            run: share,
        },
        CommandSpec {
            id: "library.export_element",
            label: "Export library Element…",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: export_schema,
            params_doc: "{name, path: absolute file path (.ncelement), overwrite?: bool (default false)} -> {exported, bytes}; writes the library's entry as an Element bundle someone else can import: its name, category, guidelines and images, nothing else (no keys, accounts or history), marked private. Refuses a file that's already there unless `overwrite`, a missing folder, and a full disk, leaving nothing half written. Desktop only.",
            enabled: enabled_export,
            run: export,
        },
        CommandSpec {
            id: "library.import_element",
            label: "Import Element…",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: import_schema,
            params_doc: "{path: absolute path of an Element bundle, as?: name, replace?: bool} -> {imported, replaced}; adds the bundle's Element to the library, its images checked against their hashes, marked private. When the library already has an Element with its name, import it under another (as), or replace that one. Desktop only.",
            enabled: enabled_import,
            run: import,
        },
    ]
}

#[cfg(test)]
mod tests;

//! `folder.fill`: replaces the files a Folder card holds ([`nodecraft_nodes::folder`]). The desktop
//! fills one from a folder dropped on the canvas or picked with Import folder, after storing its
//! media through [`crate::import::import_folder`]; programmatic callers pass media already in the
//! project. Its filters, order and wires stay; another kind of file changes its ports, so a wired
//! card keeps its kind.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_graph::{Change, Changeset, Node};
use nodecraft_nodes::folder;
use nodecraft_types::{CanvasId, Error, ErrorKind, Modality, NodeId, Value as Wire};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    name: String,
    value: Wire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FillParams {
    canvas: CanvasId,
    id: NodeId,
    /// The folder's own name, for its card.
    folder: String,
    modality: Modality,
    files: Vec<File>,
}

fn schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "folder", "modality", "files"], "properties": {
        "canvas": {"type": "string"}, "id": {"type": "string"}, "folder": {"type": "string", "maxLength": 256},
        "modality": {"enum": ["image", "video", "audio", "text", "mesh", "document"]},
        "files": {"type": "array", "maxItems": folder::MAX_FILES, "items": {"type": "object", "additionalProperties": false, "required": ["name", "value"], "properties": {
            "name": {"type": "string"}, "value": {}
        }}}
    }})
}

fn folder_node<'a>(c: &'a Context<'_>, canvas: CanvasId, id: NodeId) -> Result<&'a Node, Error> {
    let node = c
        .document
        .canvases()
        .get(&canvas)
        .and_then(|canvas| canvas.nodes().get(&id))
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "document entity does not exist"))?;
    if node.kind != folder::ID {
        return Err(Error::new(ErrorKind::InvalidInput, "only a Folder card holds a folder's files"));
    }
    Ok(node)
}

fn enabled(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let p: FillParams = params(value)?;
    folder_node(c, p.canvas, p.id).map(|_| ())
}

fn fill(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: FillParams = params(value)?;
    let node = folder_node(c, p.canvas, p.id)?;
    let modality = p.modality;
    if !folder::MODALITIES.contains(&modality) {
        return Err(Error::new(ErrorKind::InvalidInput, "a folder holds image, video, audio, text, mesh or document files"));
    }
    let count = p.files.len();
    let files = folder::config(modality, &p.folder, p.files.into_iter().map(|file| (file.name, file.value)).collect())?;
    let mut changed = node.clone();
    // Other config keys are the kind's to keep (a later version's, or the cache setting).
    changed.config.extend(files);
    let descriptor = c.kinds.get(folder::ID)?.resolve_instance(&changed)?;
    if descriptor.outputs != node.outputs {
        let wired = c.document.canvases().get(&p.canvas).is_some_and(|canvas| canvas.edges().values().any(|e| e.from.node == p.id || e.to.node == p.id));
        if wired {
            return Err(Error::new(ErrorKind::Conflict, "the Folder card is wired for another kind of file; disconnect it or add a new Folder card"));
        }
        changed.outputs = descriptor.outputs.clone();
    }
    let result = json!({"files": count, "modality": modality});
    if changed == *node {
        return Ok(Effect::None { result });
    }
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::SetNode { canvas: p.canvas, node: changed }]), result, gesture: None })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "folder.fill",
        label: "Fill folder",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, id, folder, modality, files: [{name, value}]} -> {files, modality}. Replaces the files a Folder card (source.folder) holds, in import order: each file's path inside the folder (unique, /-separated) and its value, a media reference already in the project or text for a text folder. At most 1000 files, all of one modality. The card's filters, order and wires stay; a wired card can't change its modality. One undo step.",
        enabled,
        run: fill,
    }]
}

#[cfg(test)]
mod tests;

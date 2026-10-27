//! Versioned original starter projects (#952). Workflows are ordinary .ncraft data, with assets.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_format::Snapshot;
use nodecraft_types::{CanvasId, Error, ErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
const INDEX: &str = include_str!("../../../assets/templates/index.json");
const FILES: &[(&str, &[u8])] = &[
    ("generate-text", include_bytes!("../../../assets/templates/generate-text.ncraft")),
    ("generate-image", include_bytes!("../../../assets/templates/generate-image.ncraft")),
    ("generate-video", include_bytes!("../../../assets/templates/generate-video.ncraft")),
    ("generate-audio", include_bytes!("../../../assets/templates/generate-audio.ncraft")),
    ("generate-3d", include_bytes!("../../../assets/templates/generate-3d.ncraft")),
    ("cloud-local", include_bytes!("../../../assets/templates/cloud-local.ncraft")),
    ("documents", include_bytes!("../../../assets/templates/documents.ncraft")),
    ("document-prompt", include_bytes!("../../../assets/templates/document-prompt.ncraft")),
    ("combine", include_bytes!("../../../assets/templates/combine.ncraft")),
    ("branch", include_bytes!("../../../assets/templates/branch.ncraft")),
    ("prompt-helper", include_bytes!("../../../assets/templates/prompt-helper.ncraft")),
    ("csv-batch", include_bytes!("../../../assets/templates/csv-batch.ncraft")),
    ("prompt-list", include_bytes!("../../../assets/templates/prompt-list.ncraft")),
    ("batch-cross", include_bytes!("../../../assets/templates/batch-cross.ncraft")),
    ("batch-zip", include_bytes!("../../../assets/templates/batch-zip.ncraft")),
    ("technique", include_bytes!("../../../assets/templates/technique.ncraft")),
    ("published-technique", include_bytes!("../../../assets/templates/published-technique.ncraft")),
    ("app-form", include_bytes!("../../../assets/templates/app-form.ncraft")),
    ("router", include_bytes!("../../../assets/templates/router.ncraft")),
    ("comfy-core", include_bytes!("../../../assets/templates/comfy-core.ncraft")),
    ("action-image-blur", include_bytes!("../../../assets/templates/action-image-blur.ncraft")),
    ("action-image-grade", include_bytes!("../../../assets/templates/action-image-grade.ncraft")),
    ("action-image-filter", include_bytes!("../../../assets/templates/action-image-filter.ncraft")),
    ("action-image-tint", include_bytes!("../../../assets/templates/action-image-tint.ncraft")),
    ("action-image-flip", include_bytes!("../../../assets/templates/action-image-flip.ncraft")),
    ("action-image-rotate", include_bytes!("../../../assets/templates/action-image-rotate.ncraft")),
    ("action-image-crop", include_bytes!("../../../assets/templates/action-image-crop.ncraft")),
    ("action-image-key", include_bytes!("../../../assets/templates/action-image-key.ncraft")),
    ("action-image-replace_color", include_bytes!("../../../assets/templates/action-image-replace_color.ncraft")),
    ("action-image-aspect", include_bytes!("../../../assets/templates/action-image-aspect.ncraft")),
    ("action-image-overlay", include_bytes!("../../../assets/templates/action-image-overlay.ncraft")),
    ("action-image-duplicate", include_bytes!("../../../assets/templates/action-image-duplicate.ncraft")),
    ("action-image-resize", include_bytes!("../../../assets/templates/action-image-resize.ncraft")),
    ("action-image-expand", include_bytes!("../../../assets/templates/action-image-expand.ncraft")),
    ("action-text-split", include_bytes!("../../../assets/templates/action-text-split.ncraft")),
    ("action-text-replace", include_bytes!("../../../assets/templates/action-text-replace.ncraft")),
    ("action-text-template", include_bytes!("../../../assets/templates/action-text-template.ncraft")),
    ("action-text-concat", include_bytes!("../../../assets/templates/action-text-concat.ncraft")),
    ("layout", include_bytes!("../../../assets/templates/layout.ncraft")),
    ("mesh-convert", include_bytes!("../../../assets/templates/mesh-convert.ncraft")),
    ("elements", include_bytes!("../../../assets/templates/elements.ncraft")),
    ("storyboard", include_bytes!("../../../assets/templates/storyboard.ncraft")),
    ("generate-document", include_bytes!("../../../assets/templates/generate-document.ncraft")),
    ("action-image-mask", include_bytes!("../../../assets/templates/action-image-mask.ncraft")),
    ("action-image-shader", include_bytes!("../../../assets/templates/action-image-shader.ncraft")),
    ("action-mesh-photo", include_bytes!("../../../assets/templates/action-mesh-photo.ncraft")),
    ("action-mesh-views", include_bytes!("../../../assets/templates/action-mesh-views.ncraft")),
    ("action-mesh-turntable", include_bytes!("../../../assets/templates/action-mesh-turntable.ncraft")),
    ("action-video-trim", include_bytes!("../../../assets/templates/action-video-trim.ncraft")),
    ("action-video-speed", include_bytes!("../../../assets/templates/action-video-speed.ncraft")),
    ("action-video-stitch", include_bytes!("../../../assets/templates/action-video-stitch.ncraft")),
    ("action-video-replace_audio", include_bytes!("../../../assets/templates/action-video-replace_audio.ncraft")),
    ("action-video-frames", include_bytes!("../../../assets/templates/action-video-frames.ncraft")),
    ("action-video-contact_sheet", include_bytes!("../../../assets/templates/action-video-contact_sheet.ncraft")),
    ("action-video-long_exposure", include_bytes!("../../../assets/templates/action-video-long_exposure.ncraft")),
    ("action-video-reverse", include_bytes!("../../../assets/templates/action-video-reverse.ncraft")),
    ("action-video-ken_burns", include_bytes!("../../../assets/templates/action-video-ken_burns.ncraft")),
    ("action-video-grade", include_bytes!("../../../assets/templates/action-video-grade.ncraft")),
    ("action-video-filter", include_bytes!("../../../assets/templates/action-video-filter.ncraft")),
    ("action-video-key", include_bytes!("../../../assets/templates/action-video-key.ncraft")),
    ("action-video-watermark", include_bytes!("../../../assets/templates/action-video-watermark.ncraft")),
    ("action-video-duplicate", include_bytes!("../../../assets/templates/action-video-duplicate.ncraft")),
    ("action-video-split", include_bytes!("../../../assets/templates/action-video-split.ncraft")),
    ("action-video-extract_audio", include_bytes!("../../../assets/templates/action-video-extract_audio.ncraft")),
    ("timeline", include_bytes!("../../../assets/templates/timeline.ncraft")),
    ("train-style", include_bytes!("../../../assets/templates/train-style.ncraft")),
    ("folder", include_bytes!("../../../assets/templates/folder.ncraft")),
    ("edit-image", include_bytes!("../../../assets/templates/edit-image.ncraft")),
    ("image-to-video", include_bytes!("../../../assets/templates/image-to-video.ncraft")),
    ("image-to-3d", include_bytes!("../../../assets/templates/image-to-3d.ncraft")),
    ("transcribe-audio", include_bytes!("../../../assets/templates/transcribe-audio.ncraft")),
    ("generation-sheet", include_bytes!("../../../assets/templates/generation-sheet.ncraft")),
];
const MAX_BYTES: usize = 2 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub description: String,
    pub version: u32,
    pub file: String,
    pub blake3: String,
    pub features: Vec<String>,
    pub canvas: CanvasId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    version: u32,
    templates: Vec<Entry>,
}
fn invalid() -> Error {
    Error::new(ErrorKind::InvalidInput, "The shipped template index or graph is damaged")
}
pub fn entries() -> Result<Vec<Entry>, Error> {
    if INDEX.len() > 128 * 1024 {
        return Err(invalid());
    }
    let index: Index = serde_json::from_str(INDEX).map_err(|_| invalid())?;
    if index.version != 1 || index.templates.len() != FILES.len() || index.templates.len() > 128 {
        return Err(invalid());
    }
    let mut ids = BTreeSet::new();
    for entry in &index.templates {
        if entry.version != 1
            || entry.id.is_empty()
            || entry.id.len() > 64
            || !entry.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
            || !ids.insert(entry.id.as_str())
            || entry.file != format!("{}.ncraft", entry.id)
            || entry.title.is_empty()
            || entry.title.len() > 128
            || entry.description.is_empty()
            || entry.description.len() > 512
            || entry.features.is_empty()
            || entry.features.len() > 16
            || entry.features.iter().any(|f| f.is_empty() || f.len() > 64)
            || entry.blake3.len() != 64
            || !entry.blake3.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid());
        }
    }
    Ok(index.templates)
}
pub fn load(id: &str) -> Result<Snapshot, Error> {
    let entry = entries()?.into_iter().find(|entry| entry.id == id).ok_or_else(|| Error::new(ErrorKind::NotFound, "No shipped template has that id"))?;
    let bytes = FILES.iter().find(|(name, _)| *name == id).map(|(_, bytes)| *bytes).ok_or_else(invalid)?;
    if bytes.len() > MAX_BYTES || blake3::hash(bytes).to_hex().as_str() != entry.blake3 {
        return Err(invalid());
    }
    let snapshot = nodecraft_format::decode(bytes)?;
    if !snapshot.project.canvases().contains_key(&entry.canvas) {
        return Err(invalid());
    }
    Ok(snapshot)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Add {
    id: String,
}
fn list(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    if !value.is_null() && !value.as_object().is_some_and(serde_json::Map::is_empty) {
        return Err(Error::new(ErrorKind::InvalidInput, "template.list takes no parameters"));
    }
    Ok(Effect::None { result: json!({"templates":entries()?}) })
}
fn add(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    c.editable()?;
    let p: Add = params(value)?;
    if p.id.len() > 64 {
        return Err(invalid());
    }
    let snapshot = load(&p.id)?;
    for canvas in snapshot.project.canvases().values() {
        if c.document.canvases().contains_key(&canvas.id()) {
            return Err(Error::new(ErrorKind::Conflict, "This template version is already in the project; open its canvas or duplicate its nodes"));
        }
        for node in canvas.nodes().values() {
            c.kinds.get(&node.kind)?.validate_instance(node, false)?;
        }
    }
    Ok(Effect::TemplateAdd { id: p.id })
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
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","required":["id"],"properties":{"id":{"type":"string","minLength":1,"maxLength":64}},"additionalProperties":false})
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "template.list",
            label: "List starter templates",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{} -> {templates:[{id,title,description,version,file,blake3,features,canvas}]}; lists shipped starter projects without network or library writes.",
            enabled: enabled_list,
            run: list,
        },
        CommandSpec {
            id: "template.add",
            label: "Add starter template",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: add_schema,
            params_doc: "{id:string} -> {canvas,template,version}; adds a shipped template's canvas(es) and verified assets in one undoable edit, preserving current work. Each template version may be added once to a project; duplicate existing nodes for another copy. Never starts a run or retrieves credentials.",
            enabled: enabled_add,
            run: add,
        },
    ]
}

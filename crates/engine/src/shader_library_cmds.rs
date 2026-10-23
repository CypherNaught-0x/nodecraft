//! The shader library's commands (#90; `docs/decisions/2026-10-23-shader-library.md`).
//! `shader.save` keeps a custom shader card's shader as the next version of a name in the library
//! every project shares; `shader.place` puts a version on a canvas as a new card and
//! `shader.update` gives a card a version, both recording which version the card came from.
//! `library.shaders` lists the library and `library.remove_shader` tidies it. Saving, listing and
//! removing ask the host's `shaders.store` service ([`crate::shader_library`]); placing and
//! updating read the library as the service last read it and edit the document only.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::element_cmds::{canvas, check_pos, pos, uuid};
use crate::shader_cmds::{set_shader, spec};
use crate::shader_library::{MAX_NAME_CHARS, SHADERS_SERVICE, ShaderShelf, valid_name};
use nodecraft_graph::{Change, Changeset, Position};
use nodecraft_nodes::shader::{CONFIG, FROM_CONFIG, ID, Pin, StoredShader, config_value, pin_value, pinned, stored};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ParamValues};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

fn service(request: Json) -> Effect {
    Effect::Service { capability: SHADERS_SERVICE, request }
}

fn shelf<'a>(c: &'a Context<'_>) -> Result<&'a ShaderShelf, Error> {
    c.resources.get::<ShaderShelf>().ok_or_else(|| Error::new(ErrorKind::Unsupported, "the shader library isn't available here"))
}

fn named(name: &str) -> Result<(), Error> {
    if valid_name(name) { Ok(()) } else { Err(invalid("a shader's name is 1 to 40 characters on one line")) }
}

/// The custom shader card `id` on `canvas`.
fn card<'a>(c: &'a Context<'_>, canvas_id: CanvasId, id: NodeId) -> Result<&'a nodecraft_graph::Node, Error> {
    let node = canvas(c, canvas_id)?.nodes().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "document entity does not exist"))?;
    if node.kind != ID {
        return Err(invalid("only a custom shader card has a library shader"));
    }
    Ok(node)
}

/// Version `version` of `name` (the latest when `None`), as the library was last read.
fn kept(c: &Context<'_>, name: &str, version: Option<u32>) -> Result<(Pin, StoredShader), Error> {
    shelf(c)?.with(|library| {
        let entry = library.get(name).ok_or_else(|| Error::new(ErrorKind::NotFound, "the library has no shader with that name"))?;
        let kept = match version {
            Some(version) => entry.get(version).ok_or_else(|| Error::new(ErrorKind::NotFound, "the library has no such version of that shader"))?,
            None => entry.latest().ok_or_else(|| Error::new(ErrorKind::NotFound, "the library has no shader with that name"))?,
        };
        Ok((Pin { name: entry.name.clone(), version: kept.version }, kept.shader.clone()))
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveParams {
    canvas: CanvasId,
    /// The custom shader card.
    id: NodeId,
    /// The library name; the one the card came from when absent.
    #[serde(default)]
    name: Option<String>,
}

fn save(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: SaveParams = params(value)?;
    let node = card(c, p.canvas, p.id)?;
    let name = p.name.or_else(|| pinned(node).map(|pin| pin.name)).ok_or_else(|| invalid("name the shader to save it"))?;
    named(&name)?;
    let shader = stored(node)?;
    nodecraft_gpu::shader::validate(&spec(&shader)).map_err(|_| invalid("the card's shader isn't valid; fix it before saving"))?;
    Ok(service(json!({"op": "save", "name": name, "shader": shader})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceParams {
    canvas: CanvasId,
    name: String,
    /// The latest when absent.
    #[serde(default)]
    version: Option<u32>,
    /// The new card's id.
    id: NodeId,
    pos: Position,
}

fn place(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: PlaceParams = params(value)?;
    check_pos(p.pos)?;
    canvas(c, p.canvas)?;
    let (pin, shader) = kept(c, &p.name, p.version)?;
    // A file edited elsewhere is checked like any shader a card is given.
    nodecraft_gpu::shader::validate(&spec(&shader)).map_err(|_| invalid("that library shader isn't valid in this version of NodeCraft"))?;
    let config: BTreeMap<String, nodecraft_types::Value> = [(CONFIG.to_owned(), config_value(&shader)?), (FROM_CONFIG.to_owned(), pin_value(&pin)?)].into();
    let mut node = c.kinds.get(ID)?.instantiate_instance(p.id, config, &ParamValues::new())?;
    node.pos = p.pos;
    node.title = pin.name.clone();
    let changes = Changeset::new(vec![Change::AddNode { canvas: p.canvas, node }]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.id, "name": pin.name, "version": pin.version}), gesture: None })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateParams {
    canvas: CanvasId,
    /// The custom shader card.
    id: NodeId,
    /// The one the card came from when absent.
    #[serde(default)]
    name: Option<String>,
    /// The latest when absent.
    #[serde(default)]
    version: Option<u32>,
}

fn update(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: UpdateParams = params(value)?;
    let node = card(c, p.canvas, p.id)?;
    let name = p.name.or_else(|| pinned(node).map(|pin| pin.name)).ok_or_else(|| invalid("this card didn't come from the library; name a shader"))?;
    let (pin, shader) = kept(c, &name, p.version)?;
    let result = json!({"name": pin.name, "version": pin.version});
    match set_shader(c, p.canvas, p.id, shader, Some(&pin))? {
        Effect::Edit { changes, gesture, .. } => Ok(Effect::Edit { changes, result: json!({"changed": true, "from": result}), gesture }),
        _ => Ok(Effect::None { result: json!({"changed": false, "from": result}) }),
    }
}

fn list(_: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    if !value.as_object().is_some_and(serde_json::Map::is_empty) && !value.is_null() {
        return Err(invalid("library.shaders takes no parameters"));
    }
    Ok(service(json!({"op": "list"})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveParams {
    name: String,
    /// The whole shader when absent.
    #[serde(default)]
    version: Option<u32>,
}

fn remove(_: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: RemoveParams = params(value)?;
    named(&p.name)?;
    Ok(service(json!({"op": "remove", "name": p.name, "version": p.version})))
}

// ---- registration ----

/// Available where the host keeps a shader library.
fn available(c: &Context<'_>) -> Result<(), Error> {
    if c.services.contains(SHADERS_SERVICE) { Ok(()) } else { Err(Error::new(ErrorKind::Unsupported, "the shader library isn't available here")) }
}

fn enabled(c: &Context<'_>, value: &Json, run: fn(&Context<'_>, &Json) -> Result<Effect, Error>) -> Result<(), Error> {
    available(c)?;
    run(c, value).map(|_| ())
}

fn enabled_edit(c: &Context<'_>, value: &Json, run: fn(&Context<'_>, &Json) -> Result<Effect, Error>) -> Result<(), Error> {
    enabled(c, value, run)?;
    c.editable()
}

fn name() -> Json {
    json!({"type": "string", "minLength": 1, "maxLength": MAX_NAME_CHARS})
}
fn number() -> Json {
    json!({"type": "integer", "minimum": 1, "maximum": u32::MAX})
}
fn schema(required: &[&str], properties: Json) -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

macro_rules! spec {
    ($id:literal, $label:literal, $schema:expr, $doc:literal, $run:ident, $enabled:ident) => {
        CommandSpec {
            id: $id,
            label: $label,
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || $schema,
            params_doc: $doc,
            enabled: |c, v| $enabled(c, v, $run),
            run: $run,
        }
    };
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "shader.save",
            "Save shader to library",
            schema(&["canvas", "id"], json!({"canvas": uuid(), "id": uuid(), "name": name()})),
            "{canvas, id: custom shader NodeId, name?} -> {name, version, saved}; keeps the card's shader (its WGSL and declared parameters, nothing else) as the next version of `name` in the shader library every project shares; without a name, the one the card came from. `saved` is false when that name's latest version is this shader already. Then shader.update {canvas, id, name, version} records the version on the card.",
            save,
            enabled
        ),
        spec!(
            "shader.place",
            "Place shader from library",
            schema(&["canvas", "name", "id", "pos"], json!({"canvas": uuid(), "name": name(), "version": number(), "id": uuid(), "pos": pos()})),
            "{canvas, name, version?, id: new NodeId, pos: {x, y}} -> {id, name, version}; a new custom shader card with that library version (the latest when absent), titled with its name, as one undo step. Reads the library as last listed (library.shaders reads it afresh).",
            place,
            enabled_edit
        ),
        spec!(
            "shader.update",
            "Update shader from library",
            schema(&["canvas", "id"], json!({"canvas": uuid(), "id": uuid(), "name": name(), "version": number()})),
            "{canvas, id: custom shader NodeId, name?, version?} -> {changed, from: {name, version}}; gives the card that library version (by default the latest of the shader it came from) and records it as where the card's shader came from, as one undo step. Values for parameters the version still declares are kept. Reads the library as last listed.",
            update,
            enabled_edit
        ),
        spec!(
            "library.shaders",
            "List library shaders",
            schema(&[], json!({})),
            "{} -> {shaders: [{name, versions: [number], latest}]}; the shader library every project shares, read afresh.",
            list,
            enabled
        ),
        spec!(
            "library.remove_shader",
            "Remove shader from library",
            schema(&["name"], json!({"name": name(), "version": number()})),
            "{name, version?} -> {removed}; removes that version of the shader from the library, or the whole shader without a version. Cards placed from it keep their shader.",
            remove,
            enabled
        ),
    ]
}

#[cfg(test)]
mod tests;

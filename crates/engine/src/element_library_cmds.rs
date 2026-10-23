//! The Elements library's commands (#552). `element.save` keeps a canvas's Element in the library
//! (its settings and images, never anything else), `element.place` puts one on a canvas as image
//! sources wired into a new Element, and `element.update` brings a placed Element up to date
//! with the library's entry of its name. `library.elements` lists the entries;
//! `library.remove_element` and `library.rename_element` tidy them. Placing and updating read
//! the library as last read (as `library.place` does): listing reads it afresh, so a change made
//! in another window shows once the library is listed.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::element_cmds::{canvas, check_pos, image_wires, pos, taken, text, uuid};
use crate::element_library::{ElementShelf, Request, Saved};
use crate::history_cmds::source_kind;
use crate::technique_cmds::derive;
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Node, Position};
use nodecraft_nodes::element::{CATEGORY, GUIDELINES, ID, IMAGES, MAX_NAME, NAME, SHEET, valid_name};
use nodecraft_types::{AssetRef, Blake3, CanvasId, EdgeId, Error, ErrorKind, Modality, NodeId, ParamValues, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// How far left of an Element its image sources go, and how far apart they stack.
const SOURCE_DX: f32 = 360.0;
const SOURCE_DY: f32 = 280.0;

/// The library entry an Element was placed from (#765), kept in its config so it still updates
/// from that entry once it is renamed on the canvas.
pub const ENTRY_CONFIG: &str = "nodecraft.library_entry";

fn not_found(what: &str) -> Error {
    Error::new(ErrorKind::NotFound, format!("{what} does not exist"))
}

pub(crate) fn shelf<'a>(c: &'a Context<'_>) -> Result<&'a ElementShelf, Error> {
    c.resources.get::<ElementShelf>().ok_or_else(|| Error::new(ErrorKind::Unsupported, "this host has no Elements library"))
}

fn element(canvas: &Canvas, id: NodeId) -> Result<&Node, Error> {
    let node = canvas.nodes().get(&id).ok_or_else(|| not_found("document entity"))?;
    if node.kind != ID {
        return Err(invalid("that node isn't an Element"));
    }
    Ok(node)
}

/// The image a wire into an Element carries now: a source's image, or a card's chosen result.
fn wired_image(canvas: &Canvas, edge: &Edge) -> Option<AssetRef> {
    canvas.nodes().get(&edge.from.node).and_then(crate::split::image_of).cloned()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveParams {
    canvas: CanvasId,
    /// The Element.
    id: NodeId,
    #[serde(default)]
    shared: bool,
    /// Over the library's entry of that name.
    #[serde(default)]
    replace: bool,
}

fn save(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: SaveParams = params(value)?;
    shelf(c)?;
    let canvas = canvas(c, p.canvas)?;
    let node = element(canvas, p.id)?;
    let mut images = Vec::new();
    for wire in image_wires(canvas, p.id) {
        images.push(wired_image(canvas, wire).ok_or_else(|| invalid("run or load the Element's images first"))?);
    }
    let sheet = canvas.edges().values().find(|e| e.to.node == p.id && e.to.port == SHEET).and_then(|wire| wired_image(canvas, wire));
    let entry = Saved { name: text(node, NAME), category: text(node, CATEGORY), guidelines: text(node, GUIDELINES), images, sheet, shared: p.shared };
    let entry = entry.checked()?;
    for asset in entry.media() {
        if !c.assets.contains(&asset.hash) {
            return Err(Error::new(ErrorKind::NotFound, "an image of the Element is missing from the project"));
        }
    }
    Ok(Effect::Elements { request: Request::Save { entry, replace: p.replace } })
}

/// New image sources showing `entry`'s images (and sheet) left of an Element at `at`, wired into
/// `element` in order, with ids derived from `seed`. Returns the changes and the new sources'
/// ids, the sheet's last.
fn sources(c: &Context<'_>, canvas: CanvasId, element: NodeId, seed: NodeId, at: Position, entry: &Saved) -> Result<(Vec<Change>, Vec<NodeId>), Error> {
    let (kind, parameter) = source_kind(Modality::Image)?;
    let kind = c.kinds.get(kind)?;
    let wired = entry.images.iter().map(|asset| (asset, IMAGES)).chain(entry.sheet.iter().map(|asset| (asset, SHEET)));
    let (mut changes, mut ids) = (Vec::new(), Vec::new());
    let mut orders: BTreeMap<&str, u32> = BTreeMap::new();
    for (n, (asset, port)) in wired.enumerate() {
        let id = NodeId::from_bytes(derive(seed, "element-source", n));
        let supplied: ParamValues = [(parameter.to_owned(), Value::Media(asset.clone()))].into_iter().collect();
        let mut source = kind.instantiate_instance(id, BTreeMap::new(), &supplied)?;
        let row = u16::try_from(n).map_err(|_| invalid("too many images"))?;
        source.pos = Position { x: at.x - SOURCE_DX, y: at.y + SOURCE_DY * f32::from(row) };
        check_pos(source.pos)?;
        let order = orders.entry(port).or_default();
        let edge = Edge {
            id: EdgeId::from_bytes(derive(seed, "element-wire", n)),
            from: Endpoint { node: id, port: "out".into() },
            to: Endpoint { node: element, port: port.into() },
            order: *order,
        };
        *order = order.saturating_add(1);
        changes.push(Change::AddNode { canvas, node: source });
        changes.push(Change::Connect { canvas, edge });
        ids.push(id);
    }
    Ok((changes, ids))
}

fn media(entry: &Saved) -> Vec<Blake3> {
    entry.media().map(|asset| asset.hash).collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceParams {
    canvas: CanvasId,
    /// The library entry.
    name: String,
    /// The new Element's id; its image sources' and wires' ids derive from it.
    id: NodeId,
    pos: Position,
    /// The name it takes on this canvas, when the entry's is taken here.
    #[serde(default, rename = "as")]
    rename: Option<String>,
}

fn place(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: PlaceParams = params(value)?;
    check_pos(p.pos)?;
    let canvas = canvas(c, p.canvas)?;
    let mut entry = shelf(c)?
        .with(|elements| elements.get(&p.name).cloned().ok_or_else(|| Error::new(ErrorKind::NotFound, "the library has no Element with that name")))?;
    let config = BTreeMap::from([(ENTRY_CONFIG.to_owned(), Value::Text(entry.name.clone()))]);
    if let Some(rename) = p.rename {
        if !valid_name(&rename) {
            return Err(invalid("an Element's name takes 1 to 40 letters, digits, - or _"));
        }
        entry.name = rename;
    }
    if taken(canvas, &entry.name, None) {
        return Err(Error::new(
            ErrorKind::Conflict,
            "an Element with that name is already on this canvas: place this one under another name (as), or update that one",
        ));
    }
    let mut supplied = ParamValues::new();
    supplied.insert(NAME.into(), Value::Text(entry.name.clone()));
    supplied.insert(CATEGORY.into(), Value::Text(entry.category.clone()));
    supplied.insert(GUIDELINES.into(), Value::Text(entry.guidelines.clone()));
    let mut node = c.kinds.get(ID)?.instantiate_instance(p.id, config, &supplied)?;
    node.pos = p.pos;
    let (wiring, ids) = sources(c, p.canvas, p.id, p.id, p.pos, &entry)?;
    let mut changes = vec![Change::AddNode { canvas: p.canvas, node }];
    changes.extend(wiring);
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Place { media: media(&entry), changes, result: json!({"id": p.id, "name": entry.name, "sources": ids}) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateParams {
    canvas: CanvasId,
    /// The placed Element.
    id: NodeId,
    /// Seeds the new image sources' and wires' ids; a fresh id each update.
    seed: NodeId,
}

fn update(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: UpdateParams = params(value)?;
    let canvas = canvas(c, p.canvas)?;
    let node = element(canvas, p.id)?;
    // The entry it was placed from; one saved before that was kept goes by its name.
    let entry = match node.config.get(ENTRY_CONFIG) {
        Some(Value::Text(from)) => shelf(c)?.with(|elements| {
            elements
                .get(from)
                .cloned()
                .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("the library no longer has {from}, the Element this one was placed from")))
        })?,
        _ => {
            let name = text(node, NAME);
            shelf(c)?.with(|elements| {
                elements.get(&name).cloned().ok_or_else(|| Error::new(ErrorKind::NotFound, "the library has no Element with this one's name"))
            })?
        }
    };
    let mut changes = Vec::new();
    // What feeds it now goes: an image source that feeds only this Element is removed (with its
    // wires), anything else is just unwired.
    let feeding: Vec<&Edge> = canvas.edges().values().filter(|e| e.to.node == p.id && (e.to.port == IMAGES || e.to.port == SHEET)).collect();
    let mut removed = Vec::new();
    for wire in &feeding {
        let from = wire.from.node;
        let only_here =
            canvas.nodes().get(&from).is_some_and(|n| n.kind == "source.image") && canvas.edges().values().all(|e| e.from.node != from || e.to.node == p.id);
        if only_here {
            if !removed.contains(&from) {
                removed.push(from);
                changes.push(Change::RemoveNode { canvas: p.canvas, id: from });
            }
        } else {
            changes.push(Change::Disconnect { canvas: p.canvas, id: wire.id });
        }
    }
    for (param, value) in [(CATEGORY, &entry.category), (GUIDELINES, &entry.guidelines)] {
        if text(node, param) != *value {
            changes.push(Change::SetParam { canvas: p.canvas, id: p.id, name: param.into(), value: Some(Value::Text(value.clone())) });
        }
    }
    let (wiring, ids) = sources(c, p.canvas, p.id, p.seed, node.pos, &entry)?;
    changes.extend(wiring);
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Place { media: media(&entry), changes, result: json!({"id": p.id, "removed": removed, "sources": ids}) })
}

fn list(c: &Context<'_>, _: &Json) -> Result<Effect, Error> {
    shelf(c)?;
    Ok(Effect::Elements { request: Request::List })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveParams {
    name: String,
}

fn remove(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: RemoveParams = params(value)?;
    shelf(c)?;
    Ok(Effect::Elements { request: Request::Remove { name: p.name } })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameParams {
    name: String,
    to: String,
}

fn rename(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: RenameParams = params(value)?;
    shelf(c)?;
    if !valid_name(&p.to) {
        return Err(invalid("an Element's name takes 1 to 40 letters, digits, - or _"));
    }
    Ok(Effect::Elements { request: Request::Rename { name: p.name, to: p.to } })
}

fn enabled_save(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    save(c, value).map(|_| ())
}
fn enabled_place(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    place(c, value).map(|_| ())
}
fn enabled_update(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    update(c, value).map(|_| ())
}
fn enabled_library(c: &Context<'_>, _: &Json) -> Result<(), Error> {
    shelf(c).map(|_| ())
}

pub(crate) fn name() -> Json {
    json!({"type": "string", "minLength": 1, "maxLength": MAX_NAME, "pattern": "^[A-Za-z0-9_-]+$"})
}

pub(crate) fn schema(required: &[&str], properties: Json) -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

fn save_schema() -> Json {
    schema(&["canvas", "id"], json!({"canvas": uuid(), "id": uuid(), "shared": {"type": "boolean"}, "replace": {"type": "boolean"}}))
}
fn place_schema() -> Json {
    schema(&["canvas", "name", "id", "pos"], json!({"canvas": uuid(), "name": name(), "id": uuid(), "pos": pos(), "as": name()}))
}
fn update_schema() -> Json {
    schema(&["canvas", "id", "seed"], json!({"canvas": uuid(), "id": uuid(), "seed": uuid()}))
}
fn list_schema() -> Json {
    schema(&[], json!({}))
}
fn remove_schema() -> Json {
    schema(&["name"], json!({"name": name()}))
}
fn rename_schema() -> Json {
    schema(&["name", "to"], json!({"name": name(), "to": name()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "element.save",
            label: "Save Element to library",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: save_schema,
            params_doc: "{canvas, id: Element NodeId, shared?: bool, replace?: bool} -> {saved: {name, category, guidelines, images, sheet, shared}, replaced}; keeps the Element in the Elements library outside any project: its name, category, guidelines, the images wired into it now and its reference sheet, if it has one. Nothing else is saved (no keys, accounts or history). Saving over an entry of the same name needs replace. Shared marks it meant for others; entries are private by default.",
            enabled: enabled_save,
            run: save,
        },
        CommandSpec {
            id: "element.place",
            label: "Place Element from library",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: place_schema,
            params_doc: "{canvas, name: library entry, id: new NodeId, pos: {x, y}, as?: name} -> {id, name, sources: [NodeId]}; puts the library's Element on the canvas as one undo step: a new Element with its settings, fed by new image sources holding its images (and its reference sheet), whose ids derive from id. Its images are copied into the project. Reads the library as last listed (library.elements reads it afresh). When an Element on the canvas already has that name, place it under another (as), or update that one (element.update).",
            enabled: enabled_place,
            run: place,
        },
        CommandSpec {
            id: "element.update",
            label: "Update Element from library",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: update_schema,
            params_doc: "{canvas, id: Element NodeId, seed: new NodeId} -> {id, removed: [NodeId], sources: [NodeId]}; brings the Element up to date with the library's entry it was placed from (whatever it is called on the canvas now), or, for one never placed, the entry of its name, as one undo step: image sources feeding only it are removed, anything else feeding it is unwired, new sources with the entry's images are wired in (ids derived from seed), and its category and guidelines are set to the entry's. Reads the library as last listed (library.elements reads it afresh).",
            enabled: enabled_update,
            run: update,
        },
        CommandSpec {
            id: "library.elements",
            label: "List library Elements",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{} -> {elements: [{name, category, guidelines, images, sheet, shared}], damaged?: [{index, name, reason}], file?}; the Elements library, read afresh. Entries this build can't use (written by a newer one, or edited by hand) are listed under damaged with why, and kept in the file as they are.",
            enabled: enabled_library,
            run: list,
        },
        CommandSpec {
            id: "library.remove_element",
            label: "Remove Element from library",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: remove_schema,
            params_doc: "{name} -> {removed}; removes the entry from the Elements library. Elements placed from it stay in their projects.",
            enabled: enabled_library,
            run: remove,
        },
        CommandSpec {
            id: "library.rename_element",
            label: "Rename library Element",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: rename_schema,
            params_doc: "{name, to} -> {renamed}; renames the library's entry. Elements placed from it keep their names.",
            enabled: enabled_library,
            run: rename,
        },
    ]
}

#[cfg(test)]
mod tests;

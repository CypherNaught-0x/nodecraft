//! My library's commands (#96). `library.saved` lists the files a person saved outside any
//! project; `library.save_assets` saves files the open project holds into it, and
//! `library.unsave_assets` takes entries out (only a person runs those two: the library is
//! shared by every project and outside undo); `library.place_saved` puts entries on a canvas as
//! source cards, copying their bytes into the project through the import path, as one undo step.
use crate::asset_library::{AssetShelf, MAX_CHANGE, Request, Saving, entry_name};
use crate::commands::{CommandSpec, Context, Effect, PlaceFrom, invalid, params};
use crate::history_cmds::source_kind;
use crate::library_attach_cmds::{DEFAULT_STEP, MAX_COORDINATE, MAX_ITEMS, MAX_STEP, columns, within};
use crate::library_browse_cmds::{ASSET_KINDS, locate_all, object, page, paging, query, short_label};
use nodecraft_graph::{Change, Changeset, Position};
use nodecraft_types::{Blake3, CanvasId, Error, ErrorKind, Modality, NodeId, ParamValues, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, HashSet};

pub(crate) fn shelf<'a>(c: &'a Context<'_>) -> Result<&'a AssetShelf, Error> {
    c.resources.get::<AssetShelf>().ok_or_else(|| Error::new(ErrorKind::Unsupported, "this host has no My library; save files from the desktop app"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {
    #[serde(default)]
    kind: Option<Modality>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}

fn list_request(c: &Context<'_>, value: &Json) -> Result<Request, Error> {
    let mut p: ListParams = params(value)?;
    shelf(c)?;
    if p.kind.is_some_and(|kind| !ASSET_KINDS.contains(&kind)) {
        return Err(invalid("kind must be image, video, audio, mesh or document"));
    }
    let (offset, limit) = page(p.offset, p.limit)?;
    Ok(Request::List { offset, limit, kind: p.kind, query: query(p.query.take())? })
}

fn list(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    Ok(Effect::Assets { request: list_request(c, value)? })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    hash: Blake3,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveParams {
    items: Vec<Item>,
}

/// Distinct hashes, 1 to [`MAX_CHANGE`] of them.
fn distinct(items: Vec<Item>) -> Result<Vec<Blake3>, Error> {
    if items.is_empty() || items.len() > MAX_CHANGE {
        return Err(invalid("name 1 to 32 files"));
    }
    let mut seen = HashSet::new();
    Ok(items.into_iter().map(|item| item.hash).filter(|hash| seen.insert(*hash)).collect())
}

fn save(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: SaveParams = params(value)?;
    shelf(c)?;
    let hashes = distinct(p.items)?;
    let located = locate_all(c.document);
    let mut items = Vec::with_capacity(hashes.len());
    for hash in hashes {
        let found = located.get(&hash).ok_or_else(|| Error::new(ErrorKind::NotFound, "the project holds no such image, video, sound, 3D model or PDF"))?;
        if !c.assets.contains(&hash) {
            return Err(Error::new(ErrorKind::NotFound, "the file is missing from this project"));
        }
        // Named as library.attach titles a card placed from it.
        let name = match found.result {
            Some((_, index, count)) => entry_name(&format!("{} · {index}/{count}", short_label(&found.label))),
            None if found.titled => entry_name(&found.label),
            None => entry_name(kind_noun(found.kind)),
        };
        items.push(Saving { asset: found.asset.clone(), name });
    }
    Ok(Effect::Assets { request: Request::Save { items, origin: c.document.title().to_owned() } })
}

fn kind_noun(kind: Modality) -> &'static str {
    match kind {
        Modality::Image => "Image",
        Modality::Video => "Video",
        Modality::Audio => "Sound",
        Modality::Mesh => "3D model",
        Modality::Document => "PDF",
        Modality::Text | Modality::Style => "File",
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveParams {
    hashes: Vec<Blake3>,
}

fn remove(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: RemoveParams = params(value)?;
    shelf(c)?;
    let hashes = distinct(p.hashes.into_iter().map(|hash| Item { hash }).collect())?;
    Ok(Effect::Assets { request: Request::Remove { hashes } })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceParams {
    canvas: CanvasId,
    items: Vec<Item>,
    /// One new card id per item, supplied by the caller so replay stays deterministic.
    ids: Vec<NodeId>,
    at: Position,
    #[serde(default)]
    step: Option<f32>,
}

fn place(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: PlaceParams = params(value)?;
    if p.items.is_empty() || p.items.len() > MAX_ITEMS {
        return Err(invalid("place 1 to 32 entries at once"));
    }
    if p.ids.len() != p.items.len() {
        return Err(invalid("give one new card id per entry"));
    }
    let step = p.step.unwrap_or(DEFAULT_STEP);
    if !(step.is_finite() && step > 0.0 && step <= MAX_STEP) {
        return Err(invalid("step must be more than 0 and at most 10000"));
    }
    if !(within(p.at.x) && within(p.at.y)) {
        return Err(invalid("position is outside the canvas"));
    }
    let target = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let mut fresh = HashSet::new();
    for id in &p.ids {
        if target.nodes().contains_key(id) || !fresh.insert(*id) {
            return Err(Error::new(ErrorKind::Conflict, "a new card id is already taken; choose fresh ids"));
        }
    }
    let entries = shelf(c)?.with(|index| {
        p.items
            .iter()
            .map(|item| index.get(&item.hash).cloned().ok_or_else(|| Error::new(ErrorKind::NotFound, "My library has no such entry; list it again")))
            .collect::<Result<Vec<_>, Error>>()
    })?;
    let columns = columns(entries.len());
    let (mut changes, mut placed, mut media) = (Vec::new(), Vec::new(), Vec::new());
    for (index, (entry, id)) in entries.into_iter().zip(&p.ids).enumerate() {
        let (column, row) = (index % columns, index / columns);
        let pos = Position { x: p.at.x + step * column as f32, y: p.at.y + step * row as f32 };
        if !(within(pos.x) && within(pos.y)) {
            return Err(invalid("the cards would go outside the canvas"));
        }
        let kind = entry.kind().filter(|kind| ASSET_KINDS.contains(kind)).ok_or_else(|| invalid("that entry has no source card"))?;
        let (card, parameter) = source_kind(kind)?;
        let supplied: ParamValues = [(parameter.to_owned(), Value::Media(entry.asset.clone()))].into_iter().collect();
        let mut node = c.kinds.get(card)?.instantiate_instance(*id, BTreeMap::new(), &supplied)?;
        node.pos = pos;
        node.title = entry.name.clone();
        placed.push(json!({"id": id, "kind": card, "hash": entry.asset.hash, "name": entry.name, "origin": entry.origin}));
        media.push(entry.asset.hash);
        changes.push(Change::AddNode { canvas: p.canvas, node });
    }
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Place { from: PlaceFrom::Assets, media, changes, result: json!({"placed": placed}) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn repair(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let _: Empty = params(value)?;
    shelf(c)?;
    Ok(Effect::Assets { request: Request::Repair })
}
fn enabled_repair(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    repair(c, value).map(|_| ())
}
fn enabled_list(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    list_request(c, value).map(|_| ())
}
fn enabled_save(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    save(c, value).map(|_| ())
}
fn enabled_remove(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    remove(c, value).map(|_| ())
}
fn enabled_place(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    c.editable()?;
    place(c, value).map(|_| ())
}

fn hash() -> Json {
    json!({"type": "string", "pattern": "^[0-9a-f]{64}$"})
}
fn items(max: usize) -> Json {
    json!({"type": "array", "minItems": 1, "maxItems": max, "items": {"type": "object", "additionalProperties": false, "required": ["hash"], "properties": {"hash": hash()}}})
}
fn schema(required: &[&str], properties: Json) -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": required, "properties": properties})
}
fn list_schema() -> Json {
    let mut properties = paging();
    properties.insert("kind".into(), json!({"type": "string", "enum": ["image", "video", "audio", "mesh", "document"]}));
    object(properties)
}
fn repair_schema() -> Json {
    schema(&[], json!({}))
}
fn save_schema() -> Json {
    schema(&["items"], json!({"items": items(MAX_CHANGE)}))
}
fn remove_schema() -> Json {
    schema(&["hashes"], json!({"hashes": {"type": "array", "minItems": 1, "maxItems": MAX_CHANGE, "items": hash()}}))
}
fn place_schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    schema(
        &["canvas", "items", "ids", "at"],
        json!({"canvas": uuid, "items": items(MAX_ITEMS), "ids": {"type": "array", "minItems": 1, "maxItems": MAX_ITEMS, "items": uuid},
            "at": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}},
            "step": {"type": "number", "exclusiveMinimum": 0, "maximum": MAX_STEP}}),
    )
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "library.repair_saved",
            label: "Repair My library",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: repair_schema,
            params_doc: "{} -> {repaired: false} | {repaired: true, why, entries, kept, kept_as}; when My library's index can't be read, keeps it aside (kept_as, never over an earlier one) and rebuilds it from the files kept: each file that still matches its hash and is recognised becomes an entry named after its kind (names and origins were in the index); others are kept as unusable entries (kept), never deleted. Reads at most 1 GiB. Only a person runs it.",
            enabled: enabled_repair,
            run: repair,
        },
        CommandSpec {
            id: "library.saved",
            label: "Browse My library",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{kind?: image|video|audio|mesh|document, query?, offset?: 0, limit?: 1-200 (50)} -> {assets: [{hash, mime, kind, meta, name, origin, bytes}], total, offset, limit, used_bytes, max_bytes, entries, max_entries, damaged?: [why], index_damaged?: why, file?}; My library: files a person saved outside any project (from any of their projects), newest first, read afresh. `origin` is the title of the project it was saved from. `query` matches names, origins and MIME types. Never writes: an index that can't be read is reported as index_damaged, with nothing listed, until a person repairs it (library.repair_saved). Place entries with library.place_saved.",
            enabled: enabled_list,
            run: list,
        },
        CommandSpec {
            id: "library.save_assets",
            label: "Save to My library",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: save_schema,
            params_doc: "{items: [{hash}] (1-32)} -> {saved: [{hash, name, state: saved|already|failed, reason?}], used_bytes}; copies files the open project holds (as library.assets lists them, results included) into My library, outside any project, named after their card and marked with this project's title. Nothing else is kept: no path, canvas, card or address. At most 4096 entries and 1 GiB; a file that doesn't fit is refused, never anything evicted. Only a person runs it.",
            enabled: enabled_save,
            run: save,
        },
        CommandSpec {
            id: "library.unsave_assets",
            label: "Remove from My library",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: remove_schema,
            params_doc: "{hashes: [hash] (1-32)} -> {removed, used_bytes}; takes entries out of My library and deletes the files no entry holds any more. Cards placed from them keep their own copies. Only a person runs it.",
            enabled: enabled_remove,
            run: remove,
        },
        CommandSpec {
            id: "library.place_saved",
            label: "Place from My library",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: place_schema,
            params_doc: "{canvas, items: [{hash}] (1-32), ids: [NodeId] (one new card id per item), at: {x, y}, step?: grid spacing (320)} -> {placed: [{id, kind, hash, name, origin}]}; puts My library's entries on `canvas` as source cards titled with their names, in a grid from `at`, as one undo step. Files the project lacks are copied in through the import checks (hash, type, size caps). Reads the library as last listed (library.saved reads it afresh); refused whole if any entry is unknown or an id is taken.",
            enabled: enabled_place,
            run: place,
        },
    ]
}

#[cfg(test)]
mod tests;

//! The `library.*` commands (#96): keep a project's published technique versions in the library
//! shared by every project, and place one from it in any project
//! (`docs/decisions/2026-10-20-technique-library.md`). Listing, adding and removing ask the host's
//! `library.store` service ([`crate::library`]); placing reads the library the service last read and
//! edits the document only.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::library::{self, LIBRARY_SERVICE, MAX_ENTRY_BYTES, Shelf};
use crate::technique_cmds::derive;
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Position, Technique, TechniqueParam, TechniquePort};
use nodecraft_nodes::technique;
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, NodeId, ParamValues, TechniqueId};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};

/// The canvas extent positions may use (as `node.create`).
const MAX_COORDINATE: f32 = 10_000_000.0;

fn service(request: Json) -> Effect {
    Effect::Service { capability: LIBRARY_SERVICE, request }
}

fn list(_: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    if !value.as_object().is_some_and(serde_json::Map::is_empty) && !value.is_null() {
        return Err(invalid("library.list takes no parameters"));
    }
    Ok(service(json!({"op": "list"})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddParams {
    /// The canvas holding the project's version.
    canvas: CanvasId,
}

fn add(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: AddParams = params(value)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let entry = library::entry(canvas)?;
    let encoded = serde_json::to_string(&entry).map_err(|_| Error::new(ErrorKind::Internal, "the version couldn't be described"))?;
    if encoded.len() > MAX_ENTRY_BYTES {
        return Err(invalid("that version is too large for the library"));
    }
    Ok(service(json!({"op": "add", "canvas": encoded})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionParams {
    technique: TechniqueId,
    version: u32,
}

fn remove(_: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: VersionParams = params(value)?;
    Ok(service(json!({"op": "remove", "technique": p.technique, "version": p.version})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceParams {
    canvas: CanvasId,
    technique: TechniqueId,
    version: u32,
    id: NodeId,
    pos: Position,
    /// The id the version's canvas takes if the project doesn't have that version yet.
    copy: CanvasId,
}

/// `version` with new ids: the canvas takes `id`, and its nodes and wires ids derived from it,
/// since ids are unique across a project's canvases.
fn copied(version: &Canvas, id: CanvasId) -> Result<Canvas, Error> {
    let info = version.technique_info().ok_or_else(|| invalid("that canvas isn't a published technique"))?;
    let seed = NodeId::from_bytes(*id.as_bytes());
    let ids: BTreeMap<NodeId, NodeId> =
        version.nodes().keys().enumerate().map(|(n, old)| (*old, NodeId::from_bytes(derive(seed, "library node", n)))).collect();
    let new = |old: NodeId| ids.get(&old).copied().ok_or_else(|| invalid("the library's version names a node it doesn't hold"));
    let mut nodes = Vec::with_capacity(ids.len());
    for node in version.nodes().values() {
        let mut node = node.clone();
        node.id = new(node.id)?;
        nodes.push(node);
    }
    let mut edges = Vec::with_capacity(version.edges().len());
    for (n, edge) in version.edges().values().enumerate() {
        edges.push(Edge {
            id: EdgeId::from_bytes(derive(seed, "library edge", n)),
            from: Endpoint { node: new(edge.from.node)?, port: edge.from.port.clone() },
            to: Endpoint { node: new(edge.to.node)?, port: edge.to.port.clone() },
            order: edge.order,
        });
    }
    let rebase =
        |ports: &[TechniquePort]| ports.iter().map(|port| Ok(TechniquePort { node: new(port.node)?, ..port.clone() })).collect::<Result<Vec<_>, Error>>();
    let params = info.params.iter().map(|param| Ok(TechniqueParam { node: new(param.node)?, ..param.clone() })).collect::<Result<Vec<_>, Error>>()?;
    let info = Technique { inputs: rebase(&info.inputs)?, outputs: rebase(&info.outputs)?, params, source: None, ..info.clone() };
    Canvas::technique(id, version.title(), info, nodes, edges)
}

fn place(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: PlaceParams = params(value)?;
    if !(p.pos.x.is_finite() && p.pos.y.is_finite() && p.pos.x.abs() <= MAX_COORDINATE && p.pos.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    let canvases = c.document.canvases();
    if !canvases.contains_key(&p.canvas) {
        return Err(Error::new(ErrorKind::NotFound, "canvas does not exist"));
    }
    let mut changes = Vec::new();
    // A version number names one content, so a project that has this version uses its own.
    let owned = canvases.values().find(|canvas| canvas.technique_info().is_some_and(|info| info.id == p.technique && info.version == p.version));
    let version = match owned {
        Some(canvas) if canvas.technique_info().is_some_and(|info| info.retired) => {
            return Err(Error::new(ErrorKind::Conflict, "this version was removed from the project; undo that to place it again"));
        }
        Some(canvas) => canvas.clone(),
        None => {
            let kept = c.resources.require::<Shelf>()?.with(|library| {
                library.get(p.technique, p.version).cloned().ok_or_else(|| Error::new(ErrorKind::NotFound, "that version isn't in the library"))
            })?;
            if canvases.contains_key(&p.copy) {
                return Err(Error::new(ErrorKind::Conflict, "that canvas id is taken"));
            }
            let copy = copied(&kept, p.copy)?;
            let taken: BTreeSet<NodeId> = canvases.values().flat_map(|canvas| canvas.nodes().keys().copied()).chain([p.id]).collect();
            if copy.nodes().keys().any(|id| taken.contains(id)) {
                return Err(Error::new(ErrorKind::Conflict, "a copied node's id is already taken; use another copy id"));
            }
            changes.push(Change::AddCanvas { canvas: copy.clone() });
            copy
        }
    };
    let info = version.technique_info().ok_or_else(|| Error::new(ErrorKind::Internal, "the placed version isn't a technique"))?;
    let interface = technique::interface_of(&version, c.kinds)?;
    let mut node = c.kinds.get(technique::ID)?.instantiate_instance(p.id, interface.config()?, &ParamValues::new())?;
    node.pos = p.pos;
    node.title = info.name.clone();
    let result = json!({"id": p.id, "technique": info.id, "version": info.version, "canvas": version.id(), "copied": !changes.is_empty()});
    changes.push(Change::AddNode { canvas: p.canvas, node });
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result, gesture: None })
}

// ---- registration ----

/// Available where the host keeps a library.
fn available(c: &Context<'_>) -> Result<(), Error> {
    if c.services.contains(LIBRARY_SERVICE) { Ok(()) } else { Err(Error::new(ErrorKind::Unsupported, "the technique library isn't available here")) }
}

fn enabled(c: &Context<'_>, value: &Json, run: fn(&Context<'_>, &Json) -> Result<Effect, Error>) -> Result<(), Error> {
    available(c)?;
    run(c, value).map(|_| ())
}

fn enabled_edit(c: &Context<'_>, value: &Json, run: fn(&Context<'_>, &Json) -> Result<Effect, Error>) -> Result<(), Error> {
    enabled(c, value, run)?;
    c.editable()
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}
fn number() -> Json {
    json!({"type": "integer", "minimum": 1, "maximum": u32::MAX})
}
fn coordinate() -> Json {
    json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE})
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
            "library.list",
            "List the technique library",
            schema(&[], json!({})),
            "{} -> {techniques: [{technique, version, name, description, category, tags, inputs, outputs, nodes}]}; the versions kept in the library every project shares, by technique and version.",
            list,
            enabled
        ),
        spec!(
            "library.add",
            "Add a technique to the library",
            schema(&["canvas"], json!({"canvas": uuid()})),
            "{canvas: CanvasId of a published version} -> {added, technique}; keeps a copy of that version in the library, without its example results. `added` is false when the library already has that version. A version holding media can't be added yet.",
            add,
            enabled
        ),
        spec!(
            "library.remove",
            "Remove a technique from the library",
            schema(&["technique", "version"], json!({"technique": uuid(), "version": number()})),
            "{technique: TechniqueId, version} -> {removed}; takes that version out of the library. Projects that placed it keep their own copy.",
            remove,
            enabled
        ),
        spec!(
            "library.place",
            "Place a technique from the library",
            schema(
                &["canvas", "technique", "version", "id", "pos", "copy"],
                json!({"canvas": uuid(), "technique": uuid(), "version": number(), "id": uuid(), "copy": uuid(),
                    "pos": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate(), "y": coordinate()}}})
            ),
            "{canvas, technique: TechniqueId, version, id: new NodeId, pos: {x, y}, copy: new CanvasId} -> {id, technique, version, canvas, copied}; one technique node running that version, as one undo step. A project without that version gets a copy of it from the library as canvas `copy` (`copied` true); one that has it uses its own. Read library.list first: placing uses the library as last read.",
            place,
            enabled_edit
        ),
    ]
}

#[cfg(test)]
mod tests;

//! Batch Generate sheets (#163): a canvas of kind sheet whose Generate cards are its rows, listed
//! top to bottom (`docs/decisions/2026-10-20-batch-sheets.md`). These commands add, copy, move
//! and remove rows and wire references into them; a row's cells are its card's parameters, so
//! bulk edits are `node.params_set_bulk` and a run is `run.quote`/`run.start` with the rows' ids.
//!
//! Commands that add several nodes or wires take one caller-supplied `seed` and derive every new
//! id from it, so replay stays deterministic without the caller listing hundreds of ids.
use crate::commands::{CommandSpec, Context, Effect, Phase, invalid, params};
use nodecraft_graph::{Canvas, CanvasKind, Change, Changeset, Edge, Endpoint, Node, NodePosition, Port, Position};
use nodecraft_nodes::generation;
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, NodeId, ParamValues, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};

/// Most rows one sheet holds. Every row is a Generate card, so this also bounds a sheet's run.
pub const MAX_ROWS: usize = 1000;
/// Most rows one command adds.
pub const MAX_ADD: usize = 100;
/// Vertical distance between rows on the sheet canvas.
pub const ROW_SPACING: f32 = 400.0;
/// Where reference nodes sit, left of the rows.
const REFERENCE_X: f32 = -480.0;
const MAX_TITLE_BYTES: usize = 256;

/// The sheet's rows, top to bottom: its Generate cards by position, then id.
pub fn rows(canvas: &Canvas) -> Vec<&Node> {
    let mut rows: Vec<&Node> = canvas.nodes().values().filter(|node| generation::is_generation(&node.kind)).collect();
    rows.sort_by(|a, b| a.pos.y.total_cmp(&b.pos.y).then(a.pos.x.total_cmp(&b.pos.x)).then(a.id.cmp(&b.id)));
    rows
}

/// The nodes wired into every row: the sheet's shared references, top to bottom. A sheet with
/// one row has none, so a reference wired into that row alone doesn't spread to new rows.
pub fn shared(canvas: &Canvas, rows: &[&Node]) -> Vec<NodeId> {
    let row_ids: BTreeSet<NodeId> = rows.iter().map(|row| row.id).collect();
    let mut reaches: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        if row_ids.contains(&edge.to.node) && !row_ids.contains(&edge.from.node) {
            reaches.entry(edge.from.node).or_default().insert(edge.to.node);
        }
    }
    let mut shared: Vec<&Node> =
        reaches.into_iter().filter(|(_, reached)| row_ids.len() >= 2 && reached.len() == row_ids.len()).filter_map(|(id, _)| canvas.nodes().get(&id)).collect();
    shared.sort_by(|a, b| a.pos.y.total_cmp(&b.pos.y).then(a.id.cmp(&b.id)));
    shared.into_iter().map(|node| node.id).collect()
}

fn sheet<'a>(c: &'a Context<'_>, canvas: CanvasId) -> Result<&'a Canvas, Error> {
    let found = c.document.canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "sheet does not exist"))?;
    if found.kind() != CanvasKind::Sheet {
        return Err(invalid("that canvas isn't a sheet"));
    }
    Ok(found)
}

/// A new id for the `n`th node or wire a command makes from `seed`.
fn derive(seed: NodeId, what: &str, n: usize) -> [u8; 16] {
    let mut hash = blake3::Hasher::new_derive_key("nodecraft sheet ids 2026-10-20");
    hash.update(seed.as_bytes());
    hash.update(what.as_bytes());
    hash.update(&(n as u64).to_le_bytes());
    let mut bytes = [0; 16];
    if let Some(head) = hash.finalize().as_bytes().first_chunk::<16>() {
        bytes = *head;
    }
    bytes
}

fn row_pos(index: usize) -> Position {
    // MAX_ROWS rows fit well inside the canvas extent.
    Position { x: 0.0, y: index as f32 * ROW_SPACING }
}

/// Moves each existing row in `slots` (row ids top to bottom) that is out of its slot into it.
/// New rows are placed when they are made, so ids not on the canvas are skipped.
fn reflow(id: CanvasId, canvas: &Canvas, slots: &[NodeId]) -> Option<Change> {
    let positions: Vec<NodePosition> = slots
        .iter()
        .enumerate()
        .filter_map(|(index, row)| canvas.nodes().get(row).filter(|node| node.pos != row_pos(index)).map(|_| NodePosition { node: *row, pos: row_pos(index) }))
        .collect();
    (!positions.is_empty()).then_some(Change::MoveNodes { canvas: id, positions })
}

fn ensure_room(existing: usize, adding: usize) -> Result<(), Error> {
    if existing.saturating_add(adding) > MAX_ROWS {
        return Err(Error::new(ErrorKind::Overflow, format!("a sheet holds at most {MAX_ROWS} rows")));
    }
    Ok(())
}

fn edit(c: &Context<'_>, changes: Vec<Change>, result: Json) -> Result<Effect, Error> {
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result, gesture: None })
}

// ---- sheet.create ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateParams {
    canvas: CanvasId,
    title: String,
}

fn create(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: CreateParams = params(value)?;
    if p.title.trim().is_empty() || p.title.len() > MAX_TITLE_BYTES {
        return Err(invalid("a sheet's title must have 1 to 256 bytes"));
    }
    if c.document.canvases().contains_key(&p.canvas) {
        return Err(Error::new(ErrorKind::Conflict, "that canvas id is taken"));
    }
    edit(c, vec![Change::AddCanvas { canvas: Canvas::sheet(p.canvas, p.title) }], json!({"canvas": p.canvas}))
}

// ---- sheet.list / sheet.rows ----

fn list(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    if !value.as_object().is_some_and(serde_json::Map::is_empty) && !value.is_null() {
        return Err(invalid("sheet.list takes no parameters"));
    }
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Json::Null });
    }
    let sheets: Vec<Json> = c
        .document
        .canvases()
        .values()
        .filter(|canvas| canvas.kind() == CanvasKind::Sheet)
        .map(|canvas| json!({"canvas": canvas.id(), "title": canvas.title(), "rows": rows(canvas).len()}))
        .collect();
    Ok(Effect::None { result: json!({"sheets": sheets}) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CanvasParams {
    canvas: CanvasId,
}

fn text_param(node: &Node, name: &str) -> Json {
    match node.params.get(name) {
        Some(Value::Text(text)) => json!(text),
        _ => Json::Null,
    }
}

/// The values and copy number a template expansion gave `row` (`sheet.expand`), if it made it.
fn expansion(row: &Node) -> Json {
    let Some(Value::Text(text)) = row.config.get(nodecraft_exec::staleness::PROVENANCE_CONFIG) else { return Json::Null };
    let Ok(recorded) = serde_json::from_str::<Json>(text) else { return Json::Null };
    json!({"values": recorded.get("values").cloned().unwrap_or(Json::Null), "copy": recorded.get("copy").cloned().unwrap_or(Json::Null)})
}

fn describe_rows(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: CanvasParams = params(value)?;
    let canvas = sheet(c, p.canvas)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Json::Null });
    }
    let order = rows(canvas);
    let shared = shared(canvas, &order);
    let stale = nodecraft_exec::staleness::stale_report(canvas, c.kinds).map(|report| report.nodes).unwrap_or_default();
    let shared_set: BTreeSet<NodeId> = shared.iter().copied().collect();
    let row_ids: BTreeSet<NodeId> = order.iter().map(|row| row.id).collect();
    let rows: Vec<Json> = order
        .iter()
        .map(|row| {
            // Wires from other rows are a row feeding a row, not references.
            let mut wires: Vec<&Edge> = canvas.edges().values().filter(|edge| edge.to.node == row.id && !row_ids.contains(&edge.from.node)).collect();
            wires.sort_by(|a, b| a.to.port.cmp(&b.to.port).then(a.order.cmp(&b.order)).then(a.id.cmp(&b.id)));
            let references: Vec<Json> =
                wires.iter().map(|edge| json!({"node": edge.from.node, "port": edge.to.port, "shared": shared_set.contains(&edge.from.node)})).collect();
            let active = row.active.and_then(|id| row.history.iter().find(|g| g.id == id));
            let result = match active {
                None => "none",
                Some(generation) if generation.is_partial() => "partial",
                Some(_) => "complete",
            };
            json!({
                "id": row.id,
                "kind": row.kind,
                "title": row.title,
                "model": text_param(row, generation::MODEL),
                "prompt": text_param(row, generation::PROMPT),
                "references": references,
                "result": result,
                "stale": stale.contains(&row.id),
                "expansion": expansion(row),
            })
        })
        .collect();
    Ok(Effect::None { result: json!({"canvas": p.canvas, "title": canvas.title(), "rows": rows, "shared": shared}) })
}

// ---- sheet.rows_add ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddParams {
    canvas: CanvasId,
    seed: NodeId,
    /// The row the new ones follow and copy their model and settings from; the last row when
    /// absent.
    #[serde(default)]
    after: Option<NodeId>,
    /// One new row per prompt.
    prompts: Vec<String>,
    /// For the first row of an empty sheet: any catalog-backed generation kind.
    #[serde(default)]
    kind: Option<String>,
}

/// A copy of `row` under a new id: its model, settings and ports, without results.
fn blank_copy(row: &Node, id: NodeId) -> Node {
    let mut copy = row.clone();
    copy.id = id;
    copy.history.clear();
    copy.active = None;
    copy.tag = None;
    copy.field = None;
    copy
}

/// Where new rows go and what they copy: the row they follow (`after`, else the last row), or,
/// on an empty sheet, a new card of `kind`.
struct Base<'a> {
    /// The first new row's place.
    insert: usize,
    /// What every new row is a blank copy of.
    node: Node,
    /// The template row's wires from the sheet's shared references, which new rows get too.
    wires: Vec<&'a Edge>,
}

fn base<'a>(c: &Context<'_>, canvas: &'a Canvas, order: &[&'a Node], after: Option<NodeId>, kind: Option<&str>, seed: NodeId) -> Result<Base<'a>, Error> {
    let at = match after {
        Some(after) => order.iter().position(|row| row.id == after).ok_or_else(|| Error::new(ErrorKind::NotFound, "that row isn't on this sheet"))?,
        None => order.len().saturating_sub(1),
    };
    let template = order.get(at).copied();
    if template.is_some() && kind.is_some() {
        return Err(invalid("`kind` only picks an empty sheet's first row; new rows copy the row they follow"));
    }
    let mut node = match template {
        Some(row) => row.clone(),
        None => {
            let kind = kind.unwrap_or(generation::KIND);
            if !generation::is_generation(kind) {
                return Err(invalid("a sheet's rows are Generate cards"));
            }
            c.kinds.get(kind)?.instantiate_instance(NodeId::from_bytes(derive(seed, "base", 0)), BTreeMap::new(), &ParamValues::new())?
        }
    };
    // A new row says nothing of the expansion the row it copies came from.
    node.config.remove(nodecraft_exec::staleness::PROVENANCE_CONFIG);
    // New rows take the template's place in the shared references.
    let shared = shared(canvas, order);
    let wires =
        template.map(|row| canvas.edges().values().filter(|edge| edge.to.node == row.id && shared.contains(&edge.from.node)).collect()).unwrap_or_default();
    let insert = if template.is_some() { at.saturating_add(1) } else { 0 };
    Ok(Base { insert, node, wires })
}

/// The rows' ids top to bottom once `made` are placed at `insert`.
fn slots_with(order: &[&Node], insert: usize, made: &[NodeId]) -> Vec<NodeId> {
    let mut slots: Vec<NodeId> = order.iter().map(|row| row.id).collect();
    let tail = slots.split_off(insert.min(slots.len()));
    slots.extend(made.iter().copied());
    slots.extend(tail);
    slots
}

fn add(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: AddParams = params(value)?;
    let canvas = sheet(c, p.canvas)?;
    if p.prompts.is_empty() || p.prompts.len() > MAX_ADD {
        return Err(invalid("add 1 to 100 rows at a time"));
    }
    let order = rows(canvas);
    ensure_room(order.len(), p.prompts.len())?;
    let base = base(c, canvas, &order, p.after, p.kind.as_deref(), p.seed)?;
    let mut changes = Vec::new();
    let mut made = Vec::with_capacity(p.prompts.len());
    for (n, prompt) in p.prompts.into_iter().enumerate() {
        let id = NodeId::from_bytes(derive(p.seed, "row", n));
        let mut node = blank_copy(&base.node, id);
        node.pos = row_pos(base.insert.saturating_add(n));
        node.params.insert(generation::PROMPT.into(), Value::Text(prompt));
        changes.push(Change::AddNode { canvas: p.canvas, node });
        for wire in &base.wires {
            let edge = Edge {
                id: EdgeId::from_bytes(derive(p.seed, "wire", changes.len())),
                from: wire.from.clone(),
                to: Endpoint { node: id, port: wire.to.port.clone() },
                order: wire.order,
            };
            changes.push(Change::Connect { canvas: p.canvas, edge });
        }
        made.push(id);
    }
    changes.extend(reflow(p.canvas, canvas, &slots_with(&order, base.insert, &made)));
    edit(c, changes, json!({"rows": made}))
}

// ---- sheet.rows_duplicate ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RowsParams {
    canvas: CanvasId,
    seed: NodeId,
    rows: Vec<NodeId>,
}

fn selected(order: &[&Node], ids: &[NodeId], most: usize) -> Result<BTreeSet<NodeId>, Error> {
    if ids.is_empty() || ids.len() > most {
        return Err(invalid("too many or no rows chosen"));
    }
    let chosen: BTreeSet<NodeId> = ids.iter().copied().collect();
    if chosen.len() != ids.len() {
        return Err(invalid("a row is listed twice"));
    }
    if chosen.iter().any(|id| !order.iter().any(|row| row.id == *id)) {
        return Err(Error::new(ErrorKind::NotFound, "a row isn't on this sheet"));
    }
    Ok(chosen)
}

fn duplicate(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: RowsParams = params(value)?;
    let canvas = sheet(c, p.canvas)?;
    let order = rows(canvas);
    let chosen = selected(&order, &p.rows, MAX_ADD)?;
    ensure_room(order.len(), chosen.len())?;
    // Each copy follows its row, with its prompt, settings and references.
    let mut changes = Vec::new();
    let mut made = Vec::new();
    let mut slots: Vec<NodeId> = Vec::with_capacity(order.len().saturating_add(chosen.len()));
    let mut wire = 0usize;
    for row in &order {
        slots.push(row.id);
        if !chosen.contains(&row.id) {
            continue;
        }
        let id = NodeId::from_bytes(derive(p.seed, "row", made.len()));
        let mut node = blank_copy(row, id);
        node.pos = row_pos(slots.len());
        slots.push(id);
        changes.push(Change::AddNode { canvas: p.canvas, node });
        for edge in canvas.edges().values().filter(|edge| edge.to.node == row.id) {
            let copy = Edge {
                id: EdgeId::from_bytes(derive(p.seed, "wire", wire)),
                from: edge.from.clone(),
                to: Endpoint { node: id, port: edge.to.port.clone() },
                order: edge.order,
            };
            wire = wire.saturating_add(1);
            changes.push(Change::Connect { canvas: p.canvas, edge: copy });
        }
        made.push(id);
    }
    changes.extend(reflow(p.canvas, canvas, &slots));
    edit(c, changes, json!({"rows": made}))
}

// ---- sheet.rows_remove ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveParams {
    canvas: CanvasId,
    rows: Vec<NodeId>,
}

fn remove(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: RemoveParams = params(value)?;
    let canvas = sheet(c, p.canvas)?;
    let order = rows(canvas);
    let chosen = selected(&order, &p.rows, MAX_ROWS)?;
    let mut changes: Vec<Change> = chosen.iter().map(|id| Change::RemoveNode { canvas: p.canvas, id: *id }).collect();
    let kept: Vec<NodeId> = order.iter().map(|row| row.id).filter(|id| !chosen.contains(id)).collect();
    changes.extend(reflow(p.canvas, canvas, &kept));
    edit(c, changes, json!({"removed": chosen.len()}))
}

// ---- sheet.row_move ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveParams {
    canvas: CanvasId,
    row: NodeId,
    /// Its new place, counting from 0 at the top.
    index: usize,
}

fn move_row(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: MoveParams = params(value)?;
    let canvas = sheet(c, p.canvas)?;
    let mut order: Vec<NodeId> = rows(canvas).iter().map(|row| row.id).collect();
    let from = order.iter().position(|row| *row == p.row).ok_or_else(|| Error::new(ErrorKind::NotFound, "that row isn't on this sheet"))?;
    if p.index >= order.len() {
        return Err(invalid("that place is past the last row"));
    }
    let row = order.remove(from);
    order.insert(p.index, row);
    let changes: Vec<Change> = reflow(p.canvas, canvas, &order).into_iter().collect();
    if changes.is_empty() {
        return Err(Error::new(ErrorKind::Unsupported, "the row is already there"));
    }
    edit(c, changes, json!({"row": p.row, "index": p.index}))
}

// ---- sheet.reference_wire / sheet.reference_unwire ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireParams {
    canvas: CanvasId,
    seed: NodeId,
    /// A node on the sheet that isn't a row, such as an image source.
    reference: NodeId,
    /// The rows it is wired into; every row (a shared reference) when absent.
    #[serde(default)]
    rows: Option<Vec<NodeId>>,
}

fn targets<'a>(order: &[&'a Node], rows: Option<&[NodeId]>) -> Result<Vec<&'a Node>, Error> {
    match rows {
        None if order.is_empty() => Err(Error::new(ErrorKind::Unsupported, "the sheet has no rows")),
        None => Ok(order.to_vec()),
        Some(ids) => {
            if ids.len() > MAX_ROWS {
                return Err(invalid("too many rows"));
            }
            let chosen: BTreeSet<NodeId> = ids.iter().copied().collect();
            if chosen.is_empty() || chosen.len() != ids.len() {
                return Err(invalid("list each row once"));
            }
            let found: Vec<&Node> = order.iter().filter(|row| chosen.contains(&row.id)).copied().collect();
            if found.len() != chosen.len() {
                return Err(Error::new(ErrorKind::NotFound, "a row isn't on this sheet"));
            }
            Ok(found)
        }
    }
}

/// `row`'s first free input for `output`: one of its modality that is a list, or a single one
/// nothing feeds yet (`fed` says whether a port is).
fn free_input<'a>(row: &'a Node, fed: impl Fn(&str) -> bool, output: &Port) -> Option<&'a Port> {
    row.inputs.iter().find(|port| {
        port.port_type.modality == output.port_type.modality && (port.port_type.many || !output.port_type.many) && (port.port_type.many || !fed(&port.name))
    })
}

fn no_free_input(row: &Node, output: &Port) -> Error {
    let name = if row.title.is_empty() { format!("row {}", row.id) } else { format!("\"{}\"", row.title) };
    Error::new(ErrorKind::Unsupported, format!("{name} has no free {:?} input", output.port_type.modality))
}

fn wire(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: WireParams = params(value)?;
    let canvas = sheet(c, p.canvas)?;
    let order = rows(canvas);
    let reference = canvas.nodes().get(&p.reference).ok_or_else(|| Error::new(ErrorKind::NotFound, "that reference isn't on this sheet"))?;
    if generation::is_generation(&reference.kind) {
        return Err(invalid("a row can't be another row's reference"));
    }
    let [output] = reference.outputs.as_slice() else {
        return Err(invalid("a reference has exactly one output"));
    };
    let mut changes = Vec::new();
    let mut wired = Vec::new();
    for row in targets(&order, p.rows.as_deref())? {
        if canvas.edges().values().any(|edge| edge.from.node == reference.id && edge.to.node == row.id) {
            continue;
        }
        let fed = |port: &str| canvas.edges().values().any(|edge| edge.to.node == row.id && edge.to.port == port);
        let port = free_input(row, fed, output).ok_or_else(|| no_free_input(row, output))?;
        let order = canvas
            .edges()
            .values()
            .filter(|edge| edge.to.node == row.id && edge.to.port == port.name)
            .map(|edge| edge.order)
            .max()
            .map_or(0, |last| last.saturating_add(1));
        let edge = Edge {
            id: EdgeId::from_bytes(derive(p.seed, "wire", wired.len())),
            from: Endpoint { node: reference.id, port: output.name.clone() },
            to: Endpoint { node: row.id, port: port.name.clone() },
            order,
        };
        changes.push(Change::Connect { canvas: p.canvas, edge });
        wired.push(row.id);
    }
    if changes.is_empty() {
        return Err(Error::new(ErrorKind::Unsupported, "it is already wired into those rows"));
    }
    if reference.pos.x > REFERENCE_X {
        changes.push(Change::MoveNode { canvas: p.canvas, id: reference.id, pos: Position { x: REFERENCE_X, y: reference.pos.y } });
    }
    edit(c, changes, json!({"rows": wired}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnwireParams {
    canvas: CanvasId,
    reference: NodeId,
    #[serde(default)]
    rows: Option<Vec<NodeId>>,
}

fn unwire(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: UnwireParams = params(value)?;
    let canvas = sheet(c, p.canvas)?;
    let order = rows(canvas);
    let chosen: BTreeSet<NodeId> = targets(&order, p.rows.as_deref())?.into_iter().map(|row| row.id).collect();
    let changes: Vec<Change> = canvas
        .edges()
        .values()
        .filter(|edge| edge.from.node == p.reference && chosen.contains(&edge.to.node))
        .map(|edge| Change::Disconnect { canvas: p.canvas, id: edge.id })
        .collect();
    if changes.is_empty() {
        return Err(Error::new(ErrorKind::Unsupported, "it isn't wired into those rows"));
    }
    let count = changes.len();
    edit(c, changes, json!({"unwired": count}))
}

// ---- registration ----

/// Enabled when it would succeed; edits also need an editable document.
fn enabled(c: &Context<'_>, value: &Json, run: fn(&Context<'_>, &Json) -> Result<Effect, Error>) -> Result<(), Error> {
    if let Effect::Edit { .. } = run(c, value)? {
        c.editable()?;
    }
    Ok(())
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}
fn schema(required: &[&str], properties: Json) -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": required, "properties": properties})
}
fn rows_list() -> Json {
    json!({"type": "array", "items": uuid(), "minItems": 1, "maxItems": MAX_ROWS})
}

macro_rules! spec {
    ($id:literal, $label:literal, $schema:expr, $doc:literal, $run:ident) => {
        CommandSpec {
            id: $id,
            label: $label,
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || $schema,
            params_doc: $doc,
            enabled: |c, v| enabled(c, v, $run),
            run: $run,
        }
    };
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "sheet.create",
            "New Batch Generate sheet",
            schema(
                &["canvas", "title"],
                json!({"canvas": uuid(), "title": {"type": "string", "minLength": 1, "maxLength": MAX_TITLE_BYTES, "description": "1 to 256 bytes of UTF-8, not only spaces"}})
            ),
            "{canvas: new CanvasId, title} -> {canvas}; an empty Batch Generate sheet. Its rows are Generate cards: edit them with node.params_set, node.params_set_bulk and node.set_model, and run them with run.quote/run.start {nodes: row ids}.",
            create
        ),
        spec!(
            "sheet.list",
            "List sheets",
            schema(&[], json!({})),
            "{} -> {sheets: [{canvas, title, rows}]}; the project's Batch Generate sheets. Read-only.",
            list
        ),
        spec!(
            "sheet.rows",
            "List a sheet's rows",
            schema(&["canvas"], json!({"canvas": uuid()})),
            "{canvas} -> {canvas, title, rows: [{id, kind, title, model, prompt, references: [{node, port, shared}], result: none|complete|partial, stale, expansion: null|{values: {name: text|{node}}, copy}}], shared: [NodeId]}; rows top to bottom. A shared reference is wired into every row. Read-only.",
            describe_rows
        ),
        spec!(
            "sheet.rows_add",
            "Add rows",
            schema(
                &["canvas", "seed", "prompts"],
                json!({"canvas": uuid(), "seed": uuid(), "after": uuid(), "kind": {"enum": generation::KINDS},
                    "prompts": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": MAX_ADD}})
            ),
            "{canvas, seed: new UUID, after?: row, prompts: [text], kind?} -> {rows}; one row per prompt after `after` (the last row when absent), copying its model, settings and shared references. `kind` picks an empty sheet's first row: generation.image (default), generation.text, generation.mesh, generation.video, generation.style or generation.audio; it is refused once the sheet has rows. New ids derive from `seed`: use a fresh one per command. A one-row sheet has no shared references.",
            add
        ),
        spec!(
            "sheet.rows_duplicate",
            "Duplicate rows",
            schema(
                &["canvas", "seed", "rows"],
                json!({"canvas": uuid(), "seed": uuid(), "rows": {"type": "array", "items": uuid(), "minItems": 1, "maxItems": MAX_ADD}})
            ),
            "{canvas, seed: new UUID, rows} -> {rows}; a copy of each row, with its prompt, settings and references but no results, right after it. New ids derive from `seed`.",
            duplicate
        ),
        spec!(
            "sheet.rows_remove",
            "Remove rows",
            schema(&["canvas", "rows"], json!({"canvas": uuid(), "rows": rows_list()})),
            "{canvas, rows} -> {removed}; removes rows with their results, as one undo step. References stay on the sheet.",
            remove
        ),
        spec!(
            "sheet.row_move",
            "Move a row",
            schema(&["canvas", "row", "index"], json!({"canvas": uuid(), "row": uuid(), "index": {"type": "integer", "minimum": 0, "maximum": MAX_ROWS - 1}})),
            "{canvas, row, index} -> {row, index}; moves a row to `index` (0 is the top).",
            move_row
        ),
        spec!(
            "sheet.reference_wire",
            "Wire a reference into rows",
            schema(&["canvas", "seed", "reference"], json!({"canvas": uuid(), "seed": uuid(), "reference": uuid(), "rows": rows_list()})),
            "{canvas, seed: new UUID, reference: NodeId, rows?} -> {rows}; wires a node on the sheet (an image source, say) into each row's first input of its modality, after the row's other wires. Without `rows` it is wired into every row: a shared reference (on a sheet of two or more rows). Single inputs already fed are skipped. New wire ids derive from `seed`: use a fresh one per command.",
            wire
        ),
        spec!(
            "sheet.reference_unwire",
            "Unwire a reference",
            schema(&["canvas", "reference"], json!({"canvas": uuid(), "reference": uuid(), "rows": rows_list()})),
            "{canvas, reference, rows?} -> {unwired}; removes the reference's wires into `rows`, or into every row.",
            unwire
        ),
        expand::spec(),
    ]
}

mod expand;
pub mod template;

#[cfg(test)]
mod tests;

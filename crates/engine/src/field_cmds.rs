//! Persistent fields; narrow edits and one bounded transaction per action.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Field, FieldRect, NodeFieldAssignment, NodePosition, Position};
use nodecraft_types::{CanvasId, Error, ErrorKind, FieldId, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Coordinates {
    x: f32,
    y: f32,
}
impl From<Coordinates> for Position {
    fn from(p: Coordinates) -> Self {
        Self { x: p.x, y: p.y }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rectangle {
    pos: Coordinates,
    width: f32,
    height: f32,
}
impl From<Rectangle> for FieldRect {
    fn from(r: Rectangle) -> Self {
        Self { pos: r.pos.into(), width: r.width, height: r.height }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    canvas: CanvasId,
    field: Definition,
    #[serde(default)]
    nodes: Vec<NodeId>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    id: FieldId,
    title: String,
    rect: Rectangle,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    collapsed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    canvas: CanvasId,
    field: FieldId,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    rect: Option<Rectangle>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    collapsed: Option<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Move {
    canvas: CanvasId,
    field: FieldId,
    delta: Coordinates,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Remove {
    canvas: CanvasId,
    field: FieldId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    canvas: CanvasId,
    nodes: Vec<NodeId>,
    field: Option<FieldId>,
}
fn missing() -> Error {
    Error::new(ErrorKind::NotFound, "field or node does not exist in this canvas")
}
fn preflight(v: &Value) -> Result<(), Error> {
    if v.get("nodes").and_then(Value::as_array).is_some_and(|a| a.len() > 10_000) {
        return Err(invalid("at most 10000 nodes per field edit"));
    }
    for obj in [Some(v), v.get("field").filter(|v| v.is_object())].into_iter().flatten() {
        if obj.get("title").and_then(Value::as_str).is_some_and(|s| s.len() > 4096) || obj.get("color").and_then(Value::as_str).is_some_and(|s| s.len() > 64) {
            return Err(invalid("field metadata exceeds limit"));
        }
    }
    Ok(())
}
fn edit(changes: Vec<Change>, affected: usize) -> Effect {
    let result = json!({"affected":affected});
    if changes.is_empty() { Effect::None { result } } else { Effect::Edit { changes: Changeset::new(changes), result, gesture: None } }
}
fn assignments(c: &nodecraft_graph::Canvas, nodes: Vec<NodeId>, field: Option<FieldId>) -> Result<Vec<NodeFieldAssignment>, Error> {
    if field.is_some_and(|id| !c.fields().contains_key(&id)) {
        return Err(missing());
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for id in nodes {
        if !seen.insert(id) {
            return Err(invalid("duplicate node in field assignment"));
        }
        let node = c.nodes().get(&id).ok_or_else(missing)?;
        if node.field != field {
            out.push(NodeFieldAssignment { node: id, field });
        }
    }
    Ok(out)
}
fn create(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    preflight(v)?;
    let p: Create = params(v)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(missing)?;
    if c.document.canvases().values().any(|c| c.fields().contains_key(&p.field.id)) {
        return Err(Error::new(ErrorKind::Conflict, "field already exists"));
    }
    let field = Field { id: p.field.id, title: p.field.title, rect: p.field.rect.into(), color: p.field.color, collapsed: p.field.collapsed };
    field.validate()?;
    let mut seen = BTreeSet::new();
    let mut assignments = Vec::new();
    for id in p.nodes {
        if !seen.insert(id) {
            return Err(invalid("duplicate node in field assignment"));
        }
        canvas.nodes().get(&id).ok_or_else(missing)?;
        assignments.push(NodeFieldAssignment { node: id, field: Some(field.id) });
    }
    let affected = assignments.len();
    let mut changes = vec![Change::PutField { canvas: p.canvas, field }];
    if !assignments.is_empty() {
        changes.push(Change::SetNodeFields { canvas: p.canvas, assignments });
    }
    Ok(edit(changes, affected))
}
fn update(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    preflight(v)?;
    for name in ["title", "rect", "collapsed"] {
        if v.get(name).is_some_and(Value::is_null) {
            return Err(invalid("omit unchanged field properties; only color permits null"));
        }
    }
    let p: Update = params(v)?;
    let original = c.document.canvases().get(&p.canvas).and_then(|c| c.fields().get(&p.field)).ok_or_else(missing)?;
    let mut field = original.clone();
    if let Some(title) = p.title {
        field.title = title;
    }
    if let Some(rect) = p.rect {
        field.rect = rect.into();
    }
    if let Some(collapsed) = p.collapsed {
        field.collapsed = collapsed;
    }
    if v.get("color").is_some() {
        field.color = p.color;
    }
    field.validate()?;
    if field == *original {
        return Ok(edit(vec![], 0));
    }
    Ok(edit(vec![Change::PutField { canvas: p.canvas, field }], 1))
}
fn translated(pos: Position, delta: Position) -> Result<Position, Error> {
    let x = f64::from(pos.x) + f64::from(delta.x);
    let y = f64::from(pos.y) + f64::from(delta.y);
    if !x.is_finite() || !y.is_finite() || x.abs() > 10_000_000.0 || y.abs() > 10_000_000.0 {
        return Err(invalid("field move exceeds canvas bounds"));
    }
    Ok(Position { x: x as f32, y: y as f32 })
}
fn movement(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    let p: Move = params(v)?;
    let delta: Position = p.delta.into();
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(missing)?;
    let mut field = canvas.fields().get(&p.field).ok_or_else(missing)?.clone();
    field.rect.pos = translated(field.rect.pos, delta)?;
    field.validate()?;
    let mut positions = Vec::new();
    for node in canvas.nodes().values().filter(|n| n.field == Some(p.field)) {
        let pos = translated(node.pos, delta)?;
        if pos != node.pos {
            positions.push(NodePosition { node: node.id, pos });
        }
    }
    let count = positions.len();
    let mut changes = Vec::new();
    if canvas.fields().get(&p.field) != Some(&field) {
        changes.push(Change::PutField { canvas: p.canvas, field });
    }
    if !positions.is_empty() {
        changes.push(Change::MoveNodes { canvas: p.canvas, positions });
    }
    Ok(edit(changes, count))
}
fn remove(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    let p: Remove = params(v)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(missing)?;
    if !canvas.fields().contains_key(&p.field) {
        return Err(missing());
    }
    let assignments: Vec<_> = canvas.nodes().values().filter(|n| n.field == Some(p.field)).map(|n| NodeFieldAssignment { node: n.id, field: None }).collect();
    let count = assignments.len();
    let mut changes = vec![];
    if !assignments.is_empty() {
        changes.push(Change::SetNodeFields { canvas: p.canvas, assignments });
    }
    changes.push(Change::RemoveField { canvas: p.canvas, id: p.field });
    Ok(edit(changes, count))
}
fn set(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    preflight(v)?;
    if v.get("field").is_none() {
        return Err(invalid("field is required; null clears membership"));
    }
    let p: Set = params(v)?;
    if p.nodes.is_empty() {
        return Err(invalid("select at least one node"));
    }
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(missing)?;
    let assignments = assignments(canvas, p.nodes, p.field)?;
    let count = assignments.len();
    Ok(edit(if count == 0 { vec![] } else { vec![Change::SetNodeFields { canvas: p.canvas, assignments }] }, count))
}
fn enabled(c: &Context<'_>, v: &Value, run: fn(&Context<'_>, &Value) -> Result<Effect, Error>) -> Result<(), Error> {
    if let Effect::Edit { changes, .. } = run(c, v)? {
        c.preview_changes(&changes)?;
    }
    Ok(())
}
fn id() -> Value {
    json!({"type":"string","format":"uuid"})
}
fn position() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{"x":{"type":"number"},"y":{"type":"number"}},"required":["x","y"]})
}
fn rect() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{"pos":position(),"width":{"type":"number","exclusiveMinimum":0,"maximum":20000000},"height":{"type":"number","exclusiveMinimum":0,"maximum":20000000}},"required":["pos","width","height"],"description":"Both corners within +/-10000000 world points"})
}
fn schema(kind: &str) -> Value {
    let mut props = json!({"canvas":id(),"field":id()});
    let required = match kind {
        "create" => {
            props["field"] = json!({"type":"object","additionalProperties":false,"properties":{"id":id(),"title":{"type":"string","maxLength":4096},"rect":rect(),"color":{"type":["string","null"],"maxLength":64},"collapsed":{"type":"boolean"}},"required":["id","title","rect"]});
            props["nodes"] = json!({"type":"array","maxItems":10000,"uniqueItems":true,"items":id()});
            vec!["canvas", "field"]
        }
        "update" => {
            props["title"] = json!({"type":"string","maxLength":4096});
            props["rect"] = rect();
            props["color"] = json!({"type":["string","null"],"maxLength":64});
            props["collapsed"] = json!({"type":"boolean"});
            vec!["canvas", "field"]
        }
        "move" => {
            props["delta"] = position();
            vec!["canvas", "field", "delta"]
        }
        "set" => {
            props["nodes"] = json!({"type":"array","minItems":1,"maxItems":10000,"uniqueItems":true,"items":id()});
            props["field"] = json!({"type":["string","null"],"format":"uuid"});
            vec!["canvas", "nodes", "field"]
        }
        _ => vec!["canvas", "field"],
    };
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":props,"required":required})
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "field.create",
            label: "Create field",
            shortcut: Some("F"),
            alternate_shortcuts: &[],
            input_schema: || schema("create"),
            params_doc: "{canvas,field:{id,title,rect,color?,collapsed?},nodes?}; create and assign in one undo step",
            enabled: |c, v| enabled(c, v, create),
            run: create,
        },
        CommandSpec {
            id: "field.update",
            label: "Edit field",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || schema("update"),
            params_doc: "{canvas,field,title?,rect?,color?,collapsed?}; omission preserves; color:null clears; resize never moves members",
            enabled: |c, v| enabled(c, v, update),
            run: update,
        },
        CommandSpec {
            id: "field.move",
            label: "Move field and members",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || schema("move"),
            params_doc: "{canvas,field,delta:{x,y}}; every member moves atomically; zero delta is a no-op",
            enabled: |c, v| enabled(c, v, movement),
            run: movement,
        },
        CommandSpec {
            id: "field.remove",
            label: "Remove field",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || schema("remove"),
            params_doc: "{canvas,field}; clear membership and remove field, preserving nodes and edges",
            enabled: |c, v| enabled(c, v, remove),
            run: remove,
        },
        CommandSpec {
            id: "node.field_set",
            label: "Set node field",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || schema("set"),
            params_doc: "{canvas,nodes,field}; required nullable field; explicit same-canvas membership",
            enabled: |c, v| enabled(c, v, set),
            run: set,
        },
    ]
}

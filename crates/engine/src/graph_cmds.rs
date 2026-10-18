//! The initial graph commands. IDs are supplied by the caller so replay is deterministic.

use nodecraft_graph::{Change, Changeset, Edge, Node, Position};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, Context, Effect, params};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddNodeParams {
    pub canvas: CanvasId,
    pub node: Node,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveNodeParams {
    pub canvas: CanvasId,
    pub id: NodeId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveNodeParams {
    pub canvas: CanvasId,
    pub id: NodeId,
    pub pos: Position,
    /// Reuse only within one pointer gesture; omit for separate undo steps.
    #[serde(default)]
    pub gesture: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectParams {
    pub canvas: CanvasId,
    pub edge: Edge,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisconnectParams {
    pub canvas: CanvasId,
    pub id: EdgeId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn edit(change: Change, result: Value, gesture: Option<String>) -> Effect {
    Effect::Edit { changes: Changeset::new(vec![change]), result, gesture }
}

fn add(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: AddNodeParams = params(value)?;
    if !p.node.history.is_empty() || p.node.active.is_some() {
        return Err(crate::commands::invalid("node.add cannot supply generation history or active selection; use generation edits"));
    }
    let result = json!({"id": p.node.id});
    Ok(edit(Change::AddNode { canvas: p.canvas, node: p.node }, result, None))
}
fn remove(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: RemoveNodeParams = params(value)?;
    Ok(edit(Change::RemoveNode { canvas: p.canvas, id: p.id }, Value::Null, None))
}
fn move_node(context: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: MoveNodeParams = params(value)?;
    crate::session::validate_gesture(p.gesture.as_deref())?;
    let node = context
        .document
        .canvases()
        .get(&p.canvas)
        .and_then(|canvas| canvas.nodes().get(&p.id))
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "document entity does not exist"))?;
    if node.pos == p.pos {
        return Ok(Effect::None { result: Value::Null });
    }
    Ok(edit(Change::MoveNode { canvas: p.canvas, id: p.id, pos: p.pos }, Value::Null, p.gesture))
}
fn connect(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: ConnectParams = params(value)?;
    let result = json!({"id": p.edge.id});
    Ok(edit(Change::Connect { canvas: p.canvas, edge: p.edge }, result, None))
}
fn disconnect(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: DisconnectParams = params(value)?;
    Ok(edit(Change::Disconnect { canvas: p.canvas, id: p.id }, Value::Null, None))
}

fn enabled_edit(context: &Context<'_>, value: &Value, run: fn(&Context<'_>, &Value) -> Result<Effect, Error>) -> Result<(), Error> {
    context.editable()?;
    if let Effect::Edit { changes, .. } = run(context, value)? {
        context.preview_changes(&changes)?;
    }
    Ok(())
}
fn enabled_add(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    enabled_edit(c, v, add)
}
fn enabled_remove(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    enabled_edit(c, v, remove)
}
fn enabled_move(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    enabled_edit(c, v, move_node)
}
fn enabled_connect(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    enabled_edit(c, v, connect)
}
fn enabled_disconnect(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    enabled_edit(c, v, disconnect)
}
fn enabled_undo(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    c.editable()?;
    if !v.is_null() {
        let _: Empty = params(v)?;
    }
    if c.can_undo { Ok(()) } else { Err(Error::new(ErrorKind::Conflict, "nothing to undo")) }
}
fn enabled_redo(c: &Context<'_>, v: &Value) -> Result<(), Error> {
    c.editable()?;
    if !v.is_null() {
        let _: Empty = params(v)?;
    }
    if c.can_redo { Ok(()) } else { Err(Error::new(ErrorKind::Conflict, "nothing to redo")) }
}
fn undo(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    enabled_undo(c, v)?;
    Ok(Effect::Undo)
}
fn redo(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    enabled_redo(c, v)?;
    Ok(Effect::Redo)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "node.add",
            alternate_shortcuts: &[],
            input_schema: crate::command_schemas::add_schema,
            label: "Add node",
            shortcut: None,
            params_doc: "{canvas: CanvasId, node: graph::Node}; node id and kind are required; optional graph fields use defaults; caller supplies a new node id.",
            enabled: enabled_add,
            run: add,
        },
        CommandSpec {
            id: "node.remove",
            alternate_shortcuts: &[],
            input_schema: crate::command_schemas::remove_schema,
            label: "Remove node",
            shortcut: None,
            params_doc: "{canvas: CanvasId, id: NodeId}; removes incident edges too.",
            enabled: enabled_remove,
            run: remove,
        },
        CommandSpec {
            id: "node.move",
            alternate_shortcuts: &[],
            input_schema: crate::command_schemas::move_schema,
            label: "Move node",
            shortcut: None,
            params_doc: "{canvas: CanvasId, id: NodeId, pos: {x: number, y: number}, gesture?: string}; gesture is a nonempty token of at most 128 bytes, unique per drag.",
            enabled: enabled_move,
            run: move_node,
        },
        CommandSpec {
            id: "edge.connect",
            alternate_shortcuts: &[],
            input_schema: crate::command_schemas::connect_schema,
            label: "Connect ports",
            shortcut: None,
            params_doc: "{canvas: CanvasId, edge: {id: EdgeId, from: {node: NodeId, port: string}, to: {node: NodeId, port: string}, order: u32}}.",
            enabled: enabled_connect,
            run: connect,
        },
        CommandSpec {
            id: "edge.disconnect",
            alternate_shortcuts: &[],
            input_schema: crate::command_schemas::disconnect_schema,
            label: "Disconnect ports",
            shortcut: None,
            params_doc: "{canvas: CanvasId, id: EdgeId}.",
            enabled: enabled_disconnect,
            run: disconnect,
        },
        CommandSpec {
            id: "edit.undo",
            alternate_shortcuts: &[],
            input_schema: crate::command_schemas::empty_schema,
            label: "Undo",
            shortcut: Some("Command+Z"),
            params_doc: "{}",
            enabled: enabled_undo,
            run: undo,
        },
        CommandSpec {
            id: "edit.redo",
            alternate_shortcuts: &["Ctrl+Y"],
            input_schema: crate::command_schemas::empty_schema,
            label: "Redo",
            shortcut: Some("Command+Shift+Z"),
            params_doc: "{}",
            enabled: enabled_redo,
            run: redo,
        },
    ]
}

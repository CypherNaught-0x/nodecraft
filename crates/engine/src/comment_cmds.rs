//! Local-owner discussion editing. Identity/roles and sharing are deliberately not implied.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Comment, CommentReply, MAX_COMMENT_BYTES, MAX_REPLY_BYTES, Position};
use nodecraft_types::{CanvasId, CommentId, Error, ErrorKind, NodeId, ProjectId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictPosition {
    x: f32,
    y: f32,
}
impl From<StrictPosition> for Position {
    fn from(pos: StrictPosition) -> Self {
        Self { x: pos.x, y: pos.y }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    canvas: CanvasId,
    id: CommentId,
    text: String,
    pos: StrictPosition,
    node: Option<NodeId>,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    canvas: CanvasId,
    id: CommentId,
    text: String,
    reply: Option<CommentId>,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    canvas: CanvasId,
    id: CommentId,
    reply: CommentId,
    text: String,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyRemove {
    canvas: CanvasId,
    id: CommentId,
    reply: CommentId,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Move {
    canvas: CanvasId,
    id: CommentId,
    pos: StrictPosition,
    node: Option<NodeId>,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolve {
    canvas: CanvasId,
    id: CommentId,
    resolved: bool,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Remove {
    canvas: CanvasId,
    id: CommentId,
    #[serde(default, deserialize_with = "revision_value")]
    expected_revision: Option<u64>,
}
fn revision_value<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    u64::deserialize(deserializer).map(Some)
}
fn revision(c: &Context<'_>, expected: Option<u64>) -> Result<(), Error> {
    c.ensure_editable()?;
    if expected.is_some_and(|expected| expected != c.revision) {
        return Err(Error::new(ErrorKind::Conflict, "discussion changed; review the current document before saving"));
    }
    Ok(())
}
fn raw(c: &Context<'_>, v: &Value, limit: usize) -> Result<(), Error> {
    revision(c, v.get("expected_revision").and_then(Value::as_u64))?;
    if v.get("text").and_then(Value::as_str).is_some_and(|text| text.len() > limit) {
        return Err(invalid("message exceeds UTF-8 byte limit"));
    }
    Ok(())
}
fn thread<'a>(c: &'a Context<'_>, canvas: CanvasId, id: CommentId) -> Result<&'a Comment, Error> {
    c.document.canvases().get(&canvas).and_then(|canvas| canvas.comments().get(&id)).ok_or_else(|| Error::new(ErrorKind::NotFound, "discussion does not exist"))
}
fn nonempty(text: &str) -> Result<(), Error> {
    if text.trim().is_empty() { Err(invalid("message must contain non-whitespace text")) } else { Ok(()) }
}
fn effect(change: Option<Change>) -> Effect {
    match change {
        Some(change) => Effect::Edit { changes: Changeset::new(vec![change]), result: Value::Null, gesture: None },
        None => Effect::None { result: Value::Null },
    }
}
fn create(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    raw(c, v, MAX_COMMENT_BYTES)?;
    let p: Create = params(v)?;
    revision(c, p.expected_revision)?;
    nonempty(&p.text)?;
    let mut comment = Comment::new(p.id, p.text, p.pos.into());
    comment.node = p.node;
    if let Some(old) = c.document.canvases().get(&p.canvas).and_then(|canvas| canvas.comments().get(&p.id)) {
        if *old == comment {
            return Ok(effect(None));
        }
        return Err(Error::new(ErrorKind::Conflict, "discussion id is already in use"));
    }
    Ok(effect(Some(Change::PutComment { canvas: p.canvas, comment })))
}
fn edit(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    let limit = if v.get("reply").is_some_and(|v| !v.is_null()) { MAX_REPLY_BYTES } else { MAX_COMMENT_BYTES };
    raw(c, v, limit)?;
    let p: Edit = params(v)?;
    revision(c, p.expected_revision)?;
    nonempty(&p.text)?;
    let root = thread(c, p.canvas, p.id)?;
    let change = if let Some(reply) = p.reply {
        let old = root.replies.iter().find(|item| item.id == reply).ok_or_else(|| Error::new(ErrorKind::NotFound, "reply does not belong to discussion"))?;
        (old.text != p.text).then_some(Change::SetCommentReplyText { canvas: p.canvas, id: p.id, reply, text: p.text })
    } else {
        (root.text != p.text).then_some(Change::SetCommentText { canvas: p.canvas, id: p.id, text: p.text })
    };
    Ok(effect(change))
}
fn reply(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    raw(c, v, MAX_REPLY_BYTES)?;
    let p: Reply = params(v)?;
    revision(c, p.expected_revision)?;
    nonempty(&p.text)?;
    let root = thread(c, p.canvas, p.id)?;
    if root.resolved {
        return Err(Error::new(ErrorKind::Conflict, "reopen the discussion before replying"));
    }
    Ok(effect(Some(Change::InsertCommentReply { canvas: p.canvas, id: p.id, index: root.replies.len(), reply: CommentReply { id: p.reply, text: p.text } })))
}
fn reply_remove(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    raw(c, v, 0)?;
    let p: ReplyRemove = params(v)?;
    revision(c, p.expected_revision)?;
    thread(c, p.canvas, p.id)?;
    Ok(effect(Some(Change::RemoveCommentReply { canvas: p.canvas, id: p.id, reply: p.reply })))
}
fn move_pin(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    raw(c, v, 0)?;
    let p: Move = params(v)?;
    revision(c, p.expected_revision)?;
    let root = thread(c, p.canvas, p.id)?;
    Ok(effect((root.node != p.node || root.pos != Position::from(p.pos)).then_some(Change::SetCommentPin {
        canvas: p.canvas,
        id: p.id,
        node: p.node,
        pos: p.pos.into(),
    })))
}
fn resolve(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    raw(c, v, 0)?;
    let p: Resolve = params(v)?;
    revision(c, p.expected_revision)?;
    let root = thread(c, p.canvas, p.id)?;
    Ok(effect((root.resolved != p.resolved).then_some(Change::SetCommentResolved { canvas: p.canvas, id: p.id, resolved: p.resolved })))
}
fn remove(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    raw(c, v, 0)?;
    let p: Remove = params(v)?;
    revision(c, p.expected_revision)?;
    thread(c, p.canvas, p.id)?;
    Ok(effect(Some(Change::RemoveComment { canvas: p.canvas, id: p.id })))
}
fn enabled(c: &Context<'_>, v: &Value, run: fn(&Context<'_>, &Value) -> Result<Effect, Error>) -> Result<(), Error> {
    if let Effect::Edit { changes, .. } = run(c, v)? {
        c.preview_changes(&changes)?;
    }
    Ok(())
}
fn schema(action: &str) -> Value {
    let id = json!({"type":"string","format":"uuid"});
    let mut properties = json!({"canvas":id,"id":id,"expected_revision":{"type":"integer","minimum":0,"maximum":u64::MAX}});
    let mut required = vec!["canvas", "id"];
    if matches!(action, "create" | "edit" | "reply") {
        properties["text"] = json!({"type":"string","maxLength":if action=="reply"{MAX_REPLY_BYTES}else{MAX_COMMENT_BYTES},"description":"Non-whitespace text; UTF-8 byte limit also enforced; thread total at most 1 MiB"});
        required.push("text");
    }
    if matches!(action, "create" | "move") {
        properties["pos"] = json!({"type":"object","properties":{"x":{"type":"number","minimum":-10000000,"maximum":10000000},"y":{"type":"number","minimum":-10000000,"maximum":10000000}},"required":["x","y"],"additionalProperties":false});
        properties["node"] = json!({"anyOf":[id,{"type":"null"}],"description":"Absent/null means canvas position; node means offset from stored body origin"});
        required.push("pos");
    }
    if matches!(action, "edit" | "reply" | "reply_remove") {
        properties["reply"] = if action == "edit" { json!({"anyOf":[id,{"type":"null"}]}) } else { id };
        if action != "edit" {
            required.push("reply");
        }
    }
    if action == "resolve" {
        properties["resolved"] = json!({"type":"boolean"});
        required.push("resolved");
    }
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":properties,"required":required,"additionalProperties":false})
}

/// Fresh view capability. This is not authorization or a persisted sharing identity.
#[derive(Clone, Debug)]
pub struct CommentContext {
    pub project: ProjectId,
    pub canvas: CanvasId,
    pub revision: u64,
    pub blocked: bool,
}
pub const SERVICE: &str = "canvas.comments";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct View {
    id: Option<CommentId>,
    node: Option<NodeId>,
    pos: Option<StrictPosition>,
}
fn view(c: &Context<'_>, v: &Value, action: &str) -> Result<Effect, Error> {
    if let Some(fields) = v.as_object() {
        let allowed: &[&str] = match action {
            "open" => &["id"],
            "compose" => &["node", "pos"],
            _ => &[],
        };
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(invalid("unknown discussion view field"));
        }
    }
    let p: View = params(v)?;
    let host = c.resources.require::<CommentContext>()?;
    if !c.services.contains(SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "live discussions host is unavailable"));
    }
    if host.project != c.document.id() || host.revision != c.revision || host.blocked {
        return Err(Error::new(ErrorKind::Conflict, "finish active editor or refresh live discussions"));
    }
    let canvas = c.document.canvases().get(&host.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "discussion canvas is unavailable"))?;
    match action {
        "open" => {
            if p.node.is_some() || p.pos.is_some() {
                return Err(invalid("open only accepts a discussion id"));
            }
            let id = p.id.ok_or_else(|| invalid("discussion id is required"))?;
            if !canvas.comments().contains_key(&id) {
                return Err(Error::new(ErrorKind::NotFound, "discussion does not exist"));
            }
        }
        "compose" => {
            if p.id.is_some() {
                return Err(invalid("composition does not accept an existing discussion id"));
            }
            let pos = p.pos.ok_or_else(|| invalid("pin position is required"))?;
            let mut pin = Comment::new(CommentId::from_bytes([0; 16]), "", pos.into());
            pin.node = p.node;
            pin.validate_payload()?;
            pin.world_position(canvas)?;
        }
        _ => {
            if p.id.is_some() || p.node.is_some() || p.pos.is_some() {
                return Err(invalid("toggle accepts no fields"));
            }
        }
    }
    Ok(Effect::Service { capability: SERVICE, request: json!({"action":action,"id":p.id,"node":p.node,"pos":p.pos}) })
}
fn view_schema(action: &str) -> Value {
    let mut result = schema(if action == "compose" { "move" } else { "remove" });
    if let Some(props) = result.get_mut("properties").and_then(Value::as_object_mut) {
        props.remove("canvas");
        props.remove("id");
        props.remove("expected_revision");
        if action == "open" {
            props.insert("id".into(), json!({"type":"string","format":"uuid"}));
        }
    }
    result["required"] = match action {
        "open" => json!(["id"]),
        "compose" => json!(["pos"]),
        _ => json!([]),
    };
    result
}
pub fn specs() -> Vec<CommandSpec> {
    let mut result = Vec::new();
    macro_rules! mutation {
        ($id:literal,$label:literal,$action:literal,$run:ident) => {
            result.push(CommandSpec {
                id: $id,
                label: $label,
                shortcut: None,
                alternate_shortcuts: &[],
                input_schema: || schema($action),
                params_doc: "Local-owner discussion edit; optional expected_revision rejects stale edits; accepted content never enters errors.",
                enabled: |c, v| enabled(c, v, $run),
                run: $run,
            });
        };
    }
    mutation!("comment.create", "Create discussion", "create", create);
    mutation!("comment.edit", "Edit message", "edit", edit);
    mutation!("comment.reply", "Reply to discussion", "reply", reply);
    mutation!("comment.reply_remove", "Delete reply", "reply_remove", reply_remove);
    mutation!("comment.move", "Move discussion pin", "move", move_pin);
    mutation!("comment.resolve", "Resolve or reopen discussion", "resolve", resolve);
    mutation!("comment.remove", "Delete discussion", "remove", remove);
    macro_rules! host {
        ($id:literal,$label:literal,$action:literal,$shortcut:expr) => {
            result.push(CommandSpec {
                id: $id,
                label: $label,
                shortcut: $shortcut,
                alternate_shortcuts: &[],
                input_schema: || view_schema($action),
                params_doc: "Live scoped discussion view only; raw headless calls require a host capability.",
                enabled: |c, v| view(c, v, $action).map(|_| ()),
                run: |c, v| view(c, v, $action),
            });
        };
    }
    host!("comment.compose", "Pin a discussion", "compose", Some("C"));
    host!("comment.open", "Open discussion", "open", None);
    host!("canvas.comments_toggle", "Show discussions", "toggle", None);
    result
}

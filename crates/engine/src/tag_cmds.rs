//! Persistent colour tags. All document changes use the existing bounded transaction path.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, NodeTagAssignment, Tag};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, TagId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_TAG_NODES: usize = 10_000;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    id: TagId,
    title: String,
    #[serde(default)]
    color: Option<String>,
}
impl Definition {
    fn into_tag(self) -> Result<Tag, Error> {
        if self.title.len() > 4096 || self.color.as_ref().is_some_and(|s| s.len() > 64) {
            return Err(invalid("tag metadata exceeds limit"));
        }
        Ok(Tag { id: self.id, title: self.title, color: self.color })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Put {
    canvas: CanvasId,
    tag: Definition,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Remove {
    canvas: CanvasId,
    tag: TagId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    canvas: CanvasId,
    nodes: Vec<NodeId>,
    tag: Option<TagId>,
    #[serde(default)]
    create: Option<Definition>,
}
fn missing() -> Error {
    Error::new(ErrorKind::NotFound, "tag or node does not exist in this canvas")
}
fn preflight(v: &Value) -> Result<(), Error> {
    if v.get("create").is_some_and(Value::is_null) {
        return Err(invalid("omit create when not creating a tag"));
    }
    if v.get("nodes").and_then(Value::as_array).is_some_and(|a| a.is_empty() || a.len() > MAX_TAG_NODES) {
        return Err(invalid("select between 1 and 10000 nodes"));
    }
    for field in ["tag", "create"] {
        if let Some(def) = v.get(field).filter(|v| v.is_object())
            && (def.get("title").and_then(Value::as_str).is_some_and(|s| s.len() > 4096)
                || def.get("color").and_then(Value::as_str).is_some_and(|s| s.len() > 64))
        {
            return Err(invalid("tag metadata exceeds limit"));
        }
    }
    Ok(())
}
fn edit(changes: Vec<Change>, affected: usize) -> Effect {
    let result = json!({"affected":affected});
    if changes.is_empty() { Effect::None { result } } else { Effect::Edit { changes: Changeset::new(changes), result, gesture: None } }
}
fn put(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    preflight(v)?;
    let p: Put = params(v)?;
    let tag = p.tag.into_tag()?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(missing)?;
    if canvas.tags().get(&tag.id) == Some(&tag) {
        return Ok(edit(vec![], 0));
    }
    Ok(edit(vec![Change::PutTag { canvas: p.canvas, tag }], 1))
}
fn set(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    preflight(v)?;
    // Option<TagId> permits explicit null; omission is deliberately rejected.
    if v.get("tag").is_none() {
        return Err(invalid("tag is required; use null to clear"));
    }
    let p: Set = params(v)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(missing)?;
    let mut changes = Vec::new();
    if let Some(def) = p.create {
        let tag = def.into_tag()?;
        if p.tag != Some(tag.id) {
            return Err(invalid("created tag must match assignment"));
        }
        if canvas.tags().contains_key(&tag.id) {
            return Err(Error::new(ErrorKind::Conflict, "tag already exists"));
        }
        changes.push(Change::PutTag { canvas: p.canvas, tag });
    } else if p.tag.is_some_and(|id| !canvas.tags().contains_key(&id)) {
        return Err(missing());
    }
    let ids: BTreeSet<_> = p.nodes.into_iter().collect();
    let mut assignments = Vec::new();
    for id in ids {
        let node = canvas.nodes().get(&id).ok_or_else(missing)?;
        if node.tag != p.tag {
            assignments.push(NodeTagAssignment { node: id, tag: p.tag });
        }
    }
    let count = assignments.len();
    if !assignments.is_empty() {
        changes.push(Change::SetNodeTags { canvas: p.canvas, assignments });
    }
    Ok(edit(changes, count))
}
fn remove(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    let p: Remove = params(v)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(missing)?;
    if !canvas.tags().contains_key(&p.tag) {
        return Err(missing());
    }
    let assignments: Vec<_> = canvas.nodes().values().filter(|n| n.tag == Some(p.tag)).map(|n| NodeTagAssignment { node: n.id, tag: None }).collect();
    let count = assignments.len();
    let mut changes = Vec::new();
    if !assignments.is_empty() {
        changes.push(Change::SetNodeTags { canvas: p.canvas, assignments });
    }
    changes.push(Change::RemoveTag { canvas: p.canvas, id: p.tag });
    Ok(edit(changes, count))
}
fn enabled(c: &Context<'_>, v: &Value, run: fn(&Context<'_>, &Value) -> Result<Effect, Error>) -> Result<(), Error> {
    if let Effect::Edit { changes, .. } = run(c, v)? {
        c.preview_changes(&changes)?;
    }
    Ok(())
}
fn definition_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{"id":{"type":"string","format":"uuid"},"title":{"type":"string","maxLength":4096,"description":"At most 4096 UTF-8 bytes"},"color":{"type":["string","null"],"maxLength":64,"description":"Optional palette identifier, at most 64 UTF-8 bytes; unknown identifiers are preserved"}},"required":["id","title"]})
}
fn schema(kind: &str) -> Value {
    let mut properties = json!({"canvas":{"type":"string","format":"uuid"},"tag":{"type":"string","format":"uuid"}});
    let required = if kind == "put" {
        properties["tag"] = definition_schema();
        vec!["canvas", "tag"]
    } else if kind == "set" {
        properties["tag"] = json!({"type":["string","null"],"format":"uuid"});
        properties["nodes"] = json!({"type":"array","minItems":1,"maxItems":10000,"items":{"type":"string","format":"uuid"}});
        properties["create"] = definition_schema();
        vec!["canvas", "nodes", "tag"]
    } else {
        vec!["canvas", "tag"]
    };
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":properties,"required":required})
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "tag.put",
            label: "Create or edit tag",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || schema("put"),
            params_doc: "{canvas,tag:{id,title,color?}}; bounded tag metadata; unknown color identifiers survive.",
            enabled: |c, v| enabled(c, v, put),
            run: put,
        },
        CommandSpec {
            id: "node.tag_set",
            label: "Set node tags",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || schema("set"),
            params_doc: "{canvas,nodes,tag,create?}; tag is required and nullable; up to 10000 nodes, one undo step including optional creation.",
            enabled: |c, v| enabled(c, v, set),
            run: set,
        },
        CommandSpec {
            id: "tag.remove",
            label: "Remove tag",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || schema("remove"),
            params_doc: "{canvas,tag}; atomically clear node references and remove the definition.",
            enabled: |c, v| enabled(c, v, remove),
            run: remove,
        },
    ]
}

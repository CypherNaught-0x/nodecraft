//! View-only tag filtering through a scoped live canvas host.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_types::{CanvasId, Error, ErrorKind, TagId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
pub const SERVICE: &str = "canvas.tag_filter";
#[derive(Clone, Debug)]
pub struct TagFilterContext {
    pub canvas: CanvasId,
    pub viewport_available: bool,
    pub blocked: bool,
    pub open: bool,
    pub active: bool,
    pub scanning: bool,
    pub matches: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    tags: Vec<TagId>,
    untagged: bool,
}
fn available<'a>(c: &'a Context<'_>) -> Result<&'a TagFilterContext, Error> {
    let h = c.resources.require::<TagFilterContext>()?;
    if !c.services.contains(SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "live tag filter host is unavailable"));
    }
    if !c.document.canvases().contains_key(&h.canvas) {
        return Err(Error::new(ErrorKind::NotFound, "active canvas does not exist"));
    }
    if !h.viewport_available || h.blocked {
        return Err(Error::new(ErrorKind::Conflict, "finish the active editor, popup or gesture before filtering"));
    }
    Ok(h)
}
fn action(c: &Context<'_>, v: &Value, action: &str) -> Result<Effect, Error> {
    let _: Empty = params(v)?;
    let h = available(c)?;
    if action != "open" && !h.open {
        return Err(Error::new(ErrorKind::Conflict, "tag filter is closed"));
    }
    if matches!(action, "next" | "previous") && (!h.active || (!h.scanning && h.matches == 0)) {
        return Err(Error::new(ErrorKind::Conflict, "tag filter has no matches"));
    }
    Ok(Effect::Service { capability: SERVICE, request: json!({"action":action}) })
}
fn set(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    if v.get("tags").and_then(Value::as_array).is_some_and(|a| a.len() > 10000) {
        return Err(invalid("tag filter exceeds tag limit"));
    }
    let p: Set = params(v)?;
    let h = available(c)?;
    let canvas = c.document.canvases().get(&h.canvas).ok_or_else(|| invalid("active canvas unavailable"))?;
    let ids: BTreeSet<_> = p.tags.into_iter().collect();
    if ids.iter().any(|id| !canvas.tags().contains_key(id)) {
        return Err(Error::new(ErrorKind::NotFound, "filter tag does not exist"));
    }
    Ok(Effect::Service { capability: SERVICE, request: json!({"action":"set","tags":ids,"untagged":p.untagged}) })
}
fn empty_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"required":[],"additionalProperties":false})
}
fn set_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"tags":{"type":"array","maxItems":10000,"items":{"type":"string","format":"uuid"}},"untagged":{"type":"boolean"}},"required":["tags","untagged"],"additionalProperties":false})
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "canvas.tag_filter_open",
            label: "Open tag filter",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{}; open live tag filter without blurring unrelated editors.",
            enabled: |c, v| action(c, v, "open").map(|_| ()),
            run: |c, v| action(c, v, "open"),
        },
        CommandSpec {
            id: "canvas.tag_filter_close",
            label: "Close tag filter",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{}; retain highlights and current selection.",
            enabled: |c, v| action(c, v, "close").map(|_| ()),
            run: |c, v| action(c, v, "close"),
        },
        CommandSpec {
            id: "canvas.tag_filter_next",
            label: "Next tagged node",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{}; wrap in NodeId order after current scan completes.",
            enabled: |c, v| action(c, v, "next").map(|_| ()),
            run: |c, v| action(c, v, "next"),
        },
        CommandSpec {
            id: "canvas.tag_filter_previous",
            label: "Previous tagged node",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{}; wrap in NodeId order after current scan completes.",
            enabled: |c, v| action(c, v, "previous").map(|_| ()),
            run: |c, v| action(c, v, "previous"),
        },
        CommandSpec {
            id: "canvas.tag_filter_set",
            label: "Set tag filter",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: set_schema,
            params_doc: "{tags:[TagId],untagged:boolean}; OR matches; empty plus false clears; no document mutation.",
            enabled: |c, v| set(c, v).map(|_| ()),
            run: set,
        },
    ]
}

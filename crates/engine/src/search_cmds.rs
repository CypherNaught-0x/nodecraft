//! Canvas search view commands, backed by a fresh live host capability.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_types::{CanvasId, Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};
pub const SERVICE: &str = "canvas.search";
pub const MAX_QUERY_BYTES: usize = 1024;
#[derive(Clone, Debug)]
pub struct SearchContext {
    pub canvas: CanvasId,
    pub viewport_available: bool,
    pub blocked: bool,
    pub open: bool,
    pub has_query: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    query: String,
}
fn available<'a>(c: &'a Context<'_>) -> Result<&'a SearchContext, Error> {
    let host = c.resources.require::<SearchContext>()?;
    if !c.services.contains(SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "live canvas search host is unavailable"));
    }
    if !c.document.canvases().contains_key(&host.canvas) {
        return Err(Error::new(ErrorKind::NotFound, "active canvas does not exist"));
    }
    if !host.viewport_available {
        return Err(Error::new(ErrorKind::Conflict, "canvas viewport is unavailable"));
    }
    if host.blocked {
        return Err(Error::new(ErrorKind::Conflict, "finish the active editor, popup or gesture before searching"));
    }
    Ok(host)
}
fn action(c: &Context<'_>, v: &Value, action: &str) -> Result<Effect, Error> {
    let _: Empty = params(v)?;
    let host = available(c)?;
    if action != "open" && !host.open {
        return Err(Error::new(ErrorKind::Conflict, "canvas search is closed"));
    }
    if matches!(action, "next" | "previous") && !host.has_query {
        return Err(Error::new(ErrorKind::Conflict, "enter a valid nonempty search query"));
    }
    Ok(Effect::Service { capability: SERVICE, request: json!({"action":action}) })
}
fn query(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    if v.get("query").and_then(Value::as_str).is_some_and(|query| query.len() > MAX_QUERY_BYTES) {
        return Err(Error::new(ErrorKind::InvalidInput, "search query exceeds 1024 UTF-8 bytes"));
    }
    let p: Query = params(v)?;
    let host = available(c)?;
    if !host.open {
        return Err(Error::new(ErrorKind::Conflict, "canvas search is closed"));
    }
    if p.query.len() > MAX_QUERY_BYTES {
        return Err(Error::new(ErrorKind::InvalidInput, "search query exceeds 1024 UTF-8 bytes"));
    }
    Ok(Effect::Service { capability: SERVICE, request: json!({"action":"query","query":p.query}) })
}
fn empty_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"required":[],"additionalProperties":false})
}
fn query_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"query":{"type":"string","maxLength":1024,"description":"At most 1024 UTF-8 bytes; Unicode whitespace is trimmed for literal lowercase matching"}},"required":["query"],"additionalProperties":false})
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "canvas.search_open",
            label: "Search canvas",
            shortcut: Some("Command+F"),
            alternate_shortcuts: &["Ctrl+F"],
            input_schema: empty_schema,
            params_doc: "{}; open/focus live canvas search without blurring unrelated editors.",
            enabled: |c, v| action(c, v, "open").map(|_| ()),
            run: |c, v| action(c, v, "open"),
        },
        CommandSpec {
            id: "canvas.search_query",
            label: "Set canvas search query",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: query_schema,
            params_doc: "{query:string}; bounded literal Unicode-lowercase search over persisted text; scan incrementally.",
            enabled: |c, v| query(c, v).map(|_| ()),
            run: query,
        },
        CommandSpec {
            id: "canvas.search_next",
            label: "Next canvas search result",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{}; next result with wrapping; Enter applies only in the search field; one pending request waits for the current scan generation; a target in a collapsed field expands it (one field.update undo step).",
            enabled: |c, v| action(c, v, "next").map(|_| ()),
            run: |c, v| action(c, v, "next"),
        },
        CommandSpec {
            id: "canvas.search_previous",
            label: "Previous canvas search result",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{}; previous result with wrapping after the current scan completes; Shift+Enter applies only in the search field; a target in a collapsed field expands it (one field.update undo step).",
            enabled: |c, v| action(c, v, "previous").map(|_| ()),
            run: |c, v| action(c, v, "previous"),
        },
        CommandSpec {
            id: "canvas.search_close",
            label: "Close canvas search",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{}; close live search while retaining query and current selection; Escape applies only while search owns input.",
            enabled: |c, v| action(c, v, "close").map(|_| ()),
            run: |c, v| action(c, v, "close"),
        },
    ]
}
#[cfg(test)]
mod tests {
    #[test]
    fn field_only_keys_are_not_global_shortcuts() {
        for spec in super::specs() {
            if spec.id != "canvas.search_open" {
                assert!(spec.shortcut.is_none());
                assert!(spec.alternate_shortcuts.is_empty());
            }
        }
    }
}

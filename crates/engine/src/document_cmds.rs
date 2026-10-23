//! The PDF reader's edits (#590). `document.use_pages` stores a page range on a PDF to images or
//! PDF to text node as one undo step, so its results go stale like any parameter change.
//! [`extract_page`] copies one page of a PDF into a new, independent PDF node, and
//! [`extract_text`] puts the text a page (or a region of it) shows into a new Text node. Their
//! commands describe a typed media effect; Session reads and stages the media only at dispatch,
//! then publishes one changeset. The reader's page, zoom and scroll remain view state.
use crate::Session;
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_graph::{Change, Changeset, Position};
use nodecraft_media::PageRegion;
use nodecraft_nodes::document::{MAX_PAGE, PAGES, TEXT};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Unsigned, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};

mod extract;
pub(crate) use extract::Produced;
pub use extract::Request as ExtractionRequest;

/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;
/// Most text one extraction puts in a Text node (a command's strings share a 1 MiB budget).
pub const MAX_EXTRACT_TEXT_BYTES: usize = 960 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsePages {
    canvas: CanvasId,
    node: NodeId,
    /// From 1.
    first: u64,
    /// From 1; 0 is the document's last page.
    last: u64,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn whole(value: u64) -> Result<Value, Error> {
    Ok(Value::Unsigned(Unsigned(value)))
}

fn use_pages(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: UsePages = params(value)?;
    if !(1..=MAX_PAGE).contains(&p.first) || p.last > MAX_PAGE {
        return Err(invalid(format!("pages are numbered 1 to {MAX_PAGE}")));
    }
    if p.last != 0 && p.last < p.first {
        return Err(invalid(format!("the last page ({}) comes before the first ({})", p.last, p.first)));
    }
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let node = canvas.nodes().get(&p.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    if node.kind != PAGES && node.kind != TEXT {
        return Err(invalid("only PDF to images and PDF to text nodes take a page range"));
    }
    let changes = Changeset::new(vec![
        Change::SetParam { canvas: p.canvas, id: p.node, name: "first".into(), value: Some(whole(p.first)?) },
        Change::SetParam { canvas: p.canvas, id: p.node, name: "last".into(), value: Some(whole(p.last)?) },
    ]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: Json::Null, gesture: None })
}

fn enabled(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    use_pages(c, value).map(|_| ())
}

fn schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "node", "first", "last"],
        "properties": {"canvas": uuid, "node": uuid,
            "first": {"type": "integer", "minimum": 1, "maximum": MAX_PAGE},
            "last": {"type": "integer", "minimum": 0, "maximum": MAX_PAGE, "description": "0 is the document's last page."}}})
}

pub fn specs() -> Vec<CommandSpec> {
    let mut specs = vec![CommandSpec {
        id: "document.use_pages",
        label: "Use these PDF pages",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, node, first, last} -> null; sets the page range (from 1; last 0 = the end) a PDF to images or PDF to text node reads, as one undo step. Its results go stale until it runs again.",
        enabled,
        run: use_pages,
    }];
    specs.extend(extract::specs());
    specs
}

/// Where the reader's extractions land: the PDF node and page (from 0), the new node's id and
/// its position.
pub struct Extract {
    pub canvas: CanvasId,
    pub node: NodeId,
    pub page: u32,
    pub id: NodeId,
    pub at: Position,
}

/// Copies page `extract.page` into a PDF of its own, added as a new PDF node in one undo step.
/// The source PDF is not changed. Compatibility adapter: this function keeps its zero-based
/// page index, while the command's `page` is numbered from one.
pub fn extract_page(session: &mut Session, extract: &Extract) -> Result<NodeId, Error> {
    session.dispatch(
        "document.extract_page",
        &json!({"canvas":extract.canvas,"node":extract.node,
        "page":u64::from(extract.page)+1,"id":extract.id,"at":extract.at}),
    )?;
    Ok(extract.id)
}

/// The text page `extract.page` shows (inside `region` when given, as fractions of the page:
/// left, top, right, bottom), added as a new Text node in one undo step.
pub fn extract_text(session: &mut Session, extract: &Extract, region: Option<PageRegion>) -> Result<NodeId, Error> {
    // JSON has no NaN/Infinity: validate before serde_json could turn one into null.
    extract::validate_region(region)?;
    session.dispatch(
        "document.extract_text",
        &json!({"canvas":extract.canvas,"node":extract.node,
        "page":u64::from(extract.page)+1,"id":extract.id,"at":extract.at,"region":region}),
    )?;
    Ok(extract.id)
}

#[cfg(test)]
mod tests;

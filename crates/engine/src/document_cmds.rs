//! The PDF reader's edits (#590). `document.use_pages` stores a page range on a PDF to images or
//! PDF to text node as one undo step, so its results go stale like any parameter change.
//! [`extract_page`] copies one page of a PDF into a new, independent PDF node, and
//! [`extract_text`] puts the text a page (or a region of it) shows into a new Text node; both
//! read the project's media, so they are session functions as `split_grid` is, each one undo
//! step. The reader's page, zoom and scroll are view state and never reach the document.
use crate::Session;
use crate::commands::{CommandSpec, Context, Effect, params};
use crate::import::{Incoming, import_files};
use nodecraft_format::MAX_ASSET_BYTES;
use nodecraft_format::asset_source::{StreamMetrics, TransferCancel};
use nodecraft_graph::{Change, Changeset, Node, Position};
use nodecraft_media::{Document, DocumentLimits, PageRegion};
use nodecraft_nodes::document::{MAX_PAGE, PAGES, SOURCE, TEXT};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ParamValues, Unsigned, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

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
    vec![CommandSpec {
        id: "document.use_pages",
        label: "Use these PDF pages",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, node, first, last} -> null; sets the page range (from 1; last 0 = the end) a PDF to images or PDF to text node reads, as one undo step. Its results go stale until it runs again.",
        enabled,
        run: use_pages,
    }]
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

/// The PDF a node holds: a PDF source's file.
fn document_of(session: &Session, extract: &Extract) -> Result<Document, Error> {
    if !(extract.at.x.is_finite() && extract.at.y.is_finite() && extract.at.x.abs() <= MAX_COORDINATE && extract.at.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    let canvas = session.document().canvases().get(&extract.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let node: &Node = canvas.nodes().get(&extract.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    if canvas.nodes().contains_key(&extract.id) {
        return Err(Error::new(ErrorKind::Conflict, "the new node's id already exists"));
    }
    let asset = match (node.kind.as_str(), node.params.get("asset")) {
        (SOURCE, Some(Value::Media(asset))) => asset,
        _ => return Err(invalid("this node has no PDF")),
    };
    let verified = session.assets().open_verified(asset, &TransferCancel::default(), &StreamMetrics::default())?;
    let bytes = verified.read_small(MAX_ASSET_BYTES, &StreamMetrics::default())?;
    let document = Document::open(&bytes, &DocumentLimits::default()).map_err(|error| invalid(format!("the PDF can't be read: {error}")))?;
    let pages = document.page_count();
    if extract.page >= pages {
        return Err(invalid(format!("the PDF has {pages} page{}; there is no page {}", if pages == 1 { "" } else { "s" }, u64::from(extract.page) + 1)));
    }
    Ok(document)
}

/// Copies page `extract.page` into a PDF of its own, added as a new PDF node in one undo step.
/// The source PDF is not changed.
pub fn extract_page(session: &mut Session, extract: &Extract) -> Result<NodeId, Error> {
    let bytes = {
        let document = document_of(session, extract)?;
        let number = u64::from(extract.page) + 1;
        crate::session::guard(|| document.extract_page(extract.page).map_err(|error| invalid(format!("page {number} can't be copied: {error}"))))?
    };
    let name = format!("page {}.pdf", u64::from(extract.page) + 1);
    let imported = import_files(session, extract.canvas, extract.at, 0.0, &[Incoming { name: &name, bytes: &bytes }], &[extract.id])?;
    match (imported.nodes.first(), imported.notice()) {
        (Some(id), _) => Ok(*id),
        (None, notice) => Err(invalid(notice.unwrap_or_else(|| "the page wasn't added".into()))),
    }
}

/// The text page `extract.page` shows (inside `region` when given, as fractions of the page:
/// left, top, right, bottom), added as a new Text node in one undo step.
pub fn extract_text(session: &mut Session, extract: &Extract, region: Option<PageRegion>) -> Result<NodeId, Error> {
    let text = {
        let document = document_of(session, extract)?;
        let number = u64::from(extract.page) + 1;
        crate::session::guard(|| document.reader().text_in(extract.page, region).map_err(|error| invalid(format!("page {number} can't be read: {error}"))))?
    };
    if text.trim().is_empty() {
        return Err(invalid(if region.is_some() { "there is no text there" } else { "this page has no text (a scan has none; there is no OCR)" }));
    }
    let mut cut = text.len().min(MAX_EXTRACT_TEXT_BYTES);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let text = text.get(..cut).unwrap_or_default().to_owned();
    let supplied: ParamValues = [("text".to_owned(), Value::Text(text))].into_iter().collect();
    let mut node = session.kinds().get("source.text")?.instantiate_instance(extract.id, BTreeMap::new(), &supplied)?;
    node.pos = extract.at;
    session.dispatch("node.add", &json!({"canvas": extract.canvas, "node": node}))?;
    Ok(extract.id)
}

#[cfg(test)]
mod tests;

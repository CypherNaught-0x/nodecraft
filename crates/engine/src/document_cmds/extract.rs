//! Pure command admission and bounded PDF work, separated by a typed Session effect.
use super::{Extract, MAX_COORDINATE, MAX_EXTRACT_TEXT_BYTES, invalid};
use crate::Session;
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_format::{
    AssetStore, MAX_ASSET_BYTES,
    asset_source::{StreamMetrics, TransferCancel},
};
use nodecraft_graph::{Position, Project};
use nodecraft_media::{Document, DocumentLimits, PageRegion};
use nodecraft_nodes::document::{MAX_PAGE, SOURCE};
use nodecraft_types::{Blake3, CanvasId, Error, ErrorKind, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    canvas: CanvasId,
    node: NodeId,
    page: u64,
    id: Option<NodeId>,
    at: Option<At>,
    region: Option<PageRegion>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct At {
    x: f32,
    y: f32,
}

/// Opaque, validated description. Only actual dispatch may read project media or mint an ID.
pub struct Request {
    params: Params,
    at: Position,
    text: bool,
}

pub(crate) enum Produced {
    Page { extract: Extract, bytes: Vec<u8> },
    Text { extract: Extract, text: String },
}

pub(super) fn validate_region(region: Option<PageRegion>) -> Result<(), Error> {
    if let Some([left, top, right, bottom]) = region
        && (!region.into_iter().flatten().all(|n| n.is_finite() && (0.0..=1.0).contains(&n)) || left >= right || top >= bottom)
    {
        return Err(invalid("region must be [left, top, right, bottom], finite fractions within the page with positive width and height"));
    }
    Ok(())
}

fn request(c: &Context<'_>, value: &Json, text: bool) -> Result<Effect, Error> {
    Request::check_access(c)?;
    let p: Params = params(value)?;
    if !text && value.get("region").is_some() {
        return Err(invalid("a page extraction does not take a region"));
    }
    if !(1..=MAX_PAGE).contains(&p.page) {
        return Err(invalid(format!("pages are numbered 1 to {MAX_PAGE}")));
    }
    validate_region(p.region)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let source = canvas.nodes().get(&p.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    if p.id.is_some_and(|id| canvas.nodes().contains_key(&id)) {
        return Err(Error::new(ErrorKind::Conflict, "the new node's id already exists"));
    }
    let asset = match (source.kind.as_str(), source.params.get("asset")) {
        (SOURCE, Some(Value::Media(asset))) if asset.mime.as_str() == "application/pdf" => asset,
        _ => return Err(invalid("this node has no PDF")),
    };
    bounded_asset(c.assets, &asset.hash)?;
    if let Some(pages) = asset.meta.page_count
        && p.page > u64::from(pages)
    {
        return Err(no_page(pages, p.page));
    }
    // A detached document coordinate, independent of the shell's current pan/zoom.
    let at = p.at.as_ref().map_or(Position { x: (source.pos.x + 300.0).min(MAX_COORDINATE), y: source.pos.y }, |at| Position { x: at.x, y: at.y });
    if !(at.x.is_finite() && at.y.is_finite() && at.x.abs() <= MAX_COORDINATE && at.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    Ok(Effect::DocumentExtract { request: Box::new(Request { params: p, at, text }) })
}

fn no_page(pages: u32, page: u64) -> Error {
    invalid(format!("the PDF has {pages} page{}; there is no page {page}", if pages == 1 { "" } else { "s" }))
}

fn bounded_asset(assets: &AssetStore, hash: &Blake3) -> Result<(), Error> {
    let descriptor = assets.descriptor(hash).ok_or_else(|| invalid("the PDF's media is missing"))?;
    if descriptor.bytes > MAX_ASSET_BYTES as u64 {
        return Err(invalid("the PDF exceeds the 16 MiB extraction limit"));
    }
    Ok(())
}

fn fresh_id(project: &Project) -> Result<NodeId, Error> {
    for _ in 0..32 {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| Error::new(ErrorKind::Internal, "couldn't allocate a new node id"))?;
        if let Some(byte) = bytes.get_mut(6) {
            *byte = (*byte & 0x0f) | 0x40;
        }
        if let Some(byte) = bytes.get_mut(8) {
            *byte = (*byte & 0x3f) | 0x80;
        }
        let id = NodeId::from_bytes(bytes);
        if project.canvases().values().all(|canvas| !canvas.nodes().contains_key(&id)) {
            return Ok(id);
        }
    }
    Err(Error::new(ErrorKind::Conflict, "couldn't allocate a unique node id; supply an explicit id"))
}

impl Request {
    pub(crate) fn check_access(c: &Context<'_>) -> Result<(), Error> {
        c.editable()?;
        if !c.access.role.edits() {
            return Err(Error::new(ErrorKind::Forbidden, "this share link doesn't let you add extracted PDF content"));
        }
        Ok(())
    }

    pub(crate) fn produce(self, session: &Session) -> Result<Produced, Error> {
        let p = self.params;
        let canvas = session.document().canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
        let source = canvas.nodes().get(&p.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
        let id = match p.id {
            Some(id) => id,
            None => fresh_id(session.document())?,
        };
        if canvas.nodes().contains_key(&id) {
            return Err(Error::new(ErrorKind::Conflict, "the new node's id already exists"));
        }
        let asset = match (source.kind.as_str(), source.params.get("asset")) {
            (SOURCE, Some(Value::Media(asset))) if asset.mime.as_str() == "application/pdf" => asset,
            _ => return Err(invalid("this node has no PDF")),
        };
        let metrics = StreamMetrics::default();
        // Refuse by its logical descriptor before verification could stream a large asset.
        bounded_asset(session.assets(), &asset.hash)?;
        let verified = session.assets().open_verified(asset, &TransferCancel::default(), &metrics)?;
        let bytes = verified.read_small(MAX_ASSET_BYTES, &metrics)?;
        let document = Document::open(&bytes, &DocumentLimits::default()).map_err(|e| invalid(format!("the PDF can't be read: {e}")))?;
        let page = u32::try_from(p.page.saturating_sub(1)).map_err(|_| invalid("page is too large"))?;
        if page >= document.page_count() {
            return Err(no_page(document.page_count(), p.page));
        }
        let extract = Extract { canvas: p.canvas, node: p.node, page, id, at: self.at };
        if !self.text {
            let bytes = document.extract_page(page).map_err(|e| invalid(format!("page {} can't be copied: {e}", p.page)))?;
            if bytes.len() > MAX_ASSET_BYTES {
                return Err(invalid("the extracted page exceeds the 16 MiB asset limit"));
            }
            return Ok(Produced::Page { extract, bytes });
        }
        let text = document.reader().text_in(page, p.region).map_err(|e| invalid(format!("page {} can't be read: {e}", p.page)))?;
        if text.trim().is_empty() {
            return Err(invalid(if p.region.is_some() { "there is no text there" } else { "this page has no text (a scan has none; there is no OCR)" }));
        }
        let mut cut = text.len().min(MAX_EXTRACT_TEXT_BYTES);
        while cut > 0 && !text.is_char_boundary(cut) {
            cut = cut.saturating_sub(1);
        }
        Ok(Produced::Text { extract, text: text.get(..cut).unwrap_or_default().to_owned() })
    }
}

fn page(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    request(c, value, false)
}
fn text(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    request(c, value, true)
}
fn enabled_page(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    page(c, value).map(|_| ())
}
fn enabled_text(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    text(c, value).map(|_| ())
}

fn schema(text: bool) -> Json {
    let uuid = json!({"type":"string","format":"uuid"});
    let mut schema = json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,
        "required":["canvas","node","page"],"properties":{"canvas":uuid,"node":uuid,"page":{"type":"integer","minimum":1,"maximum":MAX_PAGE},"id":uuid,
        "at":{"type":"object","additionalProperties":false,"required":["x","y"],"properties":{
            "x":{"type":"number","minimum":-MAX_COORDINATE,"maximum":MAX_COORDINATE},"y":{"type":"number","minimum":-MAX_COORDINATE,"maximum":MAX_COORDINATE}}}}});
    if text && let Some(properties) = schema.get_mut("properties").and_then(Json::as_object_mut) {
        properties.insert("region".into(), json!({"anyOf":[{"type":"null"},{"type":"array","minItems":4,"maxItems":4,
            "items":{"type":"number","minimum":0,"maximum":1}}],"description":"left, top, right, bottom as fractions of the page; positive width and height required"}));
    }
    schema
}
fn page_schema() -> Json {
    schema(false)
}
fn text_schema() -> Json {
    schema(true)
}

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "document.extract_page",
            label: "Extract PDF page",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: page_schema,
            params_doc: "{canvas, node, page, id?, at?: {x,y}} -> {id}; copies one PDF page (numbered from 1) to a new PDF source in one undo step. Omitted id is generated; omitted at places it beside the source. No dialog.",
            enabled: enabled_page,
            run: page,
        },
        CommandSpec {
            id: "document.extract_text",
            label: "Extract PDF text",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: text_schema,
            params_doc: "{canvas, node, page, region?: [left,top,right,bottom], id?, at?: {x,y}} -> {id}; reads a PDF page's text, optionally within a normalized region, into a new Text source in one undo step. Page is numbered from 1. Omitted id is generated; omitted at places it beside the source. No dialog or OCR.",
            enabled: enabled_text,
            run: text,
        },
    ]
}

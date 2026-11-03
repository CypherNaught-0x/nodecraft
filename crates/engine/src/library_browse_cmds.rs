//! The Library's browsers (#96): read-only listings, a page at a time, of the project's assets
//! (`library.assets`), its past generations (`library.history`) and its trained styles and Soul
//! IDs (`library.styles`), across every canvas of the open project. None of them changes the
//! document. What they list is used through the commands that already do it: a listed asset
//! becomes a source card with `node.create` and `node.param_set` (or `node.add` holding the
//! instantiated card), a past result is restored with `history.select` or placed with
//! `history.extract`, and a style is wired into a card with `style.use`.
//!
//! Browsing other projects needs a store of them, which NodeCraft doesn't keep yet: the
//! listings cover the open project only.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::history_cmds::list_items;
use nodecraft_graph::{Canvas, Generation, Node, Project};
use nodecraft_nodes::generation::{MODEL, STYLE_KIND};
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, GenerationId, Mime, Modality, NodeId, STYLE_MIME, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::{HashMap, HashSet};

/// How many entries a page holds when the caller doesn't say.
pub const DEFAULT_PAGE: usize = 50;
/// The most entries one page may hold.
pub const MAX_PAGE: usize = 200;
/// The most bytes of a search query.
pub const MAX_QUERY_BYTES: usize = 256;
/// How many characters of a text result a history entry quotes.
pub const TEXT_PREVIEW_CHARS: usize = 120;
/// How deep nested lists in a parameter are searched for media.
const MAX_LIST_DEPTH: usize = 4;
/// The style family of Higgsfield Soul IDs (`docs/decisions/2026-10-22-soul-id-training.md`).
pub const SOUL_ID_FAMILY: &str = "soul-2";

/// The kinds of asset the Assets browser lists. Text values and trained styles are listed by
/// the History and Styles browsers instead.
pub const ASSET_KINDS: [Modality; 5] = [Modality::Image, Modality::Video, Modality::Audio, Modality::Mesh, Modality::Document];

/// The modality a MIME type names, when it is one NodeCraft keeps as media.
pub fn kind_of(mime: &Mime) -> Option<Modality> {
    let essence = mime.essence();
    if essence == STYLE_MIME {
        Some(Modality::Style)
    } else if essence.starts_with("image/") {
        Some(Modality::Image)
    } else if essence.starts_with("video/") {
        Some(Modality::Video)
    } else if essence.starts_with("audio/") {
        Some(Modality::Audio)
    } else if essence.starts_with("model/") {
        Some(Modality::Mesh)
    } else if essence == "application/pdf" {
        Some(Modality::Document)
    } else if essence.starts_with("text/") {
        Some(Modality::Text)
    } else {
        None
    }
}

/// The source card that holds an asset of `kind`, and the parameter it goes in.
pub fn source_for(kind: Modality) -> Option<(&'static str, &'static str)> {
    crate::history_cmds::source_kind(kind).ok()
}

/// A page request: where it starts and how many it holds.
fn page(offset: usize, limit: Option<usize>) -> Result<(usize, usize), Error> {
    let limit = limit.unwrap_or(DEFAULT_PAGE);
    if limit == 0 || limit > MAX_PAGE {
        return Err(invalid("limit must be between 1 and 200"));
    }
    Ok((offset, limit))
}

/// A search query, lowercased; `None` when it is empty.
fn query(query: Option<String>) -> Result<Option<String>, Error> {
    match query {
        Some(q) if q.len() > MAX_QUERY_BYTES => Err(invalid("the search is too long (at most 256 bytes)")),
        Some(q) if q.trim().is_empty() => Ok(None),
        Some(q) => Ok(Some(q.trim().to_lowercase())),
        None => Ok(None),
    }
}

fn matches(query: Option<&str>, fields: &[&str]) -> bool {
    query.is_none_or(|q| fields.iter().any(|field| field.to_lowercase().contains(q)))
}

/// The canvases to read, by title then id; only `only` when given (an unknown one is refused).
fn canvases(document: &Project, only: Option<CanvasId>) -> Result<Vec<&Canvas>, Error> {
    if let Some(id) = only {
        return document.canvases().get(&id).map(|canvas| vec![canvas]).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"));
    }
    let mut all: Vec<&Canvas> = document.canvases().values().collect();
    all.sort_by(|a, b| (a.title(), a.id()).cmp(&(b.title(), b.id())));
    Ok(all)
}

/// How a card is named in a listing: its title, else its kind.
fn label(node: &Node) -> &str {
    if node.title.trim().is_empty() { &node.kind } else { &node.title }
}

/// A short quote of `text`, cut at a character boundary.
fn quote(text: &str) -> String {
    let mut chars = text.chars();
    let mut quoted: String = chars.by_ref().take(TEXT_PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        quoted.push('…');
    }
    quoted
}

/// Where an asset was found: a card's parameter, or one value of one of its results.
#[derive(Clone, Copy)]
struct Found<'a> {
    asset: &'a AssetRef,
    kind: Modality,
    canvas: &'a Canvas,
    node: &'a Node,
    /// The parameter holding it, for an asset a card was given.
    param: Option<&'a str>,
    /// The result holding it, for an asset a card made.
    generation: Option<GenerationId>,
    port: Option<&'a str>,
    item: Option<usize>,
}

/// The media in a parameter value, through at most [`MAX_LIST_DEPTH`] nested lists.
fn media_in<'a>(value: &'a Value, depth: usize, out: &mut Vec<&'a AssetRef>) {
    match value {
        Value::Media(asset) => out.push(asset),
        Value::List(items) if depth < MAX_LIST_DEPTH => items.iter().for_each(|item| media_in(item, depth.saturating_add(1), out)),
        _ => {}
    }
}

/// Every media value one result holds, with the output port and list item it is at. A result
/// recorded for another port layout has no port names; its media are still listed.
fn result_media<'a>(node: &'a Node, generation: &'a Generation, mut each: impl FnMut(&'a AssetRef, Option<&'a nodecraft_graph::Port>, Option<usize>)) {
    let positional = generation.outputs.len() == node.outputs.len();
    for (index, output) in generation.outputs.iter().enumerate() {
        let port = if positional { node.outputs.get(index) } else { None };
        match output {
            Value::Media(asset) => each(asset, port, None),
            Value::List(items) => {
                for (item, value) in list_items(items, !generation.items.is_empty()).enumerate() {
                    if let Value::Media(asset) = value {
                        each(asset, port, Some(item));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Every asset reference on `canvases`, in order: each card's parameters, then its results
/// newest first.
fn every_asset<'a>(canvases: &[&'a Canvas]) -> Vec<Found<'a>> {
    let mut found = Vec::new();
    for canvas in canvases {
        for node in canvas.nodes().values() {
            for (name, value) in &node.params {
                let mut media = Vec::new();
                media_in(value, 0, &mut media);
                for asset in media {
                    if let Some(kind) = kind_of(&asset.mime) {
                        found.push(Found { asset, kind, canvas, node, param: Some(name), generation: None, port: None, item: None });
                    }
                }
            }
            for generation in node.history.iter().rev() {
                result_media(node, generation, |asset, port, item| {
                    // The port's modality wins over the MIME type: it says what the card made. Only a
                    // text port (or a result recorded for other ports) leaves it to the MIME type.
                    let kind = port.map(|p| p.port_type.modality).filter(|m| *m != Modality::Text).or_else(|| kind_of(&asset.mime));
                    if let Some(kind) = kind {
                        found.push(Found {
                            asset,
                            kind,
                            canvas,
                            node,
                            param: None,
                            generation: Some(generation.id),
                            port: port.map(|p| p.name.as_str()),
                            item,
                        });
                    }
                });
            }
        }
    }
    found
}

/// Where the Assets browser lists an asset: the document's own reference to it (never one a
/// caller supplied), its kind, and the card and result it was first found on.
pub(crate) struct Located {
    pub asset: AssetRef,
    pub kind: Modality,
    pub canvas: CanvasId,
    pub node: NodeId,
    pub label: String,
    /// The result holding it, with its 1-based place in the card's history and the history's
    /// length, when it was first found in a result rather than a parameter.
    pub result: Option<(GenerationId, usize, usize)>,
    /// Whether the card it was first found on has a title of its own.
    pub titled: bool,
}

/// Where the open document holds each media hash: the first place, in the Assets browser's
/// order. One walk of the document, however many hashes the caller then looks up.
pub(crate) fn locate_all(document: &Project) -> HashMap<nodecraft_types::Blake3, Located> {
    let mut located = HashMap::new();
    let Ok(canvases) = canvases(document, None) else { return located };
    for found in every_asset(&canvases).into_iter().filter(|f| ASSET_KINDS.contains(&f.kind)) {
        located.entry(found.asset.hash).or_insert_with(|| {
            let result = found.generation.and_then(|id| {
                let index = found.node.history.iter().position(|g| g.id == id)?;
                Some((id, index.saturating_add(1), found.node.history.len()))
            });
            Located {
                asset: found.asset.clone(),
                kind: found.kind,
                canvas: found.canvas.id(),
                node: found.node.id,
                label: label(found.node).to_owned(),
                result,
                titled: !found.node.title.trim().is_empty(),
            }
        });
    }
    located
}

/// Every media hash the open document holds, as the Assets browser would list it.
pub fn listed_hashes(document: &Project) -> HashSet<nodecraft_types::Blake3> {
    let Ok(canvases) = canvases(document, None) else { return HashSet::new() };
    every_asset(&canvases).into_iter().filter(|f| ASSET_KINDS.contains(&f.kind)).map(|f| f.asset.hash).collect()
}

/// A card's name cut to a short quote, for the provenance title of a card placed from the
/// library.
pub(crate) fn short_label(text: &str) -> String {
    let mut chars = text.chars().filter(|c| !c.is_control());
    let mut quoted: String = chars.by_ref().take(80).collect();
    if chars.next().is_some() {
        quoted.push('…');
    }
    quoted
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetParams {
    #[serde(default)]
    kind: Option<Modality>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    canvas: Option<CanvasId>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}

/// The request's checked parts, without listing anything.
fn asset_request(c: &Context<'_>, value: &Json) -> Result<(AssetParams, usize, usize, Option<String>), Error> {
    let mut p: AssetParams = params(value)?;
    if p.kind.is_some_and(|kind| !ASSET_KINDS.contains(&kind)) {
        return Err(invalid("kind must be image, video, audio, mesh or document; trained styles are listed by library.styles"));
    }
    let (offset, limit) = page(p.offset, p.limit)?;
    let query = query(p.query.take())?;
    if let Some(id) = p.canvas
        && !c.document.canvases().contains_key(&id)
    {
        return Err(Error::new(ErrorKind::NotFound, "canvas does not exist"));
    }
    Ok((p, offset, limit, query))
}

fn assets(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let (p, offset, limit, query) = asset_request(c, value)?;
    let canvases = canvases(c.document, p.canvas)?;
    let found = every_asset(&canvases);
    let mut uses: HashMap<_, usize> = HashMap::new();
    for f in &found {
        let count = uses.entry(f.asset.hash).or_default();
        *count = count.saturating_add(1);
    }
    let mut seen = HashSet::new();
    let listed: Vec<Found<'_>> = found
        .into_iter()
        .filter(|f| ASSET_KINDS.contains(&f.kind) && p.kind.is_none_or(|kind| kind == f.kind))
        .filter(|f| {
            matches(query.as_deref(), &[label(f.node), f.canvas.title(), f.asset.mime.essence(), f.param.unwrap_or_default(), f.port.unwrap_or_default()])
        })
        // After the filters, so each asset is listed where it first matches.
        .filter(|f| seen.insert(f.asset.hash))
        .collect();
    let entries: Vec<Json> = listed
        .iter()
        .skip(offset)
        .take(limit)
        .map(|f| {
            let source = source_for(f.kind).map(|(kind, param)| json!({"kind": kind, "param": param}));
            json!({
                "hash": f.asset.hash,
                "mime": f.asset.mime,
                "kind": f.kind,
                "meta": f.asset.meta,
                "bytes": c.assets.descriptor(&f.asset.hash).map(|d| d.bytes),
                "stored": c.assets.contains(&f.asset.hash),
                "label": label(f.node),
                "canvas": f.canvas.id(),
                "canvas_title": f.canvas.title(),
                "node": f.node.id,
                "param": f.param,
                "generation": f.generation,
                "port": f.port,
                "item": f.item,
                "uses": uses.get(&f.asset.hash).copied().unwrap_or(1),
                "source": source,
            })
        })
        .collect();
    Ok(Effect::None { result: json!({"assets": entries, "total": listed.len(), "offset": offset, "limit": limit}) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryParams {
    #[serde(default)]
    canvas: Option<CanvasId>,
    /// One card's results only; needs `canvas`.
    #[serde(default)]
    node: Option<NodeId>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}

/// What a result shows in a listing: its first media value, else a quote of its first text, and
/// where `history.extract` finds that value (`None` when the result was made for other outputs).
fn preview(node: &Node, generation: &Generation) -> (Option<Json>, Option<String>, Option<Json>) {
    let mut media = None;
    let mut place = None;
    result_media(node, generation, |asset, port, item| {
        if media.is_none() {
            let kind = port.map(|p| p.port_type.modality).filter(|m| *m != Modality::Text).or_else(|| kind_of(&asset.mime));
            media = Some(json!({"hash": asset.hash, "mime": asset.mime, "kind": kind, "meta": asset.meta}));
            // history.extract reads the port's modality; a style has no source card to hold it.
            place = port.filter(|p| source_for(p.port_type.modality).is_some()).map(|p| json!({"port": p.name, "item": item}));
        }
    });
    let positional = generation.outputs.len() == node.outputs.len();
    let mut text = None;
    for (index, output) in generation.outputs.iter().enumerate() {
        let found = match output {
            Value::Text(text) => Some((text, None)),
            Value::List(items) => list_items(items, !generation.items.is_empty())
                .enumerate()
                .find_map(|(item, value)| if let Value::Text(text) = value { Some((text, Some(item))) } else { None }),
            _ => None,
        };
        if let Some((found, item)) = found {
            text = Some(quote(found));
            if place.is_none() {
                place = node.outputs.get(index).filter(|_| positional).map(|p| json!({"port": p.name, "item": item}));
            }
            break;
        }
    }
    (media, text, place)
}

/// The request's checked parts, without listing anything.
fn history_request(c: &Context<'_>, value: &Json) -> Result<(HistoryParams, usize, usize, Option<String>), Error> {
    let mut p: HistoryParams = params(value)?;
    let (offset, limit) = page(p.offset, p.limit)?;
    let query = query(p.query.take())?;
    match (p.canvas, p.node) {
        (None, Some(_)) => return Err(invalid("a card's results need its canvas")),
        (Some(id), node) => {
            let canvas = c.document.canvases().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
            if node.is_some_and(|node| !canvas.nodes().contains_key(&node)) {
                return Err(Error::new(ErrorKind::NotFound, "node does not exist"));
            }
        }
        (None, None) => {}
    }
    Ok((p, offset, limit, query))
}

fn history(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let (p, offset, limit, query) = history_request(c, value)?;
    let canvases = canvases(c.document, p.canvas)?;
    let mut listed = Vec::new();
    for canvas in &canvases {
        for node in canvas.nodes().values().filter(|n| p.node.is_none_or(|id| id == n.id)) {
            let count = node.history.len();
            for (index, generation) in node.history.iter().enumerate().rev() {
                let settings = generation.settings.first();
                let model = settings.map(|s| s.model.as_str()).or(match node.params.get(MODEL) {
                    Some(Value::Text(id)) if !id.is_empty() => Some(id.as_str()),
                    _ => None,
                });
                if !matches(query.as_deref(), &[label(node), canvas.title(), model.unwrap_or_default()]) {
                    continue;
                }
                listed.push((*canvas, node, index, count, generation, model, settings.map(|s| s.provider.as_str())));
            }
        }
    }
    let entries: Vec<Json> = listed
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(canvas, node, index, count, generation, model, provider)| {
            let (media, text, place) = preview(node, generation);
            json!({
                "canvas": canvas.id(),
                "canvas_title": canvas.title(),
                "node": node.id,
                "label": label(node),
                "kind": node.kind,
                "generation": generation.id,
                "index": index.saturating_add(1),
                "count": count,
                "active": node.active == Some(generation.id),
                "partial": generation.incomplete.is_some(),
                "model": model,
                "provider": provider,
                "outputs": generation.outputs.len(),
                "media": media,
                "text": text,
                "place": place,
            })
        })
        .collect();
    Ok(Effect::None { result: json!({"generations": entries, "total": listed.len(), "offset": offset, "limit": limit}) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StyleParams {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}

/// The request's checked parts, without listing anything.
fn style_request(value: &Json) -> Result<(usize, usize, Option<String>), Error> {
    let p: StyleParams = params(value)?;
    let (offset, limit) = page(p.offset, p.limit)?;
    Ok((offset, limit, query(p.query)?))
}

fn styles(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let (offset, limit, query) = style_request(value)?;
    let canvases = canvases(c.document, None)?;
    let mut listed = Vec::new();
    for canvas in &canvases {
        for node in canvas.nodes().values().filter(|n| n.kind == STYLE_KIND) {
            let style = crate::style_cmds::active_style(node);
            let family = style.and_then(crate::style_cmds::family);
            // Descriptors are small and resident; one that isn't has no provider or trigger listed.
            let descriptor = style.and_then(|s| c.assets.get(&s.hash)).and_then(|bytes| nodecraft_media::style::decode(bytes).ok());
            let trainer = match node.params.get(MODEL) {
                Some(Value::Text(id)) if !id.is_empty() => Some(id.as_str()),
                _ => None,
            };
            let trigger = descriptor.as_ref().map(|d| d.trigger.as_str()).unwrap_or_default();
            if !matches(query.as_deref(), &[label(node), canvas.title(), family.unwrap_or_default(), trigger, trainer.unwrap_or_default()]) {
                continue;
            }
            listed.push(json!({
                "canvas": canvas.id(),
                "canvas_title": canvas.title(),
                "id": node.id,
                "title": node.title,
                "trainer": trainer,
                "ready": family.is_some(),
                "family": family,
                "identity": family == Some(SOUL_ID_FAMILY),
                "trigger": descriptor.as_ref().map(|d| d.trigger.clone()),
                // The weights (or the identity) stay with the provider that trained them: the
                // project keeps a handle that only that provider can use.
                "kept_by": descriptor.as_ref().map(|d| d.provider.clone()),
            }));
        }
    }
    let total = listed.len();
    let entries: Vec<Json> = listed.into_iter().skip(offset).take(limit).collect();
    Ok(Effect::None { result: json!({"styles": entries, "total": total, "offset": offset, "limit": limit}) })
}

// Enabled checks only the request, so asking (as the palette does every frame) never walks
// the project.
fn enabled_assets(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    asset_request(c, value).map(|_| ())
}
fn enabled_history(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    history_request(c, value).map(|_| ())
}
fn enabled_styles(_: &Context<'_>, value: &Json) -> Result<(), Error> {
    style_request(value).map(|_| ())
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}
fn paging() -> serde_json::Map<String, Json> {
    let mut properties = serde_json::Map::new();
    properties.insert("query".into(), json!({"type": "string", "maxLength": MAX_QUERY_BYTES}));
    properties.insert("offset".into(), json!({"type": "integer", "minimum": 0}));
    properties.insert("limit".into(), json!({"type": "integer", "minimum": 1, "maximum": MAX_PAGE}));
    properties
}
fn object(properties: serde_json::Map<String, Json>) -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [], "properties": properties})
}
fn assets_schema() -> Json {
    let mut properties = paging();
    properties.insert("kind".into(), json!({"type": "string", "enum": ["image", "video", "audio", "mesh", "document"]}));
    properties.insert("canvas".into(), uuid());
    object(properties)
}
fn history_schema() -> Json {
    let mut properties = paging();
    properties.insert("canvas".into(), uuid());
    properties.insert("node".into(), uuid());
    object(properties)
}
fn styles_schema() -> Json {
    object(paging())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "library.assets",
            label: "Browse assets",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: assets_schema,
            params_doc: "{kind?: image|video|audio|mesh|document, query?, canvas?, offset?: 0, limit?: 1-200 (50)} -> {assets: [{hash, mime, kind, meta, bytes, stored, label, canvas, canvas_title, node, param, generation, port, item, uses, source: {kind, param}}], total, offset, limit}; every distinct image, video, sound, 3D model and PDF the open project's cards hold or made, once each (where it was first found: a card's `param`, or a result's `generation`, `port` and list `item`), on every canvas or only `canvas`. `query` matches card titles, canvas titles, MIME types, parameters and ports. Read-only. Put one on a canvas as a `source.kind` card whose `source.param` is {type: media, value: {hash, mime, meta}} (node.create then node.param_set, or node.add with the instantiated card), or with history.extract when it came from a result.",
            enabled: enabled_assets,
            run: assets,
        },
        CommandSpec {
            id: "library.history",
            label: "Browse generations",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: history_schema,
            params_doc: "{canvas?, node?: NodeId (needs canvas), query?, offset?: 0, limit?: 1-200 (50)} -> {generations: [{canvas, canvas_title, node, label, kind, generation, index, count, active, partial, model, provider, outputs, media, text, place}], total, offset, limit}; the open project's past results, card by card and newest first within a card, on every canvas, one canvas or one card. `index` counts from 1 (oldest) to `count`; `media` is the first media value and `text` quotes the first text; `place` is the {port, item} history.extract takes to place that value (null for a result made for other outputs). `query` matches card titles, canvas titles and models. Read-only. Restore one with history.select; place it as a source card with history.extract.",
            enabled: enabled_history,
            run: history,
        },
        CommandSpec {
            id: "library.styles",
            label: "Browse trained styles",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: styles_schema,
            params_doc: "{query?, offset?: 0, limit?: 1-200 (50)} -> {styles: [{canvas, canvas_title, id, title, trainer, ready, family, identity, trigger, kept_by}], total, offset, limit}; every Train style card on every canvas of the open project. `identity` marks a Soul ID (family soul-2); `kept_by` names the provider that holds the weights or identity: the project keeps only a handle that provider's account can use. Read-only. Wire one into a card on its canvas with style.use; style.list checks it against a card.",
            enabled: enabled_styles,
            run: styles,
        },
    ]
}

#[cfg(test)]
pub(crate) mod tests;

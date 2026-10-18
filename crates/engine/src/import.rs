//! Files from the operating system (#197): dropped on the canvas, or pasted as image data. Each
//! file is recognised from its bytes (never its name), media goes into the project's asset store,
//! and every file that can be held becomes a source node, all added as one undo step through
//! `canvas.paste`. Files that can't are reported with a reason instead of failing the rest.
//!
//! Everything that can refuse a file is checked before any byte is stored: the text a single
//! paste can carry, and the room left in the asset store. If adding the nodes still fails, what
//! was stored for them is pruned, so a drop never leaves media behind.
use crate::Session;
use nodecraft_format::{MAX_ASSET_BYTES, MAX_ASSETS, MAX_TOTAL_ASSET_BYTES};
use nodecraft_graph::{Node, Position};
use nodecraft_media::{Limits, MediaKind, Sniffed, probe_image, sniff};
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, MediaMeta, Mime, NodeId, ParamValues, Value};
use serde_json::json;
use std::collections::BTreeMap;

/// Most files one drop adds; the rest are reported as skipped.
pub const MAX_FILES: usize = 32;
/// Largest media file a project holds (its per-asset cap).
pub const MAX_MEDIA_BYTES: usize = MAX_ASSET_BYTES;
/// Most text one drop adds, across all its files. A paste is one command, and a command's
/// strings share a 1 MiB budget; this leaves room for the ids, kinds and media types beside it.
pub const MAX_DROP_TEXT_BYTES: usize = 960 * 1024;

/// One file to add: a name for messages (never trusted for its type) and its bytes.
pub struct Incoming<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
}

/// What a drop did: the nodes it added, and why each other file was left out.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub nodes: Vec<NodeId>,
    pub skipped: Vec<(String, String)>,
}

impl Imported {
    /// A one-line notice for anything left out, if anything was.
    pub fn notice(&self) -> Option<String> {
        match self.skipped.as_slice() {
            [] => None,
            [(name, why)] => Some(format!("{name} wasn't added: {why}.")),
            [(name, why), rest @ ..] => Some(format!("{name} wasn't added: {why} (and {} more files weren't).", rest.len())),
        }
    }
}

fn too_much_text() -> String {
    format!("a drop adds at most {} KiB of text", MAX_DROP_TEXT_BYTES / 1024)
}

/// Text from a file's bytes, at most `limit` bytes of UTF-8: UTF-8, or UTF-16 with a byte-order
/// mark (an odd trailing byte is refused, not dropped). Control characters anywhere, not only in
/// the first KiB that sniffing looks at, mean the file isn't text.
fn text_of(bytes: &[u8], mime: &str, limit: usize) -> Result<String, String> {
    let utf16 = |big: bool| -> Result<String, String> {
        let (pairs, rest) = bytes.get(2..).unwrap_or(&[]).as_chunks::<2>();
        if !rest.is_empty() {
            return Err("the UTF-16 text ends in half a character".into());
        }
        let mut text = String::new();
        for unit in char::decode_utf16(pairs.iter().map(|pair| if big { u16::from_be_bytes(*pair) } else { u16::from_le_bytes(*pair) })) {
            text.push(unit.map_err(|_| "the text isn't valid UTF-16".to_owned())?);
            if text.len() > limit {
                return Err(too_much_text());
            }
        }
        Ok(text)
    };
    let text = match mime {
        "text/plain; charset=utf-16le" => utf16(false)?,
        "text/plain; charset=utf-16be" => utf16(true)?,
        _ => {
            let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
            if body.len() > limit {
                return Err(too_much_text());
            }
            std::str::from_utf8(body).map_err(|_| "the text isn't valid UTF-8".to_owned())?.to_owned()
        }
    };
    if text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
        return Err("the file has binary data in it, so it isn't text".into());
    }
    Ok(text)
}

/// What one file will become, decided before anything is stored.
enum Plan<'a> {
    Text(String),
    Media { kind: &'static str, mime: Mime, meta: MediaMeta, bytes: &'a [u8] },
}

/// Images NodeCraft can store but not yet decode (common camera formats): kept without a size.
fn decodes_later(sniffed: &Sniffed) -> bool {
    matches!(sniffed.mime, "image/heic" | "image/heif" | "image/avif")
}

/// The plan for one file, given the text still allowed in this drop.
fn plan<'a>(file: &Incoming<'a>, text_left: usize) -> Result<Plan<'a>, String> {
    let Some(sniffed) = sniff(file.bytes) else {
        return Err(if file.bytes.is_empty() { "the file is empty".into() } else { "NodeCraft doesn't recognise this kind of file".into() });
    };
    let kind = match sniffed.kind {
        MediaKind::Text => return text_of(file.bytes, sniffed.mime, text_left).map(Plan::Text),
        MediaKind::Image => "source.image",
        MediaKind::Video => "source.video",
        MediaKind::Audio => "source.audio",
        MediaKind::Model3d | MediaKind::Document => return Err("3D models and documents can't be added yet".into()),
    };
    if file.bytes.len() > MAX_ASSET_BYTES {
        return Err(format!("media over {} MiB can't be added to a project yet", MAX_ASSET_BYTES / (1024 * 1024)));
    }
    let mut meta = MediaMeta::default();
    if sniffed.kind == MediaKind::Image {
        match probe_image(file.bytes, &Limits::default()) {
            Ok(probed) => {
                meta.width = Some(probed.width);
                meta.height = Some(probed.height);
            }
            Err(_) if decodes_later(&sniffed) => {}
            Err(_) => return Err("the image can't be read".into()),
        }
    }
    let mime = sniffed.mime.parse().map_err(|_| "the file type isn't supported".to_owned())?;
    Ok(Plan::Media { kind, mime, meta, bytes: file.bytes })
}

/// Stores planned media and builds the nodes, the first at `at` and the rest `step` apart.
fn build(session: &mut Session, plans: Vec<(NodeId, Plan<'_>)>, at: Position, step: f32) -> Result<Vec<Node>, Error> {
    let mut nodes = Vec::with_capacity(plans.len());
    for (id, plan) in plans {
        let (kind, parameter, value) = match plan {
            Plan::Text(text) => ("source.text", "text", Value::Text(text)),
            Plan::Media { kind, mime, meta, bytes } => {
                let hash = session.import_asset(bytes)?;
                (kind, "asset", Value::Media(AssetRef { hash, mime, meta }))
            }
        };
        let supplied: ParamValues = [(parameter.to_owned(), value)].into_iter().collect();
        let mut node = session.kinds().get(kind)?.instantiate_instance(id, BTreeMap::new(), &supplied)?;
        node.pos = Position { x: at.x + step * nodes.len() as f32, y: at.y };
        nodes.push(node);
    }
    Ok(nodes)
}

/// Adds `files` to `canvas` as source nodes, the first at `at` and the rest in a row `step`
/// apart, as one undo step. `ids` supplies a fresh id per file (the caller's, so replay stays
/// deterministic). `Err` only when adding the nodes fails; files that can't be held are listed
/// in [`Imported::skipped`].
pub fn import_files(session: &mut Session, canvas: CanvasId, at: Position, step: f32, files: &[Incoming<'_>], ids: &[NodeId]) -> Result<Imported, Error> {
    if ids.len() < files.len().min(MAX_FILES) {
        return Err(Error::new(ErrorKind::InvalidInput, "an id is needed for every file"));
    }
    let mut imported = Imported::default();
    // Decide every file within the drop's text budget and the store's room, storing nothing yet.
    let mut plans = Vec::new();
    let mut text_left = MAX_DROP_TEXT_BYTES;
    let mut store_left = MAX_TOTAL_ASSET_BYTES.saturating_sub(session.assets().byte_len());
    let mut assets_left = MAX_ASSETS.saturating_sub(session.assets().len());
    for (index, file) in files.iter().enumerate() {
        let name = file.name.chars().take(80).collect::<String>();
        let (Some(id), true) = (ids.get(index), index < MAX_FILES) else {
            imported.skipped.push((name, format!("at most {MAX_FILES} files are added at once")));
            continue;
        };
        match plan(file, text_left) {
            Ok(Plan::Text(text)) => {
                text_left = text_left.saturating_sub(text.len());
                plans.push((*id, Plan::Text(text)));
            }
            Ok(Plan::Media { bytes, .. }) if bytes.len() > store_left || assets_left == 0 => {
                imported.skipped.push((name, format!("the project's media is full ({} MiB)", MAX_TOTAL_ASSET_BYTES / (1024 * 1024))));
            }
            Ok(media) => {
                if let Plan::Media { bytes, .. } = &media {
                    store_left = store_left.saturating_sub(bytes.len());
                    assets_left = assets_left.saturating_sub(1);
                }
                plans.push((*id, media));
            }
            Err(why) => imported.skipped.push((name, why)),
        }
    }
    if plans.is_empty() {
        return Ok(imported);
    }
    let added = build(session, plans, at, step).and_then(|nodes| {
        imported.nodes = nodes.iter().map(|node| node.id).collect();
        session.dispatch("canvas.paste", &json!({"canvas": canvas, "nodes": nodes, "edges": []}))
    });
    if let Err(error) = added {
        // Nothing refers to what was stored for this drop: don't leave it taking the project's room.
        session.prune_assets()?;
        imported.nodes.clear();
        return Err(error);
    }
    Ok(imported)
}

#[cfg(test)]
mod tests;

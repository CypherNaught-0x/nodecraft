//! Files from the operating system (#197): dropped on the canvas, or pasted as image data. Each
//! file is recognised from its bytes (never its name), media goes into the project's asset store,
//! and every file that can be held becomes a source node, all added as one undo step through
//! `canvas.paste`. Files that can't are reported with a reason instead of failing the rest.
use crate::Session;
use nodecraft_graph::{Node, Position};
use nodecraft_media::{Limits, MediaKind, probe_image, sniff};
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, MediaMeta, NodeId, ParamValues, Value};
use serde_json::json;
use std::collections::BTreeMap;

/// Most files one drop adds; the rest are reported as skipped.
pub const MAX_FILES: usize = 32;
/// Longest text file that becomes a Text node (the Text node's own limit).
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;

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

/// Text from a file's bytes: UTF-8, or UTF-16 with a byte-order mark.
fn text_of(bytes: &[u8], mime: &str) -> Option<String> {
    let units = |big: bool| {
        bytes.get(2..).unwrap_or(&[]).as_chunks::<2>().0.iter().map(move |pair| if big { u16::from_be_bytes(*pair) } else { u16::from_le_bytes(*pair) })
    };
    match mime {
        "text/plain; charset=utf-16le" => char::decode_utf16(units(false)).collect::<Result<String, _>>().ok(),
        "text/plain; charset=utf-16be" => char::decode_utf16(units(true)).collect::<Result<String, _>>().ok(),
        _ => std::str::from_utf8(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)).ok().map(str::to_owned),
    }
}

/// The source node one file becomes, or why it can't. Media bytes go into the asset store.
fn node_for(session: &mut Session, id: NodeId, file: &Incoming<'_>) -> Result<Node, String> {
    let Some(sniffed) = sniff(file.bytes) else {
        return Err(if file.bytes.is_empty() { "the file is empty".into() } else { "NodeCraft doesn't recognise this kind of file".into() });
    };
    let (kind, parameter, value) = match sniffed.kind {
        MediaKind::Text => {
            let text = text_of(file.bytes, sniffed.mime).ok_or("the text isn't valid UTF-8 or UTF-16")?;
            if text.len() > MAX_TEXT_BYTES {
                return Err("text files over 1 MiB can't become a Text node".into());
            }
            ("source.text", "text", Value::Text(text))
        }
        MediaKind::Image | MediaKind::Video | MediaKind::Audio => {
            if file.bytes.len() > nodecraft_format::MAX_ASSET_BYTES {
                return Err(format!("media over {} MiB can't be added to a project yet", nodecraft_format::MAX_ASSET_BYTES / (1024 * 1024)));
            }
            let mut meta = MediaMeta::default();
            if sniffed.kind == MediaKind::Image {
                let probed = probe_image(file.bytes, &Limits::default()).map_err(|_| "the image can't be read".to_owned())?;
                meta.width = Some(probed.width);
                meta.height = Some(probed.height);
            }
            let mime = sniffed.mime.parse().map_err(|_| "the file type isn't supported".to_owned())?;
            let hash = session.import_asset(file.bytes).map_err(|error| error.message)?;
            let kind = match sniffed.kind {
                MediaKind::Image => "source.image",
                MediaKind::Video => "source.video",
                _ => "source.audio",
            };
            (kind, "asset", Value::Media(AssetRef { hash, mime, meta }))
        }
        MediaKind::Model3d | MediaKind::Document => return Err("3D models and documents can't be added yet".into()),
    };
    let supplied: ParamValues = [(parameter.to_owned(), value)].into_iter().collect();
    session.kinds().get(kind).and_then(|kind| kind.instantiate_instance(id, BTreeMap::new(), &supplied)).map_err(|error| error.message)
}

/// Adds `files` to `canvas` as source nodes, the first at `at` and the rest in a row `step`
/// apart, as one undo step. `ids` supplies a fresh id per file (the caller's, so replay stays
/// deterministic). `Err` only when nothing could be added at all and the paste itself failed;
/// files that can't be held are listed in [`Imported::skipped`].
pub fn import_files(session: &mut Session, canvas: CanvasId, at: Position, step: f32, files: &[Incoming<'_>], ids: &[NodeId]) -> Result<Imported, Error> {
    if ids.len() < files.len().min(MAX_FILES) {
        return Err(Error::new(ErrorKind::InvalidInput, "an id is needed for every file"));
    }
    let mut imported = Imported::default();
    let mut nodes = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let name = file.name.chars().take(80).collect::<String>();
        let (Some(id), true) = (ids.get(index), index < MAX_FILES) else {
            imported.skipped.push((name, format!("at most {MAX_FILES} files are added at once")));
            continue;
        };
        match node_for(session, *id, file) {
            Ok(mut node) => {
                let offset = step * nodes.len() as f32;
                node.pos = Position { x: at.x + offset, y: at.y };
                nodes.push(node);
            }
            Err(why) => imported.skipped.push((name, why)),
        }
    }
    if nodes.is_empty() {
        return Ok(imported);
    }
    imported.nodes = nodes.iter().map(|node| node.id).collect();
    session.dispatch("canvas.paste", &json!({"canvas": canvas, "nodes": nodes, "edges": []}))?;
    Ok(imported)
}

#[cfg(test)]
mod tests;

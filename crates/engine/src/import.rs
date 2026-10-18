//! Files from the operating system (#197): dropped on the canvas, or pasted as image data. Each
//! file is recognised from its bytes (never its name), media goes into the project's asset store,
//! and every file that can be held becomes a source node, all added as one undo step through
//! `canvas.paste`. Files that can't are reported with a reason instead of failing the rest.
//!
//! Eligibility and unique retained capacity are planned before one Session-bound import batch.
//! The candidate store and all nodes publish together through `canvas.paste`; refusing a drop
//! never logically removes unrelated staged assets. Private cleanup debt can still require retry.
use crate::Session;
use nodecraft_format::{MAX_ASSET_BYTES, MAX_ASSETS, MAX_TOTAL_ASSET_BYTES};
use nodecraft_graph::Position;
use nodecraft_media::{Limits, MediaKind, Sniffed, probe_image, sniff};
use nodecraft_types::{CanvasId, Error, MediaMeta, Mime, NodeId};

/// Most files one drop adds; the rest are reported as skipped.
pub const MAX_FILES: usize = 32;
/// Per-file limit for this bounded byte/drop adapter; native stream APIs support larger assets.
pub const MAX_MEDIA_BYTES: usize = MAX_ASSET_BYTES;
/// Buffered UI drop acquisition envelope, independent of native retained-project capacity.
pub const MAX_PROJECT_MEDIA_BYTES: usize = MAX_TOTAL_ASSET_BYTES;
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
    if text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\x0C')) {
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
    if matches!(sniffed.mime, "image/x-exr" | "image/vnd.radiance") {
        return Err("HDR images can't be added yet; convert to PNG or JPEG first".into());
    }
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

/// Adds source nodes through one candidate-store transaction and one undo step. Ineligible
/// files are skipped in input order; a rejected transaction preserves existing staged assets.
/// At most 32 node/source plans are admitted, but skipped bookkeeping is O(files.len()).
pub fn import_files(session: &mut Session, canvas: CanvasId, at: Position, step: f32, files: &[Incoming<'_>], ids: &[NodeId]) -> Result<Imported, Error> {
    let result = crate::session::guard(|| staged::run(session, canvas, at, step, files, ids));
    match result {
        Ok(imported) => Ok(imported),
        Err(mut error) => {
            // The candidate has ended before maintenance. This never retains/removes logical
            // entries or turns successful publication into a refusal. Debt remains charged.
            if crate::session::guard(|| session.assets().collect_garbage()).is_err() {
                error.message.push_str(" Asset maintenance also failed; retry maintenance before importing again.");
            }
            Err(error)
        }
    }
}

mod staged;
#[cfg(test)]
mod tests;

//! The split's private bounded consumer and single candidate publication.
use super::*;
use crate::PreparedImports;
use nodecraft_format::{
    MAX_ASSET_BYTES, MAX_ASSETS, MAX_TOTAL_ASSET_BYTES,
    asset_source::{AssetRange, MAX_LARGE_TOTAL_ASSET_BYTES, MemorySource, StreamMetrics, TransferCancel},
};
use nodecraft_graph::Position;
use nodecraft_media::{Limits, decode_image, encode_png};
use nodecraft_types::{Blake3, MediaMeta, ParamValues};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{self, Cursor, Read},
    sync::Arc,
};
const CHUNK: usize = 64 * 1024;
const MAX_INTERRUPTED: usize = 1024;
fn allocation() -> Error {
    invalid("cannot reserve split bookkeeping")
}
fn read_error() -> Error {
    Error::new(ErrorKind::Conflict, "cannot read the verified image data")
}

/// Only one explicitly requested image, never a store-wide or implicit small-read fallback.
// The 256 MiB private buffer is a per-reader bound: aggregate RSS assumes one decoder
// worker; introducing concurrent readers requires a shared concurrency/memory budget.
fn materialize(mut reader: impl Read, length: u64, cap: u64) -> Result<Vec<u8>, Error> {
    if length > cap {
        return Err(invalid("the image exceeds the split decoder input limit (256 MiB)"));
    }
    let len = usize::try_from(length).map_err(|_| invalid("image input length cannot be represented"))?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|_| invalid("cannot reserve the bounded split image input"))?;
    bytes.resize(len, 0);
    let mut at = 0usize;
    let mut interrupted = 0usize;
    while at < len {
        let end = at.checked_add(CHUNK.min(len - at)).ok_or_else(|| invalid("image read extent overflow"))?;
        let out = bytes.get_mut(at..end).ok_or_else(|| invalid("image read extent mismatch"))?;
        match reader.read(out) {
            Ok(n) if n > 0 && n <= out.len() => {
                at = at.checked_add(n).ok_or_else(|| invalid("image read count overflow"))?;
            }
            Ok(_) => return Err(invalid("the verified image ended early or returned an invalid read extent")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                interrupted = interrupted.checked_add(1).filter(|n| *n <= MAX_INTERRUPTED).ok_or_else(read_error)?;
            }
            Err(_) => return Err(read_error()),
        }
    }
    let mut probe = [0u8; 1];
    loop {
        match reader.read(&mut probe) {
            Ok(0) => break,
            Ok(_) => return Err(invalid("the image exceeds its declared input length")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                interrupted = interrupted.checked_add(1).filter(|n| *n <= MAX_INTERRUPTED).ok_or_else(read_error)?;
            }
            Err(_) => return Err(read_error()),
        }
    }
    Ok(bytes)
}
struct Encoded {
    hash: Blake3,
    bytes: Arc<[u8]>,
}
struct Tile {
    source: usize,
    width: u32,
    height: u32,
}
struct Tiles {
    unique: Vec<Encoded>,
    tiles: Vec<Tile>,
    bytes: u64,
    count: usize,
    retained: u64,
    cap: u64,
}
impl Tiles {
    fn new(session: &Session, count: usize) -> Result<Self, Error> {
        let (retained_count, retained) = session.assets().context().retained_usage()?;
        let mut unique = Vec::new();
        unique.try_reserve_exact(count).map_err(|_| allocation())?;
        let mut tiles = Vec::new();
        tiles.try_reserve_exact(count).map_err(|_| allocation())?;
        let cap = if session.assets().context().supports_large_assets() { MAX_LARGE_TOTAL_ASSET_BYTES } else { MAX_TOTAL_ASSET_BYTES as u64 };
        Ok(Self { unique, tiles, bytes: 0, count: retained_count, retained, cap })
    }
    fn admit(&mut self, session: &Session, png: Vec<u8>, width: u32, height: u32) -> Result<(), Error> {
        if png.len() > MAX_ASSET_BYTES {
            return Err(invalid("a tile is too large to store"));
        }
        let len = u64::try_from(png.len()).map_err(|_| invalid("tile length cannot be represented"))?;
        let hash = Blake3::from_bytes(*blake3::hash(&png).as_bytes());
        let source = if let Some(index) = self.unique.iter().position(|body| body.hash == hash) {
            if self.unique.get(index).is_none_or(|body| body.bytes.len() != png.len()) {
                return Err(invalid("duplicate tile has inconsistent content length"));
            }
            index
        } else {
            let encoded = self
                .bytes
                .checked_add(len)
                .filter(|n| *n <= MAX_TOTAL_ASSET_BYTES as u64)
                .ok_or_else(|| invalid("unique encoded tiles exceed the split batch limit (48 MiB)"))?;
            if let Some(existing) = session.assets().descriptor(&hash) {
                if existing.bytes != len {
                    return Err(invalid("existing tile has inconsistent content length"));
                }
            } else {
                let count = self
                    .count
                    .checked_add(1)
                    .filter(|n| *n <= MAX_ASSETS)
                    .ok_or_else(|| Error::new(ErrorKind::Conflict, "the tiles don't fit in the project's retained media"))?;
                let retained = self
                    .retained
                    .checked_add(len)
                    .filter(|n| *n <= self.cap)
                    .ok_or_else(|| Error::new(ErrorKind::Conflict, "the tiles don't fit in the project's media"))?;
                self.count = count;
                self.retained = retained;
            }
            self.bytes = encoded;
            let index = self.unique.len();
            self.unique.push(Encoded { hash, bytes: Arc::from(png) });
            index
        };
        self.tiles.push(Tile { source, width, height });
        Ok(())
    }
}
fn prepare(session: &Session, unique: &[Encoded]) -> Result<PreparedImports, Error> {
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let batch = if session.assets().context().supports_large_assets() {
        let mut cursors = Vec::new();
        cursors.try_reserve_exact(unique.len()).map_err(|_| allocation())?;
        cursors.extend(unique.iter().map(|body| Cursor::new(body.bytes.as_ref())));
        let mut readers = Vec::new();
        readers.try_reserve_exact(cursors.len()).map_err(|_| allocation())?;
        readers.extend(cursors.iter_mut().map(|cursor| cursor as &mut dyn Read));
        session.prepare_reader_imports(&mut readers, &cancel, &metrics)?
    } else {
        let mut sources = Vec::new();
        sources.try_reserve_exact(unique.len()).map_err(|_| allocation())?;
        for body in unique {
            let source = Arc::new(MemorySource::new(Arc::clone(&body.bytes)));
            sources.push(AssetRange::new(source, 0, body.bytes.len() as u64)?);
        }
        session.prepare_imports(&sources, &cancel, &metrics)?
    };
    if batch.hashes().len() != unique.len() || batch.hashes().iter().zip(unique).any(|(actual, body)| *actual != body.hash) {
        return Err(invalid("prepared tile identities disagree with their immutable inputs"));
    }
    Ok(batch)
}
pub(super) fn run(session: &mut Session, split: &Split<'_>) -> Result<Vec<NodeId>, Error> {
    let tiles = split.rows.checked_mul(split.columns).filter(|n| (1..=MAX_TILES).contains(n));
    let Some(tiles) = tiles.filter(|_| (1..=MAX_SIDE).contains(&split.rows) && (1..=MAX_SIDE).contains(&split.columns)) else {
        return Err(invalid(format!("split into 1 to {MAX_SIDE} rows and columns, at most {MAX_TILES} tiles")));
    };
    let count = usize::try_from(tiles).map_err(|_| invalid("tile count cannot be represented"))?;
    if split.ids.len() != count {
        return Err(invalid("an id is needed for every tile"));
    }
    let canvas = session.document().canvases().get(&split.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let source = canvas.nodes().get(&split.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    let asset = image_of(source).ok_or_else(|| invalid("this node has no image to split"))?;
    let origin = source.pos;
    let mut ids = Vec::new();
    ids.try_reserve_exact(count).map_err(|_| allocation())?;
    let mut positions = Vec::new();
    positions.try_reserve_exact(count).map_err(|_| allocation())?;
    for (i, id) in split.ids.iter().enumerate() {
        if canvas.nodes().contains_key(id) || ids.contains(id) {
            return Err(Error::new(ErrorKind::Conflict, "a tile id already exists or is repeated"));
        }
        let index = u32::try_from(i).map_err(|_| invalid("tile position count overflow"))?;
        let pos = Position { x: origin.x + split.step.0 * ((index % split.columns) as f32 + 1.0), y: origin.y + split.step.1 * (index / split.columns) as f32 };
        if !(pos.x.is_finite() && pos.y.is_finite() && pos.x.abs() <= MAX_COORDINATE && pos.y.abs() <= MAX_COORDINATE) {
            return Err(invalid("the tiles would land outside the canvas; move the node and try again"));
        }
        positions.push(pos);
        ids.push(*id);
    }
    let limits = Limits::default();
    let length = session.assets().descriptor(&asset.hash).ok_or_else(|| Error::new(ErrorKind::NotFound, "the image's data isn't in this project"))?.bytes;
    if length > limits.max_input_bytes {
        return Err(invalid("the image exceeds the split decoder input limit (256 MiB)"));
    }
    let (_, image) = {
        let metrics = StreamMetrics::default();
        let verified = session.assets().open_verified(asset, &TransferCancel::default(), &metrics)?;
        let bytes = materialize(verified.reader(metrics), length, limits.max_input_bytes)?;
        decode_image(&bytes, &limits).map_err(|error| invalid(format!("the image can't be read: {error}")))?
    };
    let (width, height) = (image.width(), image.height());
    if width < split.columns || height < split.rows {
        return Err(invalid("the image is smaller than one pixel per tile"));
    }
    let mut planned = Tiles::new(session, count)?;
    for row in 0..split.rows {
        let (top, bottom) = (edge(height, split.rows, row), edge(height, split.rows, row + 1));
        for column in 0..split.columns {
            let (left, right) = (edge(width, split.columns, column), edge(width, split.columns, column + 1));
            let (w, h) = (right.saturating_sub(left), bottom.saturating_sub(top));
            let tile = image.crop_imm(left, top, w, h).to_rgba8();
            let png = encode_png(w, h, tile.as_raw(), &limits).map_err(|error| invalid(format!("a tile can't be encoded: {error}")))?;
            planned.admit(session, png, w, h)?;
        }
    }
    drop(image);
    let mut nodes = Vec::new();
    nodes.try_reserve_exact(count).map_err(|_| allocation())?;
    for ((tile, id), position) in planned.tiles.iter().zip(&ids).zip(positions) {
        let body = planned.unique.get(tile.source).ok_or_else(|| invalid("tile source index is missing"))?;
        let asset = AssetRef {
            hash: body.hash,
            mime: "image/png".parse()?,
            meta: MediaMeta { width: Some(tile.width), height: Some(tile.height), ..Default::default() },
        };
        let supplied: ParamValues = [("asset".to_owned(), Value::Media(asset))].into_iter().collect();
        let mut node = session.kinds().get("source.image")?.instantiate_instance(*id, BTreeMap::new(), &supplied)?;
        node.pos = position;
        nodes.push(node);
    }
    let params = json!({"canvas":split.canvas,"nodes":nodes,"edges":[]});
    let prepared = prepare(session, &planned.unique)?;
    drop(planned);
    session.dispatch_with_imports("canvas.paste", &params, prepared)?;
    Ok(ids)
}
#[cfg(test)]
mod tests;

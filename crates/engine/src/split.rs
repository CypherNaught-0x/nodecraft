//! Split into grid (#86): an image cut into rows × columns tiles, each a new Image node, laid out
//! in the same grid beside the source, as one undo step through `canvas.paste`.
//!
//! Tiles are row-major (left to right, then top to bottom). When the image doesn't divide evenly,
//! tile edges are spread so that tiles differ by at most one pixel and together cover the image
//! exactly: nothing is cropped away or padded. Each tile is a PNG copy in the project's asset
//! store. Everything is decoded, cut and encoded, and its size checked against the store's room,
//! before anything is stored.
use crate::Session;
use nodecraft_format::{MAX_ASSET_BYTES, MAX_ASSETS, MAX_TOTAL_ASSET_BYTES};
use nodecraft_graph::{Node, Position};
use nodecraft_media::{Limits, decode_image, encode_png};
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, MediaMeta, NodeId, ParamValues, Value};
use serde_json::json;
use std::collections::BTreeMap;

/// Most rows, and most columns, a split makes.
pub const MAX_SIDE: u32 = 16;
/// Most tiles one split makes.
pub const MAX_TILES: u32 = 64;

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// The image a node shows: a Source image's asset, else the first image of its active result.
pub fn image_of(node: &Node) -> Option<&AssetRef> {
    let is_image = |asset: &&AssetRef| asset.mime.essence().starts_with("image/");
    if let Some(Value::Media(asset)) = node.params.get("asset").filter(|_| node.kind == "source.image") {
        return Some(asset).filter(is_image);
    }
    let active = node.active?;
    node.history.iter().find(|g| g.id == active)?.outputs.iter().find_map(|value| match value {
        Value::Media(asset) => Some(asset).filter(is_image),
        _ => None,
    })
}

/// Where tile `index` of `count` starts along a side `length` long: edges are spread evenly, so
/// tiles differ by at most one pixel and the last ends exactly at `length`.
fn edge(length: u32, count: u32, index: u32) -> u32 {
    let at = u64::from(length).saturating_mul(u64::from(index)) / u64::from(count.max(1));
    u32::try_from(at).unwrap_or(length)
}

/// How a split goes: which node, how many rows and columns, the new nodes' ids (one per tile,
/// row-major) and the gap between their cards.
pub struct Split<'a> {
    pub canvas: CanvasId,
    pub node: NodeId,
    pub rows: u32,
    pub columns: u32,
    pub ids: &'a [NodeId],
    /// Card size plus gap, in world units, between neighbouring tiles.
    pub step: (f32, f32),
}

/// Cuts the node's image into tiles and adds them as Image nodes, in one undo step. Returns the
/// new nodes, row-major. Nothing is stored or added when any tile can't be made.
pub fn split_grid(session: &mut Session, split: &Split<'_>) -> Result<Vec<NodeId>, Error> {
    let tiles = split.rows.checked_mul(split.columns).filter(|n| (1..=MAX_TILES).contains(n));
    let Some(tiles) = tiles.filter(|_| (1..=MAX_SIDE).contains(&split.rows) && (1..=MAX_SIDE).contains(&split.columns)) else {
        return Err(invalid(format!("split into 1 to {MAX_SIDE} rows and columns, at most {MAX_TILES} tiles")));
    };
    if split.ids.len() != tiles as usize {
        return Err(invalid("an id is needed for every tile"));
    }
    let canvas = session.document().canvases().get(&split.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let source = canvas.nodes().get(&split.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    let asset = image_of(source).ok_or_else(|| invalid("this node has no image to split"))?;
    let origin = source.pos;
    let bytes = session.assets().get(&asset.hash).ok_or_else(|| Error::new(ErrorKind::NotFound, "the image's data isn't in this project"))?;
    let (_, image) = decode_image(bytes, &Limits::default()).map_err(|_| invalid("the image can't be read"))?;
    let image = image.to_rgba8();
    let (width, height) = image.dimensions();
    if width < split.columns || height < split.rows {
        return Err(invalid("the image is smaller than one pixel per tile"));
    }
    // Cut and encode every tile, and check the store's room, before storing anything.
    let mut encoded = Vec::with_capacity(tiles as usize);
    let mut total = 0usize;
    for row in 0..split.rows {
        let (top, bottom) = (edge(height, split.rows, row), edge(height, split.rows, row + 1));
        for column in 0..split.columns {
            let (left, right) = (edge(width, split.columns, column), edge(width, split.columns, column + 1));
            let (w, h) = (right.saturating_sub(left), bottom.saturating_sub(top));
            let tile = nodecraft_media::image::imageops::crop_imm(&image, left, top, w, h).to_image();
            let png = encode_png(w, h, tile.as_raw(), &Limits::default()).map_err(|_| invalid("a tile can't be encoded"))?;
            if png.len() > MAX_ASSET_BYTES {
                return Err(invalid("a tile is too large to store"));
            }
            total = total.saturating_add(png.len());
            encoded.push((png, w, h));
        }
    }
    let store_left = MAX_TOTAL_ASSET_BYTES.saturating_sub(session.assets().byte_len());
    if total > store_left || MAX_ASSETS.saturating_sub(session.assets().len()) < encoded.len() {
        return Err(Error::new(ErrorKind::Conflict, "the tiles don't fit in the project's media"));
    }
    // Store the tiles and add their nodes.
    let added = (|| {
        let mut nodes = Vec::with_capacity(encoded.len());
        for (index, ((png, w, h), id)) in encoded.into_iter().zip(split.ids).enumerate() {
            let hash = session.import_asset(&png)?;
            let meta = MediaMeta { width: Some(w), height: Some(h), ..MediaMeta::default() };
            let asset = AssetRef { hash, mime: "image/png".parse()?, meta };
            let supplied: ParamValues = [("asset".to_owned(), Value::Media(asset))].into_iter().collect();
            let mut node = session.kinds().get("source.image")?.instantiate_instance(*id, BTreeMap::new(), &supplied)?;
            let (row, column) = (index as u32 / split.columns, index as u32 % split.columns);
            node.pos = Position { x: origin.x + split.step.0 * (column as f32 + 1.0), y: origin.y + split.step.1 * row as f32 };
            nodes.push(node);
        }
        let ids: Vec<NodeId> = nodes.iter().map(|node| node.id).collect();
        session.dispatch("canvas.paste", &json!({"canvas": split.canvas, "nodes": nodes, "edges": []}))?;
        Ok(ids)
    })();
    if added.is_err() {
        // Nothing refers to the tiles stored so far: don't leave them taking the project's room.
        let _ = session.prune_assets();
    }
    added
}

#[cfg(test)]
mod tests;

//! Split into grid (#86): an image cut into rows × columns tiles, each a new Image node, laid out
//! in the same grid beside the source, as one undo step through `canvas.paste`.
//!
//! Tiles are row-major (left to right, then top to bottom). When the image doesn't divide evenly,
//! tile edges are spread so that tiles differ by at most one pixel and together cover the image
//! exactly: nothing is cropped away or padded. Each tile is a PNG copy in the project's asset
//! store. One Session-bound candidate publishes all tiles atomically; refusal never prunes
//! unrelated staged assets. Input materialization is private, explicit and decoder-cap bounded.
use crate::Session;
use nodecraft_graph::Node;
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, NodeId, Value};

/// Most rows, and most columns, a split makes.
pub const MAX_SIDE: u32 = 16;
/// Most tiles one split makes.
pub const MAX_TILES: u32 = 64;
/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// Image types a split can decode.
const SPLITTABLE: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp", "image/tiff"];

pub(crate) fn image(value: &Value) -> Option<&AssetRef> {
    match value {
        Value::Media(asset) if SPLITTABLE.contains(&asset.mime.essence()) => Some(asset),
        _ => None,
    }
}

/// Most rows of images a card shows, and most images in a row.
pub const GALLERY_ROWS: usize = 4;
pub const GALLERY_COLUMNS: usize = 4;

/// The images a node shows, in rows: a Source image's asset; else the images of its active
/// result's first output that has any, one row per batch item when a batch fanned it out (#519)
/// and one row otherwise. Only types NodeCraft decodes; at most [`GALLERY_ROWS`] rows of
/// [`GALLERY_COLUMNS`]. The count is every such image, shown or not.
pub fn image_rows(node: &Node) -> (Vec<Vec<&AssetRef>>, usize) {
    if node.kind == "source.image" {
        let asset = node.params.get("asset").and_then(image);
        return (asset.map(|a| vec![vec![a]]).unwrap_or_default(), usize::from(asset.is_some()));
    }
    let Some(result) = node.active.and_then(|active| node.history.iter().find(|g| g.id == active)) else {
        return (Vec::new(), 0);
    };
    let fanned = !result.items.is_empty();
    for value in &result.outputs {
        let rows: Vec<&[Value]> = match value {
            Value::List(items) if fanned => {
                items.iter().map(|item| if let Value::List(inner) = item { inner.as_slice() } else { std::slice::from_ref(item) }).collect()
            }
            Value::List(items) => vec![items.as_slice()],
            single => vec![std::slice::from_ref(single)],
        };
        let count = rows.iter().map(|row| row.iter().filter_map(image).count()).sum();
        if count > 0 {
            let shown = rows.iter().map(|row| row.iter().filter_map(image).take(GALLERY_COLUMNS).collect::<Vec<_>>()).filter(|row| !row.is_empty());
            return (shown.take(GALLERY_ROWS).collect(), count);
        }
    }
    (Vec::new(), 0)
}

/// The image a node shows first, if it can be split: a Source image's asset, else the first
/// image of its active result (inside a list too), of a type NodeCraft decodes: the first of
/// [`image_rows`], found without walking the rest (cards ask for it every frame).
pub fn image_of(node: &Node) -> Option<&AssetRef> {
    if node.kind == "source.image" {
        return node.params.get("asset").and_then(image);
    }
    let result = node.history.iter().find(|g| Some(g.id) == node.active)?;
    let fanned = !result.items.is_empty();
    result.outputs.iter().find_map(|value| match value {
        Value::List(items) => items.iter().find_map(|item| match item {
            Value::List(inner) if fanned => inner.iter().find_map(image),
            single => image(single),
        }),
        single => image(single),
    })
}

/// Where tile `index` of `count` starts along a side `length` long: edges are spread evenly, so
/// tiles differ by at most one pixel and the last ends exactly at `length`.
pub fn edge(length: u32, count: u32, index: u32) -> u32 {
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
/// new nodes, row-major. No tiles or document edits publish when any tile cannot be made.
pub fn split_grid(session: &mut Session, split: &Split<'_>) -> Result<Vec<NodeId>, Error> {
    let result = crate::session::guard(|| staged::run(session, split));
    match result {
        Ok(ids) => Ok(ids),
        Err(mut error) => {
            // All uncommitted candidate/input owners ended before this non-logical maintenance.
            // Logical pruning is never operation rollback; failed owned cleanup stays charged.
            if crate::session::guard(|| session.assets().collect_garbage()).is_err() {
                error.message.push_str(" Asset maintenance also failed; retry maintenance before splitting again.");
            }
            Err(error)
        }
    }
}

mod staged;

#[cfg(test)]
mod tests;

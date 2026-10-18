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

/// The image a node shows, if it can be split: a Source image's asset, else the first image of
/// its active result, of a type NodeCraft decodes.
pub fn image_of(node: &Node) -> Option<&AssetRef> {
    let is_image = |asset: &&AssetRef| SPLITTABLE.contains(&asset.mime.essence());
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

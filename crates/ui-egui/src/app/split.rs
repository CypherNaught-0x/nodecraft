//! "Split into grid" (#86) from a card's context menu.
use super::NodeCraftApp;
use crate::{ids, theme::Tokens};
use nodecraft_engine::split::{Split, split_grid};
use nodecraft_types::NodeId;

impl NodeCraftApp {
    /// Splits `node`'s image into `rows` × `columns` Image nodes laid out beside it, and selects
    /// them.
    pub(super) fn split_grid(&mut self, node: NodeId, rows: u32, columns: u32, t: &Tokens) {
        if self.comments.drag.is_some() || self.comments.gesture_cancelled {
            self.notice = Some("Finish moving the discussion pin before splitting an image.".into());
            return;
        }
        let document = self.session.document();
        let revision = self.session.revision();
        let count = rows.saturating_mul(columns) as usize;
        let mut new_ids: Vec<NodeId> = Vec::with_capacity(count);
        for index in 0..count {
            match ids::node_id(document, ("split", node, revision, index)).filter(|id| !new_ids.contains(id)) {
                Some(id) => new_ids.push(id),
                None => {
                    self.notice = Some("Couldn't allocate node ids; try again.".into());
                    return;
                }
            }
        }
        // Tiles are new Image cards, so space them by an Image card's size, not the source's.
        let step = (t.card_width + t.card_padding * 4.0, t.card_min_height + t.card_padding * 4.0);
        let split = Split { canvas: self.canvas, node, rows, columns, ids: &new_ids, step };
        match split_grid(&mut self.session, &split) {
            Ok(made) => {
                self.notice = None;
                self.selection.replace(made.into_iter().collect());
            }
            Err(error) => self.notice = Some(error.message),
        }
    }
}

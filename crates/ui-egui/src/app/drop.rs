//! Files dragged in from the operating system (#197) become source nodes at the pointer, one
//! undo step per drop (`nodecraft_engine::import`). Reads are bounded: a file's size is checked
//! before any byte is read, so a huge drop is refused instead of filling memory.
use super::NodeCraftApp;
use crate::{ids, theme::Tokens};
use egui::{Pos2, Rect};
use nodecraft_engine::import::{Imported, Incoming, MAX_FILES, MAX_MEDIA_BYTES, MAX_PROJECT_MEDIA_BYTES, import_files};
use nodecraft_graph::Position;

/// Largest file read from a drop: the project's per-asset cap (text files are smaller still).
pub const MAX_DROP_BYTES: usize = MAX_MEDIA_BYTES;
/// Most bytes read for one drop, all files together: what the project's asset store can hold,
/// so a drop of many large files can't hold hundreds of MiB in memory in one frame.
pub const MAX_DROP_TOTAL_BYTES: usize = MAX_PROJECT_MEDIA_BYTES;

/// A dropped file's bytes, at most `limit`. Only regular files are read, and never more than
/// `limit + 1` bytes, whatever size the file claims: a pipe, a device or a file that grows after
/// the size check can't make the read unbounded. egui's own `bytes()` reads the whole file.
#[cfg(not(target_arch = "wasm32"))]
fn read_bounded(file: &egui::DroppedFileHandle, limit: usize) -> Result<Vec<u8>, Unread> {
    use std::io::Read;
    let unreadable = |_| Unread::Other("the file can't be read".to_owned());
    let meta = std::fs::metadata(file.path()).map_err(unreadable)?;
    if !meta.is_file() {
        return Err(Unread::Other("only files can be added, not folders or devices".into()));
    }
    if meta.len() > limit as u64 {
        return Err(Unread::TooBig);
    }
    let mut bytes = Vec::new();
    let take = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    std::fs::File::open(file.path()).map_err(unreadable)?.take(take).read_to_end(&mut bytes).map_err(unreadable)?;
    if bytes.len() > limit {
        return Err(Unread::TooBig);
    }
    Ok(bytes)
}

/// Browsers only read dropped files asynchronously; that path isn't wired yet (#319).
#[cfg(target_arch = "wasm32")]
fn read_bounded(_: &egui::DroppedFileHandle, _: usize) -> Result<Vec<u8>, Unread> {
    Err(Unread::Other("dropping files is only available in the desktop app for now".to_owned()))
}

/// Why a dropped file wasn't read.
enum Unread {
    /// Larger than the limit it was read with.
    TooBig,
    Other(String),
}

/// A file's name for messages.
fn name_of(file: &egui::DroppedFileHandle) -> String {
    file.path().file_name().map_or_else(|| "A file".to_owned(), |name| name.to_string_lossy().chars().take(80).collect())
}

impl NodeCraftApp {
    /// This frame's dropped files, added as nodes at the pointer; and while files hover over the
    /// window, a hint that dropping adds them.
    pub(super) fn drop_files(&mut self, ui: &egui::Ui, viewport: Rect, t: &Tokens) {
        let (dropped, hovering, pointer) = ui.input(|i| (i.raw.dropped_files.clone(), !i.raw.hovered_files.is_empty(), i.pointer.hover_pos()));
        if hovering {
            let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.id().with("drop_hint")));
            painter.rect_filled(viewport, 0.0, t.marquee_fill);
            painter.text(viewport.center(), egui::Align2::CENTER_CENTER, "Drop to add as nodes", egui::FontId::proportional(t.body_size * 1.5), t.text);
        }
        if dropped.is_empty() {
            return;
        }
        let mut skipped = Vec::new();
        let mut read = Vec::new();
        let mut left = MAX_DROP_TOTAL_BYTES;
        for (index, file) in dropped.iter().enumerate() {
            let name = name_of(file);
            if index >= MAX_FILES {
                skipped.push((name, format!("at most {MAX_FILES} files are added at once")));
                continue;
            }
            if left == 0 {
                skipped.push((name, format!("a drop reads at most {} MiB", MAX_DROP_TOTAL_BYTES / (1024 * 1024))));
                continue;
            }
            match read_bounded(file, MAX_DROP_BYTES.min(left)) {
                Ok(bytes) => {
                    left = left.saturating_sub(bytes.len());
                    read.push((name, bytes));
                }
                // Too big for what this drop has left, not for a file on its own.
                Err(Unread::TooBig) if left < MAX_DROP_BYTES => {
                    skipped.push((name, format!("a drop reads at most {} MiB", MAX_DROP_TOTAL_BYTES / (1024 * 1024))));
                }
                Err(Unread::TooBig) => skipped.push((name, format!("files over {} MiB can't be added", MAX_DROP_BYTES / (1024 * 1024)))),
                Err(Unread::Other(why)) => skipped.push((name, why)),
            }
        }
        // The first card is centred under the pointer; the rest follow in a row.
        let at: Pos2 = pointer.filter(|p| viewport.contains(*p)).unwrap_or(viewport.center());
        let world = self.view.to_world(viewport.min, at);
        let position = Position { x: world.x - t.card_width * 0.5, y: world.y - t.card_min_height * 0.5 };
        let document = self.session.document();
        let revision = self.session.revision();
        let mut new_ids = Vec::with_capacity(read.len());
        for index in 0..read.len() {
            let fresh = ids::node_id(document, ("drop", revision, index, position.x.to_bits(), position.y.to_bits())).filter(|id| !new_ids.contains(id));
            match fresh {
                Some(id) => new_ids.push(id),
                None => {
                    self.notice = Some("Couldn't allocate node ids; try again.".into());
                    return;
                }
            }
        }
        let files: Vec<Incoming<'_>> = read.iter().map(|(name, bytes)| Incoming { name, bytes }).collect();
        let step = t.card_width + t.card_padding * 4.0;
        match import_files(&mut self.session, self.canvas, position, step, &files, &new_ids) {
            Ok(mut imported) => {
                if !imported.nodes.is_empty() {
                    self.selection.replace(imported.nodes.iter().copied().collect());
                }
                skipped.append(&mut imported.skipped);
                self.notice = Imported { nodes: imported.nodes, skipped }.notice();
            }
            Err(error) => self.notice = Some(error.message),
        }
    }
}

#[cfg(test)]
mod tests;

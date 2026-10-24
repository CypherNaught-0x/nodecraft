//! Browser drops (#319). A browser's dropped files only read asynchronously: each is admitted by
//! the size the browser reports before any read, read on the browser's task queue, and the batch
//! is imported on a later frame through the same engine import as a desktop drop. Admission and
//! import are platform-neutral so native tests cover them; only the reads are wasm-only.
use super::NodeCraftApp;
use crate::ids;
use nodecraft_engine::import::{Imported, Incoming, MAX_FILES, MAX_MEDIA_BYTES, MAX_PROJECT_MEDIA_BYTES};
use nodecraft_graph::Position;
use std::{cell::RefCell, rc::Rc, sync::Arc};

/// Longest name kept for a dropped file, in characters.
const NAME_CHARS: usize = 80;

/// What becomes of one dropped file, by the size the browser reported (`None`: no size).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Admission {
    Read,
    Skip(String),
}

/// Admits files in order: at most [`MAX_FILES`], each at most 16 MiB, together at most the drop
/// total. Sizes come from the browser and are checked again after the read.
pub(super) fn admit(sizes: impl IntoIterator<Item = Option<f64>>) -> Vec<Admission> {
    let mut total: u64 = 0;
    let mut read = 0usize;
    sizes
        .into_iter()
        .map(|size| {
            if read >= MAX_FILES {
                return Admission::Skip(format!("at most {MAX_FILES} files are added at once"));
            }
            // A finite, non-negative size within the per-file cap converts exactly.
            let Some(bytes) = size.filter(|s| s.is_finite() && *s >= 0.0 && *s <= MAX_MEDIA_BYTES as f64).map(|s| s as u64) else {
                return Admission::Skip(match size {
                    Some(s) if s.is_finite() && s > 0.0 => too_big(),
                    _ => "the file can't be read".to_owned(),
                });
            };
            match total.checked_add(bytes).filter(|t| *t <= MAX_PROJECT_MEDIA_BYTES as u64) {
                Some(next) => {
                    total = next;
                    read += 1;
                    Admission::Read
                }
                None => Admission::Skip(format!("a drop reads at most {} MiB", MAX_PROJECT_MEDIA_BYTES / (1024 * 1024))),
            }
        })
        .collect()
}

fn too_big() -> String {
    format!("files over {} MiB can't be added", MAX_MEDIA_BYTES / (1024 * 1024))
}

/// A dropped file's name for messages, from its path; never trusted for its type.
pub(super) fn name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().chars().take(NAME_CHARS).collect())
        .filter(|n: &String| !n.is_empty())
        .unwrap_or_else(|| "A dropped file".to_owned())
}

/// One admitted file's read, filled in by its task.
pub(super) type Slot = Option<Result<Vec<u8>, String>>;

/// A browser drop being read: the files in drop order, and where they go.
pub(crate) struct Pending {
    /// Admitted files and their reads, in drop order.
    pub(super) reads: Rc<RefCell<Vec<(String, Slot)>>>,
    /// Files skipped before reading, in drop order.
    pub(super) skipped: Vec<(String, String)>,
    pub(super) identity: Arc<crate::navigation::SessionIdentity>,
    pub(super) project: nodecraft_types::ProjectId,
    pub(super) canvas: nodecraft_types::CanvasId,
    pub(super) revision: u64,
    pub(super) position: Position,
    pub(super) step: f32,
}
impl Pending {
    fn current(&self, app: &mut NodeCraftApp) -> bool {
        self.canvas == app.canvas
            && self.project == app.session.document().id()
            && self.revision == app.session.revision()
            && Arc::ptr_eq(&self.identity, &crate::navigation::Geometry::identity(&mut app.session))
    }
    fn done(&self) -> bool {
        self.reads.try_borrow().is_ok_and(|reads| reads.iter().all(|(_, slot)| slot.is_some()))
    }
}

impl NodeCraftApp {
    /// Imports a browser drop whose reads have all finished; keeps it while any is still reading.
    pub(super) fn poll_web_drop(&mut self) {
        let Some(pending) = self.web_drop.take() else {
            return;
        };
        if !pending.done() {
            self.web_drop = Some(pending);
            return;
        }
        if !pending.current(self) {
            self.notice = Some("The document changed while reading files; nothing was added. Drop them again.".into());
            return;
        }
        let Ok(reads) = pending.reads.try_borrow() else {
            self.notice = Some("File reading failed unexpectedly; nothing was added.".into());
            return;
        };
        let mut skipped = pending.skipped.clone();
        let mut incoming = Vec::new();
        let mut new_ids = Vec::new();
        let mut total = 0usize;
        for (index, (name, slot)) in reads.iter().enumerate() {
            let bytes = match slot {
                Some(Ok(bytes)) if bytes.len() > MAX_MEDIA_BYTES => {
                    skipped.push((name.clone(), too_big()));
                    continue;
                }
                Some(Ok(bytes)) => bytes,
                Some(Err(_)) | None => {
                    skipped.push((name.clone(), "the file can't be read".to_owned()));
                    continue;
                }
            };
            // The browser's sizes were admitted; a file that grew past them doesn't widen the drop.
            match total.checked_add(bytes.len()).filter(|t| *t <= MAX_PROJECT_MEDIA_BYTES) {
                Some(next) => total = next,
                None => {
                    skipped.push((name.clone(), format!("a drop reads at most {} MiB", MAX_PROJECT_MEDIA_BYTES / (1024 * 1024))));
                    continue;
                }
            }
            let fresh =
                ids::node_id(self.session.document(), ("drop", self.session.revision(), index, pending.position.x.to_bits(), pending.position.y.to_bits()))
                    .filter(|id| !new_ids.contains(id));
            let Some(id) = fresh else {
                self.notice = Some("Couldn't allocate node ids; try again.".into());
                return;
            };
            new_ids.push(id);
            incoming.push(Incoming { name, bytes });
        }
        let result = if incoming.is_empty() {
            Ok(Imported::default())
        } else {
            nodecraft_engine::import::import_files(&mut self.session, pending.canvas, pending.position, pending.step, &incoming, &new_ids)
        };
        match result {
            Ok(mut imported) => {
                if !imported.nodes.is_empty() {
                    self.selection.replace(imported.nodes.iter().copied().collect());
                }
                skipped.append(&mut imported.skipped);
                imported.skipped = skipped;
                self.notice = imported.notice();
            }
            Err(e) => self.notice = Some(e.message),
        }
    }

    /// Starts reading a browser drop at `position`.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn start_web_drop(&mut self, ctx: &egui::Context, files: &[egui::DroppedFileHandle], position: Position, step: f32) {
        if self.web_drop.is_some() {
            self.notice = Some("File reading is still finishing. Try again in a moment.".into());
            return;
        }
        let admissions = admit(files.iter().map(|file| file.web_file().map(|f| f.size())));
        let mut skipped = Vec::new();
        let mut reads = Vec::new();
        let mut started = Vec::new();
        for (file, admission) in files.iter().zip(admissions) {
            let name = name(file.path());
            match admission {
                Admission::Skip(why) => skipped.push((name, why)),
                Admission::Read => {
                    started.push((reads.len(), Arc::clone(file)));
                    reads.push((name, None));
                }
            }
        }
        let reads = Rc::new(RefCell::new(reads));
        for (slot, file) in started {
            let reads = Rc::clone(&reads);
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let read = file.bytes_async().await;
                if let Ok(mut reads) = reads.try_borrow_mut()
                    && let Some((_, entry)) = reads.get_mut(slot)
                {
                    *entry = Some(read);
                }
                ctx.request_repaint();
            });
        }
        let identity = crate::navigation::Geometry::identity(&mut self.session);
        self.web_drop = Some(Pending {
            reads,
            skipped,
            identity,
            project: self.session.document().id(),
            canvas: self.canvas,
            revision: self.session.revision(),
            position,
            step,
        });
        self.notice = Some("Reading dropped files…".into());
    }
}

#[cfg(test)]
mod tests;

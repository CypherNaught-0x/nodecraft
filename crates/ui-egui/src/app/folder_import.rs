//! Folders as Folder cards: dropped on the canvas (a new card, or a Folder card under the
//! pointer refilled), picked with Import folder, or picked again from a Folder card.
//!
//! A folder is read on its own thread, never on a canvas frame: its subfolders down to
//! [`MAX_DEPTH`] levels, hidden entries left out, at most [`MAX_LISTED`] entries looked at and
//! the first [`folder::MAX_FILES`] files (in natural order) read, each at most 16 MiB and all of
//! them at most [`MAX_READ_BYTES`]. Only regular files are read, through `File::take`, so a pipe,
//! a device or a growing file can't make a read unbounded. Reading can be cancelled. The read
//! bytes then reach one engine import ([`nodecraft_engine::import::import_folder`]): one undo step.
use super::NodeCraftApp;
use crate::{ids, theme::Tokens};
use nodecraft_engine::import::{FolderTarget, Incoming, MAX_MEDIA_BYTES};
use nodecraft_graph::Position;
use nodecraft_nodes::folder;
use nodecraft_types::{CanvasId, NodeId, ProjectId};
use std::{
    collections::VecDeque,
    future::Future,
    io::Read,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    task::{Context, Poll, Wake, Waker},
};

/// Deepest subfolder read.
pub const MAX_DEPTH: usize = 8;
/// Most folder entries looked at, files and folders, hidden or not.
pub const MAX_LISTED: usize = 20_000;
/// Most bytes one folder import reads. The import that follows hashes and stores them on the UI
/// thread, as a drop's does, so this also bounds how long that takes.
pub const MAX_READ_BYTES: u64 = 256 * 1024 * 1024;
/// Files an operating system leaves in folders, never imported.
const CLUTTER: [&str; 3] = ["thumbs.db", "desktop.ini", "icon\r"];

type PickFuture = Pin<Box<dyn Future<Output = Option<PathBuf>>>>;

/// Where a folder being picked or read goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Target {
    /// A new card with its top-left corner here.
    New(Position),
    /// This Folder card, refilled.
    Existing(NodeId),
}

/// The files of a folder, read.
#[derive(Debug, Default)]
pub(super) struct ReadFolder {
    pub name: String,
    /// Paths inside the folder (`/`-separated) and contents, in natural order.
    pub files: Vec<(String, Vec<u8>)>,
    pub skipped: Vec<(String, String)>,
}

struct Reading {
    name: String,
    project: ProjectId,
    canvas: CanvasId,
    target: Target,
    cancel: Arc<AtomicBool>,
    read: Arc<AtomicUsize>,
    result: mpsc::Receiver<Result<ReadFolder, String>>,
}

impl Drop for Reading {
    fn drop(&mut self) {
        // Ending the import any way stops its reader at its next file.
        self.cancel.store(true, Ordering::Release);
    }
}

#[derive(Default)]
pub(super) struct FolderImport {
    picking: Option<(PickFuture, Target)>,
    reading: Option<Reading>,
    /// What the folder picker "chooses" in tests.
    #[cfg(test)]
    pub(super) pick: Option<PathBuf>,
}

impl FolderImport {
    pub(super) fn busy(&self) -> bool {
        self.picking.is_some() || self.reading.is_some()
    }
}

struct Repaint(egui::Context);
impl Wake for Repaint {
    fn wake(self: Arc<Self>) {
        self.0.request_repaint();
    }
}

/// A name shown to people or kept for a file: control characters dropped, at most 200 chars.
fn clean(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(200).collect()
}

/// Reads a file only when it is a regular file of at most `room` bytes; why not otherwise.
fn read_file(path: &Path, room: u64) -> Result<Vec<u8>, &'static str> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // As for dropped files: a FIFO opens at once and the regular-file check refuses it.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    let file = options.open(path).map_err(|_| "the file can't be read")?;
    let metadata = file.metadata().map_err(|_| "the file can't be read")?;
    if !metadata.is_file() {
        return Err("only regular files are read, not devices or pipes");
    }
    let length = metadata.len();
    if length > MAX_MEDIA_BYTES as u64 {
        return Err("files over 16 MiB can't be added");
    }
    if length > room {
        return Err("a folder import reads at most 256 MiB");
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(usize::try_from(length).map_err(|_| "the file is too large")?).map_err(|_| "there isn't enough memory to read the file")?;
    file.take(length.saturating_add(1)).read_to_end(&mut bytes).map_err(|_| "the file can't be read")?;
    if bytes.len() as u64 != length {
        return Err("the file changed while it was being read; try again");
    }
    Ok(bytes)
}

/// Reads the folder at `root` (see the module docs for its bounds). `read` counts files read.
pub(super) fn read_folder(root: &Path, cancel: &AtomicBool, read: &AtomicUsize) -> Result<ReadFolder, String> {
    let cancelled = || cancel.load(Ordering::Acquire);
    let name = clean(&root.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "Folder".into()));
    if !std::fs::metadata(root).is_ok_and(|m| m.is_dir()) {
        return Err(format!("{name} isn't a folder that can be read."));
    }
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut listed = 0usize;
    let mut queue = VecDeque::from([(root.to_path_buf(), String::new(), 0usize)]);
    'walk: while let Some((dir, prefix, depth)) = queue.pop_front() {
        if cancelled() {
            return Err("Reading the folder was cancelled; nothing was added.".into());
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            skipped.push((if prefix.is_empty() { name.clone() } else { prefix }, "the folder can't be read".into()));
            continue;
        };
        for entry in entries {
            listed = listed.saturating_add(1);
            if listed > MAX_LISTED {
                skipped.push((name.clone(), format!("only the first {MAX_LISTED} entries of a folder are looked at")));
                break 'walk;
            }
            let Ok(entry) = entry else { continue };
            let raw = entry.file_name().to_string_lossy().to_lowercase();
            let file = clean(&raw);
            // Hidden files and folders, and what the system leaves in folders, are never wanted.
            if file.is_empty() || file.starts_with('.') || CLUTTER.contains(&raw.as_str()) {
                continue;
            }
            let file = clean(&entry.file_name().to_string_lossy());
            let path = if prefix.is_empty() { file } else { format!("{prefix}/{file}") };
            // The entry's own type. Links are never followed: one to a folder could loop, and one
            // to a file could bring in something outside the folder, such as a key file.
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() {
                skipped.push((path, "links aren't followed; only files inside the folder are read".into()));
            } else if kind.is_dir() {
                if depth < MAX_DEPTH {
                    queue.push_back((entry.path(), path, depth.saturating_add(1)));
                } else {
                    skipped.push((path, format!("folders more than {MAX_DEPTH} levels deep are left out")));
                }
            } else {
                found.push((path, entry.path()));
            }
        }
    }
    found.sort_by(|a, b| folder::natural(&a.0, &b.0).then_with(|| a.0.cmp(&b.0)));
    let mut files = Vec::new();
    let mut room = MAX_READ_BYTES;
    for (index, (path, at)) in found.into_iter().enumerate() {
        if cancelled() {
            return Err("Reading the folder was cancelled; nothing was added.".into());
        }
        if index >= folder::MAX_FILES {
            skipped.push((path, format!("a folder holds at most {} files", folder::MAX_FILES)));
            continue;
        }
        match read_file(&at, room) {
            Ok(bytes) => {
                room = room.saturating_sub(bytes.len() as u64);
                files.push((path, bytes));
                read.fetch_add(1, Ordering::Relaxed);
            }
            Err(why) => skipped.push((path, why.into())),
        }
    }
    Ok(ReadFolder { name, files, skipped })
}

impl NodeCraftApp {
    /// Whether a folder is being picked or read.
    pub(super) fn folder_import_busy(&self) -> bool {
        self.folder_import.busy()
    }

    /// Opens the system's folder picker; the chosen folder goes to `target`.
    pub(super) fn pick_folder(&mut self, target: Target) {
        if self.folder_import.busy() {
            self.notice = Some("A folder is already being imported; wait for it or cancel it.".into());
            return;
        }
        #[cfg(not(test))]
        {
            // Built on the UI thread: AppKit finds its window here.
            let future = rfd::AsyncFileDialog::new().set_title("Import folder").pick_folder();
            self.folder_import.picking = Some((Box::pin(async move { future.await.map(|folder| folder.path().to_path_buf()) }), target));
        }
        #[cfg(test)]
        {
            self.folder_import.picking = Some((Box::pin(std::future::ready(self.folder_import.pick.take())), target));
        }
    }

    /// Starts reading the folder at `path` for `target` on its own thread.
    pub(super) fn read_folder_for(&mut self, path: PathBuf, target: Target) {
        if self.folder_import.reading.is_some() {
            self.notice = Some("A folder is already being imported; wait for it or cancel it.".into());
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let read = Arc::new(AtomicUsize::new(0));
        let (send, result) = mpsc::channel();
        let name = clean(&path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default());
        let (stop, count) = (Arc::clone(&cancel), Arc::clone(&read));
        let spawned = std::thread::Builder::new().name("nodecraft-folder-read".into()).spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_folder(&path, &stop, &count)))
                .unwrap_or_else(|_| Err("Reading the folder failed unexpectedly; nothing was added.".into()));
            // The app may have ended the import; then nobody listens, which is fine.
            let _ = send.send(outcome);
        });
        if spawned.is_err() {
            self.notice = Some("Folder reading is unavailable; restart the app to try again.".into());
            return;
        }
        self.notice = None;
        self.folder_import.reading = Some(Reading { name, project: self.session.document().id(), canvas: self.canvas, target, cancel, read, result });
    }

    /// Polls the picker and the reader, shows the reading status with its Cancel button, and
    /// imports a folder once it is read.
    pub(super) fn poll_folder_import(&mut self, ctx: &egui::Context, t: &Tokens) {
        if let Some((mut future, target)) = self.folder_import.picking.take() {
            let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
            let poll = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(&mut Context::from_waker(&waker))));
            match poll {
                Ok(Poll::Pending) => self.folder_import.picking = Some((future, target)),
                Ok(Poll::Ready(Some(path))) => self.read_folder_for(path, target),
                Ok(Poll::Ready(None)) => {}
                Err(_) => self.notice = Some("The folder picker could not be opened; try again.".into()),
            }
        }
        let Some(reading) = self.folder_import.reading.take() else { return };
        match reading.result.try_recv() {
            Ok(Ok(read)) => {
                self.import_read_folder(&reading, read);
                return;
            }
            Ok(Err(message)) => {
                self.notice = Some(message);
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.notice = Some("Reading the folder failed unexpectedly; nothing was added.".into());
                return;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let read = reading.read.load(Ordering::Relaxed);
        let cancel = egui::Area::new(egui::Id::new("folder_import_reading"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::LEFT_BOTTOM, egui::Vec2::new(t.card_padding, -t.card_padding * 6.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(t.panel_fill)
                    .inner_margin(t.card_padding)
                    .show(ui, |ui| {
                        ui.label(format!("Reading folder {}… {read} files", reading.name));
                        ui.button("Cancel folder import").clicked()
                    })
                    .inner
            })
            .inner;
        if cancel {
            self.notice = Some("Folder import cancelled; nothing was added.".into());
        } else {
            self.folder_import.reading = Some(reading);
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn import_read_folder(&mut self, reading: &Reading, read: ReadFolder) {
        if reading.project != self.session.document().id() || reading.canvas != self.canvas {
            self.notice = Some("The project or canvas changed while the folder was read; nothing was added. Import it again.".into());
            return;
        }
        let target = match reading.target {
            Target::New(at) => {
                let seed = ("folder", self.session.revision(), at.x.to_bits(), at.y.to_bits(), read.files.len());
                let Some(id) = ids::node_id(self.session.document(), seed) else {
                    self.notice = Some("Couldn't allocate a node id; try again.".into());
                    return;
                };
                FolderTarget::New { canvas: reading.canvas, at, id }
            }
            Target::Existing(id) => FolderTarget::Existing { canvas: reading.canvas, id },
        };
        let incoming: Vec<Incoming<'_>> = read.files.iter().map(|(name, bytes)| Incoming { name, bytes }).collect();
        match nodecraft_engine::import::import_folder(&mut self.session, target, &read.name, &incoming) {
            Ok(imported) => {
                self.selection.replace([imported.node].into_iter().collect());
                let mut skipped = read.skipped;
                skipped.extend(imported.skipped);
                let left_out = nodecraft_engine::import::Imported { nodes: Vec::new(), skipped }.notice();
                let added = format!("Added {} files from {}.", imported.files, read.name);
                self.notice = Some(left_out.map_or(added.clone(), |left_out| format!("{added} {left_out}")));
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// Folders among dropped paths go to a Folder card; `false` when there are none. The Folder
    /// card under `world`, if any, is refilled; otherwise a new one goes at `at`.
    pub(super) fn drop_folders(&mut self, paths: &[PathBuf], world: egui::Pos2, at: Position) -> bool {
        let mut folders = paths.iter().filter(|path| path.is_dir());
        let Some(first) = folders.next() else { return false };
        let canvas = self.session.document().canvases().get(&self.canvas);
        let under = self
            .painted
            .iter()
            .filter(|(id, rect)| rect.contains(world) && canvas.and_then(|c| c.nodes().get(id)).is_some_and(|node| node.kind == folder::ID))
            .map(|(id, _)| *id)
            .next();
        let target = under.map_or(Target::New(at), Target::Existing);
        let others = paths.len().saturating_sub(1);
        self.read_folder_for(first.clone(), target);
        if others > 0 && self.folder_import.reading.is_some() {
            let name = clean(&first.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default());
            self.notice = Some(format!("Importing {name}; drop other folders and files separately ({others} left out)."));
        }
        true
    }
}

#[cfg(test)]
mod tests;

//! The Assets browser's Import files… (#96): files picked by a person are read on their own
//! thread, never on a frame, and imported into the project's library as source cards in the
//! middle of the view, in one undo step, through [`nodecraft_engine::import::import_library`].
//!
//! Reading is bounded as a folder import's is: only regular files, each at most 16 MiB and
//! all of them at most 48 MiB, at most 32 files at a time, counted as they are read and
//! cancellable between files. Each file then has its own outcome: added, a duplicate, already
//! in the library, or failed with why. The paths of the failed files stay in this window's view
//! state, so "Retry failed" reads exactly those again; they never reach the document, which
//! keeps only a file's name, as its card's title.
use super::NodeCraftApp;
use super::folder_import::{clean, read_bounded};
use super::library_browsers::{ImportLine, ImportReport};
use crate::{ids, theme::Tokens};
use nodecraft_engine::import::{FileState, Incoming, MAX_FILES, MAX_PROJECT_MEDIA_BYTES, import_library};
use nodecraft_graph::Position;
use nodecraft_types::{CanvasId, NodeId, ProjectId};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    task::{Context, Poll, Wake, Waker},
};

type PickFuture = Pin<Box<dyn Future<Output = Option<Vec<PathBuf>>>>>;

/// One picked file, read or not.
struct ReadFile {
    path: PathBuf,
    name: String,
    bytes: Result<Vec<u8>, String>,
}

struct Reading {
    project: ProjectId,
    canvas: CanvasId,
    total: usize,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
    result: mpsc::Receiver<Result<Vec<ReadFile>, String>>,
}

impl Drop for Reading {
    fn drop(&mut self) {
        // Ending the import any way stops its reader at its next file.
        self.cancel.store(true, Ordering::Release);
    }
}

/// The picker and the reader of a library import.
#[derive(Default)]
pub(super) struct LibraryImport {
    picking: Option<PickFuture>,
    reading: Option<Reading>,
    /// What the file picker "chooses" in tests.
    #[cfg(test)]
    pub(super) pick: Option<Vec<PathBuf>>,
    /// In tests, the reader waits for a message before each file.
    #[cfg(test)]
    pub(super) hold: Option<mpsc::Receiver<()>>,
}

impl LibraryImport {
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

const OVER_ROOM: &str = "one import reads at most 48 MiB; retry to import it next";

/// Reads `paths` in order: at most [`MAX_FILES`] of them, within the import's byte budget.
fn read_files(paths: Vec<PathBuf>, cancel: &AtomicBool, done: &AtomicUsize, hold: &dyn Fn()) -> Result<Vec<ReadFile>, String> {
    let mut room = MAX_PROJECT_MEDIA_BYTES as u64;
    let mut read = Vec::new();
    read.try_reserve_exact(paths.len()).map_err(|_| "There isn't enough memory to import this many files.".to_owned())?;
    for (index, path) in paths.into_iter().enumerate() {
        hold();
        if cancel.load(Ordering::Acquire) {
            return Err("Import cancelled; nothing was added.".into());
        }
        let name = clean(&path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default());
        let bytes = if index >= MAX_FILES {
            Err(format!("at most {MAX_FILES} files are imported at once; retry to import it next"))
        } else {
            read_bounded(&path, room, OVER_ROOM).map_err(str::to_owned)
        };
        if let Ok(bytes) = &bytes {
            room = room.saturating_sub(bytes.len() as u64);
        }
        done.fetch_add(1, Ordering::Relaxed);
        read.push(ReadFile { path, name, bytes });
    }
    Ok(read)
}

impl NodeCraftApp {
    /// Opens the system's file picker for files to import into the library.
    pub(super) fn pick_library_files(&mut self) {
        if self.library.browsers.import.busy() {
            self.notice = Some("Files are already being imported; wait for them or cancel.".into());
            return;
        }
        #[cfg(not(test))]
        {
            // Built on the UI thread: AppKit finds its window here.
            let future = rfd::AsyncFileDialog::new().set_title("Import into the library").pick_files();
            self.library.browsers.import.picking =
                Some(Box::pin(async move { future.await.map(|files| files.iter().map(|file| file.path().to_path_buf()).collect()) }));
        }
        #[cfg(test)]
        {
            self.library.browsers.import.picking = Some(Box::pin(std::future::ready(self.library.browsers.import.pick.take())));
        }
    }

    /// Starts reading `paths` on their own thread.
    pub(super) fn read_library_files(&mut self, paths: Vec<PathBuf>) {
        if self.library.browsers.import.reading.is_some() {
            self.notice = Some("Files are already being imported; wait for them or cancel.".into());
            return;
        }
        if paths.is_empty() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicUsize::new(0));
        let (send, result) = mpsc::channel();
        let (stop, count) = (Arc::clone(&cancel), Arc::clone(&done));
        let total = paths.len();
        #[cfg(test)]
        let hold = self.library.browsers.import.hold.take();
        let spawned = std::thread::Builder::new().name("nodecraft-library-read".into()).spawn(move || {
            #[cfg(test)]
            let wait = move || {
                if let Some(hold) = &hold {
                    let _ = hold.recv();
                }
            };
            #[cfg(not(test))]
            let wait = || {};
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_files(paths, &stop, &count, &wait)))
                .unwrap_or_else(|_| Err("Reading the files failed unexpectedly; nothing was added.".into()));
            // The app may have ended the import; then nobody listens, which is fine.
            let _ = send.send(outcome);
        });
        if spawned.is_err() {
            self.notice = Some("File reading is unavailable; restart the app to try again.".into());
            return;
        }
        self.notice = None;
        self.library.browsers.import.reading = Some(Reading { project: self.session.document().id(), canvas: self.canvas, total, cancel, done, result });
    }

    /// Polls the picker and the reader, shows the reading progress with its Cancel button, and
    /// imports the files once they are read.
    pub(super) fn poll_library_import(&mut self, ctx: &egui::Context, t: &Tokens) {
        if let Some(mut future) = self.library.browsers.import.picking.take() {
            let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
            let poll = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(&mut Context::from_waker(&waker))));
            match poll {
                Ok(Poll::Pending) => self.library.browsers.import.picking = Some(future),
                Ok(Poll::Ready(Some(paths))) => self.read_library_files(paths),
                Ok(Poll::Ready(None)) => {}
                Err(_) => self.notice = Some("The file picker could not be opened; try again.".into()),
            }
        }
        let Some(reading) = self.library.browsers.import.reading.take() else { return };
        match reading.result.try_recv() {
            Ok(Ok(read)) => {
                self.import_read_files(&reading, read, t);
                return;
            }
            Ok(Err(message)) => {
                self.notice = Some(message);
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.notice = Some("Reading the files failed unexpectedly; nothing was added.".into());
                return;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let done = reading.done.load(Ordering::Relaxed).min(reading.total);
        let cancel = egui::Area::new(egui::Id::new("library_import_reading"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::LEFT_BOTTOM, egui::Vec2::new(t.card_padding, -t.card_padding * 6.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(t.panel_fill)
                    .inner_margin(t.card_padding)
                    .show(ui, |ui| {
                        ui.label(format!("Reading files for the library… {done} of {}", reading.total));
                        let ratio = if reading.total == 0 { 0.0 } else { done as f32 / reading.total as f32 };
                        ui.add(egui::ProgressBar::new(ratio).desired_width(t.card_width));
                        ui.button("Cancel import").clicked()
                    })
                    .inner
            })
            .inner;
        if cancel {
            // Dropping the reading stops the reader at its next file.
            self.notice = Some("Import cancelled; nothing was added.".into());
        } else {
            self.library.browsers.import.reading = Some(reading);
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn import_read_files(&mut self, reading: &Reading, read: Vec<ReadFile>, t: &Tokens) {
        let failed_all = |read: Vec<ReadFile>, why: &str| ImportReport {
            summary: format!("{} failed", read.len()),
            lines: read.iter().map(|file| ImportLine { name: file.name.clone(), state: why.to_owned(), failed: true }).collect(),
            retry: read.into_iter().map(|file| file.path).collect(),
        };
        if reading.project != self.session.document().id() || reading.canvas != self.canvas {
            let why = "the project or canvas changed while it was read";
            self.library.browsers.report = Some(failed_all(read, why));
            self.notice = Some("The project or canvas changed while the files were read; nothing was added. Retry to import them here.".into());
            return;
        }
        let centre = match self.browse_centre(t) {
            Ok(centre) => centre,
            Err(error) => {
                self.notice = Some(error.message);
                return;
            }
        };
        // The files that were read, with the card id each would get.
        let mut incoming = Vec::new();
        let mut fresh: Vec<NodeId> = Vec::new();
        let mut taken = Vec::new();
        for (index, file) in read.iter().enumerate() {
            if let Ok(bytes) = &file.bytes {
                let id = ids::node_id(self.session.document(), ("library-import", self.session.revision(), index)).filter(|id| !fresh.contains(id));
                let Some(id) = id else {
                    self.notice = Some("Couldn't allocate node ids; try again.".into());
                    return;
                };
                fresh.push(id);
                incoming.push(Incoming { name: &file.name, bytes });
                taken.push(index);
            }
        }
        let at = Position { x: centre.x, y: centre.y };
        let step = t.card_width + t.card_padding * 4.0;
        let imported = if incoming.is_empty() {
            Ok(Default::default())
        } else {
            import_library(&mut self.session, self.canvas, at, step, &incoming, &fresh, &[], &|node, kinds| Self::creation_position(node, kinds, t))
        };
        let imported = match imported {
            Ok(imported) => imported,
            Err(error) => {
                self.notice = Some(error.message.clone());
                self.library.browsers.report = Some(failed_all(read, &error.message));
                return;
            }
        };
        let mut outcomes = taken.iter().zip(imported.files.iter());
        let mut next = outcomes.next();
        let mut lines = Vec::new();
        let mut retry = Vec::new();
        for (index, file) in read.into_iter().enumerate() {
            let (state, failed) = match (&file.bytes, next) {
                (Err(why), _) => (why.clone(), true),
                (Ok(_), Some((at, (_, outcome)))) if *at == index => {
                    next = outcomes.next();
                    match outcome {
                        FileState::Added { .. } => ("added".to_owned(), false),
                        FileState::Duplicate { of, .. } => {
                            let original = taken.get(*of).and_then(|i| lines.get(*i)).map_or("another file", |line: &ImportLine| line.name.as_str());
                            (format!("the same file as {original}"), false)
                        }
                        FileState::InLibrary { .. } => ("already in the library".to_owned(), false),
                        FileState::Failed { reason } => (reason.clone(), true),
                    }
                }
                (Ok(_), _) => ("it wasn't imported".to_owned(), true),
            };
            if failed {
                retry.push(file.path);
            }
            lines.push(ImportLine { name: file.name, state, failed });
        }
        let read_failed = lines.iter().filter(|line| line.failed).count().saturating_sub(imported.failed().len());
        let summary = match (read_failed, imported.files.is_empty()) {
            (0, _) => imported.summary(),
            (n, true) => format!("{n} failed"),
            (n, false) => format!("{} · {n} couldn't be read", imported.summary()),
        };
        let nodes = imported.nodes();
        if !nodes.is_empty() {
            self.selection.replace(nodes.into_iter().collect());
        }
        self.notice = None;
        self.library.browsers.report = Some(ImportReport { summary, lines, retry });
    }
}

#[cfg(test)]
mod tests;

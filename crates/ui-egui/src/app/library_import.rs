//! The Assets browser's Import files… (#96): files picked by a person are read on their own
//! thread, never on a frame, and imported into the project's library as source cards in the
//! middle of the view, in one undo step, through [`nodecraft_engine::import::import_library`].
//!
//! Reading is bounded as a folder import's is: only regular files, each at most 16 MiB and
//! all of them at most 48 MiB, at most 32 files at a time, counted as they are read and
//! cancellable between files. Each file then has its own outcome: added, a duplicate, already
//! in the library, or failed with why. The paths of the failed files stay in this window's view
//! state, so "Retry failed" reads exactly those again; they never reach the document, which
//! keeps only a file's name, as its card's title. Between reading and importing, a video whose
//! headers don't state its length and size is probed by the video service, exactly as a dropped
//! one is, within the same bounded wait.
//!
//! Import from URL… fetches one file on its own thread, under `nodecraft_engine::url_import`'s
//! policy, over a transport that resolves names only to public addresses and connects to the ones
//! it checked (`ReqwestHttp::public_only`: no proxy, no cookies, the generic User-Agent). The
//! fetched bytes then take exactly the path a picked file takes. The address stays in this
//! thread and the fetch; only the file's name and host reach the report.
use super::NodeCraftApp;
use super::folder_import::{clean, read_bounded};
use super::library_browsers::{ImportLine, ImportReport};
use crate::video::import::{Probe, WAIT};
use crate::{ids, theme::Tokens};
use nodecraft_engine::import::{FileState, Incoming, MAX_FILES, MAX_PROJECT_MEDIA_BYTES, ProbedMovie, import_library};
use nodecraft_engine::url_import::Fetched;
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
    time::Instant,
};

type PickFuture = Pin<Box<dyn Future<Output = Option<Vec<PathBuf>>>>>;

/// One picked or fetched file, read or not.
struct ReadFile {
    /// Where a picked file is, for Retry failed; a fetched one has none.
    path: Option<PathBuf>,
    name: String,
    bytes: Result<Arc<[u8]>, String>,
}

/// Read files waiting for the video service to measure the videos their headers don't.
struct Probing {
    project: ProjectId,
    canvas: CanvasId,
    read: Vec<ReadFile>,
    movies: Vec<ProbedMovie>,
    next: usize,
    active: Option<Probe>,
    started: Instant,
}

impl Probing {
    /// Moves the probes along; `true` once every file is probed or the wait is over.
    fn poll(&mut self, videos: &mut crate::video::Videos, ctx: &egui::Context) -> bool {
        if self.started.elapsed() >= WAIT {
            self.active = None;
            return true;
        }
        loop {
            if let Some(probe) = &mut self.active {
                match probe.poll(videos, ctx) {
                    None => return false,
                    Some(movie) => {
                        self.movies.extend(movie);
                        self.active = None;
                    }
                }
            }
            let Some(file) = self.read.get(self.next) else { return true };
            self.next = self.next.saturating_add(1);
            if let Ok(bytes) = &file.bytes {
                self.active = Probe::new(Arc::clone(bytes));
            }
        }
    }
}

struct Reading {
    project: ProjectId,
    canvas: CanvasId,
    total: usize,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
    result: mpsc::Receiver<Result<Vec<ReadFile>, String>>,
}

/// A file being fetched from an address.
struct Fetching {
    project: ProjectId,
    canvas: CanvasId,
    host: String,
    cancel: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<Fetched, nodecraft_types::Error>>,
}

impl Drop for Fetching {
    fn drop(&mut self) {
        // Ending the fetch any way stops it at the next piece of the body.
        self.cancel.store(true, Ordering::Release);
    }
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
    fetching: Option<Fetching>,
    probing: Option<Probing>,
    /// What the file picker "chooses" in tests.
    #[cfg(test)]
    pub(super) pick: Option<Vec<PathBuf>>,
    /// In tests, the reader waits for a message before each file.
    #[cfg(test)]
    pub(super) hold: Option<mpsc::Receiver<()>>,
    /// In tests, what a fetch "downloads": nothing reaches the network.
    #[cfg(test)]
    pub(super) fetched: Option<Result<Fetched, nodecraft_types::Error>>,
}

impl LibraryImport {
    pub(super) fn busy(&self) -> bool {
        self.picking.is_some() || self.reading.is_some() || self.fetching.is_some() || self.probing.is_some()
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
        let bytes: Result<Arc<[u8]>, String> = if index >= MAX_FILES {
            Err(format!("at most {MAX_FILES} files are imported at once; retry to import it next"))
        } else {
            // The one Vec-to-Arc conversion runs here, never on a frame.
            read_bounded(&path, room, OVER_ROOM).map(Arc::from).map_err(str::to_owned)
        };
        if let Ok(bytes) = &bytes {
            room = room.saturating_sub(bytes.len() as u64);
        }
        done.fetch_add(1, Ordering::Relaxed);
        read.push(ReadFile { path: Some(path), name, bytes });
    }
    Ok(read)
}

impl NodeCraftApp {
    /// Opens the system's file picker for files to import into the library.
    pub(super) fn pick_library_files(&mut self) {
        if self.library.browsers.import.busy() {
            self.notices.warning("Files are already being imported; wait for them or cancel.");
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

    /// Starts reading `paths` on their own thread; `false` (with a notice) when it can't start.
    pub(super) fn read_library_files(&mut self, paths: Vec<PathBuf>) -> bool {
        if self.library.browsers.import.reading.is_some() || self.library.browsers.import.probing.is_some() {
            self.notices.warning("Files are already being imported; wait for them or cancel.");
            return false;
        }
        if paths.is_empty() {
            return false;
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
            self.notices.error("File reading is unavailable; restart the app to try again.");
            return false;
        }
        self.notices.clear();
        self.library.browsers.import.reading = Some(Reading { project: self.session.document().id(), canvas: self.canvas, total, cancel, done, result });
        true
    }

    /// Starts fetching the file at `address` on its own thread; `false` (with a notice) when it
    /// can't start. The address is checked first, so a refused one starts nothing.
    pub(super) fn fetch_library_url(&mut self, address: String) -> bool {
        if self.library.browsers.import.busy() {
            self.notices.warning("A file is already being imported; wait for it or cancel.");
            return false;
        }
        let host = match nodecraft_engine::url_import::check(&address) {
            Ok(checked) => checked.host(),
            Err(error) => {
                self.notices.error(format!("That address isn't imported: {}.", error.message));
                return false;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (send, result) = mpsc::channel();
        let stop = Arc::clone(&cancel);
        #[cfg(test)]
        let canned = self.library.browsers.import.fetched.take();
        let spawned = std::thread::Builder::new().name("nodecraft-library-fetch".into()).spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                #[cfg(test)]
                {
                    let _ = (&address, &stop);
                    canned.unwrap_or_else(|| Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Network, "no network in tests")))
                }
                #[cfg(not(test))]
                fetch(&address, &stop)
            }))
            .unwrap_or_else(|_| Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Internal, "the download stopped unexpectedly")));
            // The app may have ended the fetch; then nobody listens, which is fine.
            let _ = send.send(outcome);
        });
        if spawned.is_err() {
            self.notices.error("Downloading is unavailable; restart the app to try again.");
            return false;
        }
        self.notices.clear();
        self.library.browsers.import.fetching = Some(Fetching { project: self.session.document().id(), canvas: self.canvas, host, cancel, result });
        true
    }

    /// Moves a fetch along: once it is done, its file goes on to be probed and imported as a
    /// picked one does; meanwhile its progress shows with Cancel. `true` while it is running.
    fn poll_library_fetch(&mut self, ctx: &egui::Context, t: &Tokens) -> bool {
        let Some(fetching) = self.library.browsers.import.fetching.take() else { return false };
        let outcome = match fetching.result.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Internal, "the download stopped unexpectedly"))
            }
            Err(mpsc::TryRecvError::Empty) => {
                if progress(ctx, t, &format!("Downloading from {}…", sheet_line(&fetching.host)), None) {
                    // Dropping the fetch stops it at its next piece.
                    self.notices.info("Download cancelled; nothing was added.");
                } else {
                    self.library.browsers.import.fetching = Some(fetching);
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
                return true;
            }
        };
        let (project, canvas) = (fetching.project, fetching.canvas);
        let host = fetching.host.clone();
        drop(fetching);
        match outcome {
            Ok(fetched) => {
                let name = fetched.name;
                let bytes: Arc<[u8]> = Arc::from(fetched.bytes);
                let read = vec![ReadFile { path: None, name, bytes: Ok(bytes) }];
                self.library.browsers.import.probing =
                    Some(Probing { project, canvas, read, movies: Vec::new(), next: 0, active: None, started: Instant::now() });
                ctx.request_repaint();
            }
            Err(error) => {
                let line = ImportLine { name: format!("From {}", sheet_line(&host)), state: error.message.clone(), failed: true };
                self.library.browsers.report = Some(ImportReport { summary: "1 failed".into(), lines: vec![line], retry: Vec::new() });
                self.notices.error(format!("Nothing was imported: {}.", error.message));
            }
        }
        true
    }

    /// Polls the picker and the reader, shows the reading progress with its Cancel button, and
    /// imports the files once they are read.
    pub(super) fn poll_library_import(&mut self, ctx: &egui::Context, t: &Tokens) {
        if self.poll_library_fetch(ctx, t) {
            return;
        }
        if let Some(mut future) = self.library.browsers.import.picking.take() {
            let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
            let poll = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(&mut Context::from_waker(&waker))));
            match poll {
                Ok(Poll::Pending) => self.library.browsers.import.picking = Some(future),
                Ok(Poll::Ready(Some(paths))) => {
                    self.read_library_files(paths);
                }
                Ok(Poll::Ready(None)) => {}
                Err(_) => self.notices.error("The file picker could not be opened; try again."),
            }
        }
        if let Some(mut probing) = self.library.browsers.import.probing.take() {
            if probing.poll(&mut self.videos, ctx) {
                let Probing { project, canvas, read, movies, .. } = probing;
                self.import_read_files(project, canvas, read, &movies, t);
            } else {
                let cancel = progress(ctx, t, "Checking the videos' lengths…", None);
                if cancel {
                    self.notices.info("Import cancelled; nothing was added.");
                } else {
                    self.library.browsers.import.probing = Some(probing);
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
            }
            return;
        }
        let Some(reading) = self.library.browsers.import.reading.take() else { return };
        match reading.result.try_recv() {
            Ok(Ok(read)) => {
                let (project, canvas) = (reading.project, reading.canvas);
                drop(reading);
                self.library.browsers.import.probing =
                    Some(Probing { project, canvas, read, movies: Vec::new(), next: 0, active: None, started: Instant::now() });
                ctx.request_repaint();
                return;
            }
            Ok(Err(message)) => {
                self.notices.error(message);
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.notices.error("Reading the files failed unexpectedly; nothing was added.");
                return;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let done = reading.done.load(Ordering::Relaxed).min(reading.total);
        let ratio = if reading.total == 0 { 0.0 } else { done as f32 / reading.total as f32 };
        let cancel = progress(ctx, t, &format!("Reading files for the library… {done} of {}", reading.total), Some(ratio));
        if cancel {
            // Dropping the reading stops the reader at its next file.
            self.notices.info("Import cancelled; nothing was added.");
        } else {
            self.library.browsers.import.reading = Some(reading);
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn import_read_files(&mut self, project: ProjectId, canvas: CanvasId, read: Vec<ReadFile>, movies: &[ProbedMovie], t: &Tokens) {
        // Nothing was imported: every file is reported, with why, and can be retried.
        let failed_all = |read: Vec<ReadFile>, why: &str| ImportReport {
            summary: format!("{} failed", read.len()),
            lines: read.iter().map(|file| ImportLine { name: file.name.clone(), state: why.to_owned(), failed: true }).collect(),
            retry: read.into_iter().filter_map(|file| file.path).collect(),
        };
        if project != self.session.document().id() || canvas != self.canvas {
            let why = "the project or canvas changed while it was read";
            self.library.browsers.report = Some(failed_all(read, why));
            self.notices.warning("The project or canvas changed while the files were read; nothing was added. Retry to import them here.");
            return;
        }
        let centre = match self.browse_centre(t) {
            Ok(centre) => centre,
            Err(error) => {
                self.library.browsers.report = Some(failed_all(read, &error.message));
                self.notices.report(error);
                return;
            }
        };
        // The files that were read, with the card id each would get.
        let mut fresh: Vec<NodeId> = Vec::new();
        let mut taken = Vec::new();
        let mut allocated = true;
        for (index, file) in read.iter().enumerate() {
            if file.bytes.is_ok() {
                let id = ids::node_id(self.session.document(), ("library-import", self.session.revision(), index)).filter(|id| !fresh.contains(id));
                let Some(id) = id else {
                    allocated = false;
                    break;
                };
                fresh.push(id);
                taken.push(index);
            }
        }
        if !allocated {
            self.library.browsers.report = Some(failed_all(read, "couldn't allocate node ids; retry"));
            self.notices.error_for(crate::notices::CANVAS_ADD, "Couldn't allocate node ids; nothing was added. Retry the import.");
            return;
        }
        let incoming: Vec<Incoming<'_>> =
            read.iter().filter_map(|file| file.bytes.as_ref().ok().map(|bytes| Incoming { name: &file.name, bytes: &bytes[..] })).collect();
        let at = Position { x: centre.x, y: centre.y };
        let step = t.card_width + t.card_padding * 4.0;
        let imported = if incoming.is_empty() {
            Ok(Default::default())
        } else {
            import_library(&mut self.session, self.canvas, at, step, &incoming, &fresh, movies, &|node, kinds| Self::creation_position(node, kinds, t))
        };
        let imported = match imported {
            Ok(imported) => imported,
            Err(error) => {
                drop(incoming);
                self.notices.report(error.clone());
                self.library.browsers.report = Some(failed_all(read, &error.message));
                return;
            }
        };
        drop(incoming);
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
            if failed && let Some(path) = file.path {
                retry.push(path);
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
        self.notices.clear();
        self.library.browsers.report = Some(ImportReport { summary, lines, retry });
    }
}

fn sheet_line(text: &str) -> String {
    super::sheet_view::line(text)
}

/// Fetches `address` under the URL import policy on a runtime of this thread's own, over a
/// transport that reaches public addresses only.
#[cfg(not(test))]
fn fetch(address: &str, cancel: &AtomicBool) -> Result<Fetched, nodecraft_types::Error> {
    use nodecraft_types::{Error, ErrorKind};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| Error::new(ErrorKind::Unsupported, "downloading couldn't start; restart the app to try again"))?;
    let http =
        nodecraft_providers::http::ReqwestHttp::public_only().map_err(|_| Error::new(ErrorKind::Unsupported, "downloading is unavailable on this computer"))?;
    runtime.block_on(nodecraft_engine::url_import::fetch(&http, address, cancel))
}

/// The import's progress panel at the bottom left, with Cancel; whether Cancel was pressed.
fn progress(ctx: &egui::Context, t: &Tokens, text: &str, ratio: Option<f32>) -> bool {
    egui::Area::new(egui::Id::new("library_import_reading"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::LEFT_BOTTOM, egui::Vec2::new(t.card_padding, -t.card_padding * 6.0))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(t.panel_fill)
                .inner_margin(t.card_padding)
                .show(ui, |ui| {
                    ui.label(text);
                    if let Some(ratio) = ratio {
                        ui.add(egui::ProgressBar::new(ratio).desired_width(t.card_width));
                    }
                    ui.button("Cancel import").clicked()
                })
                .inner
        })
        .inner
}

#[cfg(test)]
mod tests;

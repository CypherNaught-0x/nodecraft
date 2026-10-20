//! Choosing a file for an App mode input: the native picker, then a read on its own thread, so
//! neither blocks the frame. Only regular files up to the per-file media limit are read.
use super::{AppMode, Form};
use crate::app::NodeCraftApp;
use nodecraft_engine::import::MAX_MEDIA_BYTES;
use std::{
    future::Future,
    io::Read,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, mpsc},
    task::{Context, Poll, Wake, Waker},
};

type Pick = Pin<Box<dyn Future<Output = Option<PathBuf>>>>;
type Answer = mpsc::Receiver<Result<(String, Vec<u8>), String>>;

pub(super) struct Loading {
    port: String,
    slot: Option<usize>,
    phase: Phase,
}

enum Phase {
    Picking(Pick),
    Reading(Answer),
}

impl Loading {
    pub(super) fn start(port: &str, slot: Option<usize>) -> Self {
        Self { port: port.to_owned(), slot, phase: Phase::Picking(pick()) }
    }
    pub(super) fn port(&self) -> &str {
        &self.port
    }
}

#[cfg(not(test))]
fn pick() -> Pick {
    // Built on the UI thread: AppKit finds its window here.
    let future = rfd::AsyncFileDialog::new().set_title("Choose a file").pick_file();
    Box::pin(async move { future.await.map(|file| file.path().to_path_buf()) })
}

/// Tests never open a real dialog: the pick is cancelled.
#[cfg(test)]
fn pick() -> Pick {
    Box::pin(std::future::ready(None))
}

struct Repaint(egui::Context);
impl Wake for Repaint {
    fn wake(self: Arc<Self>) {
        self.0.request_repaint();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.request_repaint();
    }
}

/// Reads `path` whole: a regular file no larger than one media asset may be.
fn read(path: &Path) -> Result<(String, Vec<u8>), String> {
    let name: String = path.file_name().map(|n| n.to_string_lossy().chars().take(80).collect()).unwrap_or_else(|| "The file".into());
    let unreadable = |_| format!("{name} can't be read.");
    // A stat never blocks on a special file; only a regular file is opened.
    let meta = std::fs::metadata(path).map_err(unreadable)?;
    if !meta.is_file() {
        return Err(format!("{name} isn't a file."));
    }
    let limit = u64::try_from(MAX_MEDIA_BYTES).unwrap_or(u64::MAX);
    if meta.len() > limit {
        return Err(format!("{name} is larger than the {} MiB a file can be.", MAX_MEDIA_BYTES / (1024 * 1024)));
    }
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let mut bytes = Vec::new();
    // Bounded even if the file grows while it is read.
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes).map_err(unreadable)?;
    if bytes.len() > MAX_MEDIA_BYTES {
        return Err(format!("{name} is larger than the {} MiB a file can be.", MAX_MEDIA_BYTES / (1024 * 1024)));
    }
    Ok((name, bytes))
}

impl NodeCraftApp {
    /// Moves a pick or read along; a read file goes into its input.
    pub(super) fn poll_app_file(&mut self, app: &mut AppMode, form: &Form, ctx: &egui::Context) {
        let Some(mut loading) = app.loading.take() else {
            return;
        };
        if !form.inputs.iter().any(|input| input.port == loading.port) {
            // The input went (undo, an upgrade): drop what was asked.
            return;
        }
        match loading.phase {
            Phase::Picking(ref mut future) => {
                let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
                let mut cx = Context::from_waker(&waker);
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(&mut cx))) {
                    Ok(Poll::Pending) => app.loading = Some(loading),
                    Ok(Poll::Ready(None)) => {}
                    Ok(Poll::Ready(Some(path))) => {
                        let (send, receive) = mpsc::channel();
                        let ctx = ctx.clone();
                        let spawned = std::thread::Builder::new().name("nodecraft-app-file".into()).spawn(move || {
                            // The form may have closed; then nobody is waiting.
                            let _ = send.send(read(&path));
                            ctx.request_repaint();
                        });
                        match spawned {
                            Ok(_) => app.loading = Some(Loading { phase: Phase::Reading(receive), ..loading }),
                            Err(_) => self.notice = Some("The file couldn't be read; try again.".into()),
                        }
                    }
                    Err(_) => self.notice = Some("The file picker failed; nothing was changed.".into()),
                }
            }
            Phase::Reading(ref receive) => match receive.try_recv() {
                Err(mpsc::TryRecvError::Empty) => app.loading = Some(loading),
                Err(mpsc::TryRecvError::Disconnected) => self.notice = Some("The file couldn't be read; try again.".into()),
                Ok(Err(message)) => self.notice = Some(message),
                Ok(Ok((name, bytes))) => self.app_fill_file(app, form, &loading.port, loading.slot, &name, &bytes),
            },
        }
    }
}

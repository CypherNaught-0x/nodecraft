//! Exporting a result to a file (#557): the native save dialog asks where (and whether to
//! replace a file already there), then `history.export` writes it. The dialog never blocks the
//! frame; the browser build says export needs the desktop app. Saving every item of a result
//! (#581) asks for a folder instead and never replaces a file already there.
use super::NodeCraftApp;
use nodecraft_types::{CanvasId, GenerationId, NodeId};
use serde_json::json;
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

type Pick = Pin<Box<dyn Future<Output = Option<PathBuf>>>>;

/// One result to export, while its save dialog is open.
struct Pending {
    /// The project the result belongs to; another opened meanwhile gets nothing written.
    session: Arc<()>,
    /// What to write: one file named by the dialog, or each item into the folder it picks.
    target: Target,
    future: Pick,
}

enum Target {
    File(serde_json::Value),
    /// `history.export` params with the suggested file name each is numbered from.
    Folder(Vec<(serde_json::Value, String)>),
}

/// Most numbers tried for one file before a folder export gives up on it.
const MAX_NUMBERS: usize = 1000;

/// The `history.export` params for item `item` of output `port` of a result.
fn export_params(canvas: CanvasId, id: NodeId, generation: GenerationId, port: Option<String>, item: Option<usize>) -> serde_json::Value {
    let mut params = json!({"canvas": canvas, "id": id, "generation": generation});
    if let Some(port) = port {
        params["port"] = json!(port);
    }
    if let Some(item) = item {
        params["item"] = json!(item);
    }
    params
}

/// `name` numbered `n`: `result.png` becomes `result-2.png`.
pub(super) fn numbered(name: &str, n: usize) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) => format!("{stem}-{n}.{extension}"),
        None => format!("{name}-{n}"),
    }
}

#[derive(Default)]
pub(super) struct Exports {
    pending: Option<Pending>,
    /// Tests answer the dialog with this path instead of opening one.
    #[cfg(test)]
    pub(super) answer: Option<PathBuf>,
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

impl NodeCraftApp {
    /// Asks where to save output `port` (item `item`) of result `generation` of node `id`,
    /// suggesting `name`.
    pub(super) fn start_export(&mut self, canvas: CanvasId, id: NodeId, generation: GenerationId, port: Option<String>, item: Option<usize>, name: &str) {
        let params = export_params(canvas, id, generation, port, item);
        self.pick_export(Target::File(params), name);
    }

    /// Asks for a folder and saves each of `targets` (output port, item, suggested name) of
    /// result `generation` of node `id` into it, numbered in order past files already there.
    pub(super) fn start_export_all(&mut self, canvas: CanvasId, id: NodeId, generation: GenerationId, targets: Vec<(Option<String>, Option<usize>, String)>) {
        let files = targets.into_iter().map(|(port, item, name)| (export_params(canvas, id, generation, port, item), name)).collect();
        self.pick_export(Target::Folder(files), "");
    }

    fn pick_export(&mut self, target: Target, name: &str) {
        if self.exports.pending.is_some() {
            self.notice = Some("Another export is choosing where to save; finish that first.".into());
            return;
        }
        let folder = matches!(target, Target::Folder(_));
        let future = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.export_pick(name, folder))) {
            Ok(Some(future)) => future,
            Ok(None) => {
                self.notice = Some("Exporting to a file works in the desktop app.".into());
                return;
            }
            Err(_) => {
                self.notice = Some("The save dialog failed; nothing was exported.".into());
                return;
            }
        };
        self.exports.pending = Some(Pending { session: self.session.view_identity(), target, future });
    }

    #[cfg(all(not(test), not(target_arch = "wasm32")))]
    fn export_pick(&self, name: &str, folder: bool) -> Option<Pick> {
        // Built on the UI thread: AppKit finds its window here. The dialog asks before replacing.
        let dialog = rfd::AsyncFileDialog::new();
        if folder {
            let future = dialog.set_title("Export results to a folder").pick_folder();
            return Some(Box::pin(async move { future.await.map(|folder| folder.path().to_path_buf()) }));
        }
        let future = dialog.set_title("Export result").set_file_name(name).save_file();
        Some(Box::pin(async move { future.await.map(|file| file.path().to_path_buf()) }))
    }
    #[cfg(all(not(test), target_arch = "wasm32"))]
    fn export_pick(&self, _: &str, _: bool) -> Option<Pick> {
        None
    }
    #[cfg(test)]
    fn export_pick(&self, _: &str, _: bool) -> Option<Pick> {
        Some(Box::pin(std::future::ready(self.exports.answer.clone())))
    }

    /// Moves an open save dialog along; a chosen path is written at once.
    pub(super) fn poll_export(&mut self, ctx: &egui::Context) {
        let Some(mut pending) = self.exports.pending.take() else {
            return;
        };
        let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
        let mut cx = Context::from_waker(&waker);
        let path = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pending.future.as_mut().poll(&mut cx))) {
            Ok(Poll::Pending) => {
                self.exports.pending = Some(pending);
                return;
            }
            Ok(Poll::Ready(None)) => return,
            Ok(Poll::Ready(Some(path))) => path,
            Err(_) => {
                self.notice = Some("The save dialog failed; nothing was exported.".into());
                return;
            }
        };
        if !Arc::ptr_eq(&pending.session, &self.session.view_identity()) {
            self.notice = Some("Another project opened while choosing where to save; nothing was exported.".into());
            return;
        }
        match pending.target {
            Target::File(mut params) => {
                params["path"] = json!(path);
                // The dialog already asked before replacing a file.
                params["overwrite"] = json!(true);
                match self.session.dispatch("history.export", &params) {
                    Ok(_) => {
                        let name: String = path.file_name().map(|n| n.to_string_lossy().chars().take(80).collect()).unwrap_or_default();
                        self.notice = Some(format!("Exported {name}."));
                    }
                    Err(error) => self.notice = Some(error.message),
                }
            }
            Target::Folder(files) => {
                let total = files.len();
                let mut written = 0usize;
                let mut failed = None;
                // One counter for the whole folder: a name already taken moves on to the next
                // number, so saving into the same folder again adds files instead of failing.
                let mut next = 1usize;
                for (mut params, base) in files {
                    let mut outcome = Err(format!("{base}: no free number left for it"));
                    for _ in 0..MAX_NUMBERS {
                        let name = numbered(&base, next);
                        next = next.saturating_add(1);
                        // A folder pick never agreed to replace anything, so a file there is kept.
                        params["path"] = json!(path.join(&name));
                        match self.session.dispatch("history.export", &params) {
                            Ok(_) => outcome = Ok(()),
                            Err(error) if error.kind == nodecraft_types::ErrorKind::Conflict => continue,
                            Err(error) => outcome = Err(format!("{name}: {}", error.message)),
                        }
                        break;
                    }
                    match outcome {
                        Ok(()) => written = written.saturating_add(1),
                        Err(why) => {
                            failed.get_or_insert(why);
                        }
                    }
                }
                self.notice = Some(match failed {
                    None => format!("Exported {written} files."),
                    Some(why) => format!("Exported {written} of {total} files; {why}"),
                });
            }
        }
    }
}

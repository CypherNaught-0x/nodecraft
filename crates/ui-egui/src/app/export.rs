//! Exporting a result to a file (#557): the native save dialog asks where (and whether to
//! replace a file already there), then `history.export` writes it. The dialog never blocks the
//! frame; the browser build says export needs the desktop app.
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
    params: serde_json::Value,
    future: Pick,
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
        if self.exports.pending.is_some() {
            self.notice = Some("Another export is choosing where to save; finish that first.".into());
            return;
        }
        let future = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.export_pick(name))) {
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
        let mut params = json!({"canvas": canvas, "id": id, "generation": generation});
        if let Some(port) = port {
            params["port"] = json!(port);
        }
        if let Some(item) = item {
            params["item"] = json!(item);
        }
        self.exports.pending = Some(Pending { session: self.session.view_identity(), params, future });
    }

    #[cfg(all(not(test), not(target_arch = "wasm32")))]
    fn export_pick(&self, name: &str) -> Option<Pick> {
        // Built on the UI thread: AppKit finds its window here. The dialog asks before replacing.
        let future = rfd::AsyncFileDialog::new().set_title("Export result").set_file_name(name).save_file();
        Some(Box::pin(async move { future.await.map(|file| file.path().to_path_buf()) }))
    }
    #[cfg(all(not(test), target_arch = "wasm32"))]
    fn export_pick(&self, _: &str) -> Option<Pick> {
        None
    }
    #[cfg(test)]
    fn export_pick(&self, _: &str) -> Option<Pick> {
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
        pending.params["path"] = json!(path);
        // The dialog already asked before replacing a file.
        pending.params["overwrite"] = json!(true);
        match self.session.dispatch("history.export", &pending.params) {
            Ok(_) => {
                let name: String = path.file_name().map(|n| n.to_string_lossy().chars().take(80).collect()).unwrap_or_default();
                self.notice = Some(format!("Exported {name}."));
            }
            Err(error) => self.notice = Some(error.message),
        }
    }
}

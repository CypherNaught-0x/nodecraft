//! A captured canvas paste target, shared by desktop and browser image acquisition.
use super::NodeCraftApp;
use crate::{ids, theme::Tokens};
use nodecraft_engine::import::Incoming;
use nodecraft_graph::Position;
use std::sync::Arc;

pub(super) struct Target {
    pub identity: Arc<crate::navigation::SessionIdentity>,
    pub project: nodecraft_types::ProjectId,
    pub canvas: nodecraft_types::CanvasId,
    pub revision: u64,
    pub position: Position,
    #[cfg(not(target_arch = "wasm32"))]
    pub step: f32,
}
impl Target {
    pub(super) fn capture(app: &mut NodeCraftApp, centre: egui::Pos2, t: &Tokens) -> Self {
        Self {
            identity: crate::navigation::Geometry::identity(&mut app.session),
            project: app.session.document().id(),
            canvas: app.canvas,
            revision: app.session.revision(),
            position: Position { x: centre.x - t.card_width * 0.5, y: centre.y - t.card_min_height * 0.5 },
            #[cfg(not(target_arch = "wasm32"))]
            step: t.card_width + t.card_padding * 4.0,
        }
    }
    pub(super) fn current(&self, app: &mut NodeCraftApp) -> bool {
        self.canvas == app.canvas
            && self.project == app.session.document().id()
            && self.revision == app.session.revision()
            && Arc::ptr_eq(&self.identity, &crate::navigation::Geometry::identity(&mut app.session))
    }
}
impl NodeCraftApp {
    pub(super) fn poll_paste_media(&mut self, ctx: &egui::Context) {
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_image_paste(ctx);
        #[cfg(any(test, target_arch = "wasm32"))]
        self.poll_browser_paste(ctx);
    }

    pub(super) fn paste_media_keys(&mut self, ui: &egui::Ui, t: &Tokens, ready: bool) {
        #[cfg(any(test, target_arch = "wasm32"))]
        if self.browser_paste_keys(ui, t, ready) {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.image_paste_keys(ui, t, ready);
    }

    #[cfg(target_arch = "wasm32")]
    pub(super) fn paste_plain_text(&mut self, _ui: &egui::Ui, _t: &Tokens, text: String, centre: egui::Pos2) {
        self.paste_text(&text, centre);
    }

    pub(super) fn import_pasted_png(&mut self, target: Target, png: &[u8]) {
        if !target.current(self) {
            self.notice = Some("The document changed while reading the clipboard; nothing was added. Paste again.".into());
            return;
        }
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            self.notice = Some("The copied image isn't a PNG; nothing was pasted.".into());
            return;
        }
        let at = target.position;
        let Some(id) = ids::node_id(self.session.document(), ("clipboard-image", self.session.revision(), at.x.to_bits(), at.y.to_bits())) else {
            self.notice = Some("Couldn't allocate a node id; try again.".into());
            return;
        };
        let incoming = [Incoming { name: "Pasted image.png", bytes: png }];
        match nodecraft_engine::import::import_files(&mut self.session, target.canvas, at, 0.0, &incoming, &[id]) {
            Ok(imported) => {
                if let Some((_, why)) = imported.skipped.first() {
                    self.notice = Some(format!("The copied image wasn't added: {why}."));
                } else {
                    self.selection.replace(imported.nodes.iter().copied().collect());
                    self.notice = None;
                }
            }
            Err(error) => self.notice = Some(error.message),
        }
    }
}

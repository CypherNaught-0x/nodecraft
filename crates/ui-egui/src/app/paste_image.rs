//! Pasting a copied image as an Image node (#319). egui-winit 0.36 turns ⌘V into a text paste
//! and reports nothing for the press when the clipboard holds no text, but it still reports the
//! key's release. A ⌘V release with no text paste since the last one reads the image through the
//! host clipboard off the UI thread; a later frame imports it through the same path as a drop.
//! ⌘ is often let go a moment before V, so a V release whose press was never reported counts too
//! when ⌘ was held just before it.
use super::NodeCraftApp;
use crate::{ids, theme::Tokens};
use egui::Pos2;
use nodecraft_engine::import::{Incoming, MAX_MEDIA_BYTES};
use nodecraft_graph::Position;
use std::sync::{Arc, mpsc};

/// The largest side a pasted image may have, as for any image NodeCraft decodes.
const MAX_SIDE: u32 = 16_384;
/// The most pixels a pasted image may have (8192×8192): the RGBA buffer is copied once to encode
/// it, so this bounds that peak to about 512 MiB. The host clipboard's own buffer can't be capped.
const MAX_PIXELS: usize = 8192 * 8192;
/// How long after ⌘ was last held a V release whose press went unreported still pastes, in seconds.
const COMMAND_GRACE: f64 = 0.5;

/// Pixels read from the clipboard: 8-bit RGBA, row-major.
pub(super) struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Reads the clipboard's image: `Ok(None)` when it holds none. Runs on a worker thread.
pub(super) type Reader = Arc<dyn Fn() -> Result<Option<Picture>, String> + Send + Sync>;

struct Pending {
    rx: mpsc::Receiver<Result<Option<Vec<u8>>, String>>,
    identity: Arc<crate::navigation::SessionIdentity>,
    project: nodecraft_types::ProjectId,
    canvas: nodecraft_types::CanvasId,
    revision: u64,
    position: Position,
}
impl Pending {
    fn current(&self, app: &mut NodeCraftApp) -> bool {
        self.canvas == app.canvas
            && self.project == app.session.document().id()
            && self.revision == app.session.revision()
            && Arc::ptr_eq(&self.identity, &crate::navigation::Geometry::identity(&mut app.session))
    }
}

pub(super) struct ImagePaste {
    pub(super) reader: Reader,
    pending: Option<Pending>,
    /// A text paste arrived since the last ⌘V release, so that release was a text paste.
    text_seen: bool,
    /// V's press was reported (typed, not a paste), so its release isn't a paste.
    v_down: bool,
    /// When ⌘ was last seen held, in egui input time.
    command_at: Option<f64>,
}
impl ImagePaste {
    pub(super) fn new() -> Self {
        Self { reader: Arc::new(system), pending: None, text_seen: false, v_down: false, command_at: None }
    }

    /// Reads this frame's events: whether a ⌘V was released (a text paste latches `text_seen`).
    fn released(&mut self, input: &egui::InputState) -> bool {
        let mut released = false;
        for event in &input.events {
            match event {
                egui::Event::Paste(_) => self.text_seen = true,
                egui::Event::Key { key: egui::Key::V, pressed: true, .. } => self.v_down = true,
                egui::Event::Key { key: egui::Key::V, pressed: false, modifiers, .. } => {
                    let recent = self.command_at.is_some_and(|at| input.time - at <= COMMAND_GRACE);
                    released |= modifiers.command || (!self.v_down && recent);
                    self.v_down = false;
                }
                _ => {}
            }
            if let egui::Event::Key { modifiers, .. } = event
                && modifiers.command
            {
                self.command_at = Some(input.time);
            }
        }
        if input.modifiers.command {
            self.command_at = Some(input.time);
        }
        released
    }
}

/// The OS clipboard, through `arboard`.
fn system() -> Result<Option<Picture>, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| {
        log::warn!("clipboard unavailable: {e}");
        "The clipboard can't be opened; nothing was pasted.".to_owned()
    })?;
    match clipboard.get_image() {
        Ok(image) => Ok(Some(Picture { width: image.width, height: image.height, rgba: image.bytes.into_owned() })),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(e) => {
            log::warn!("clipboard image unreadable: {e}");
            Err("The copied image can't be read; nothing was pasted.".to_owned())
        }
    }
}

/// The picture as PNG bytes within the import caps.
fn encode(picture: &Picture) -> Result<Vec<u8>, String> {
    let too_big = || format!("Copied images are at most {MAX_SIDE}×{MAX_SIDE} pixels; nothing was pasted.");
    let width = u32::try_from(picture.width).ok().filter(|w| *w <= MAX_SIDE).ok_or_else(too_big)?;
    let height = u32::try_from(picture.height).ok().filter(|h| *h <= MAX_SIDE).ok_or_else(too_big)?;
    if picture.width.checked_mul(picture.height).is_none_or(|pixels| pixels > MAX_PIXELS) {
        return Err(format!("Copied images are at most {} megapixels; nothing was pasted.", MAX_PIXELS / 1_000_000));
    }
    let limits = nodecraft_media::Limits { max_width: MAX_SIDE, max_height: MAX_SIDE, ..Default::default() };
    let png = nodecraft_media::encode_png(width, height, &picture.rgba, &limits).map_err(|e| {
        log::warn!("clipboard image not encodable: {e}");
        "The copied image can't be read; nothing was pasted.".to_owned()
    })?;
    if png.len() > MAX_MEDIA_BYTES {
        return Err(format!("The copied image is over {} MiB as PNG; nothing was pasted.", MAX_MEDIA_BYTES / (1024 * 1024)));
    }
    Ok(png)
}

impl NodeCraftApp {
    /// Watches this frame's keys for a ⌘V without text. `ready` is false while something else
    /// owns the canvas keys.
    pub(super) fn image_paste_keys(&mut self, ui: &egui::Ui, t: &Tokens, ready: bool) {
        let released = ui.input(|input| self.image_paste.released(input));
        if !released {
            return;
        }
        let was_text = std::mem::take(&mut self.image_paste.text_seen);
        if was_text || !ready {
            return;
        }
        match self.paste_target(ui, t) {
            Some(centre) => self.start_image_paste(ui.ctx(), centre, t),
            None => self.notice = Some("There is no unobscured canvas space for this paste.".into()),
        }
    }

    fn start_image_paste(&mut self, ctx: &egui::Context, centre: Pos2, t: &Tokens) {
        if self.image_paste.pending.is_some() {
            self.notice = Some("The copied image is still being read; try again in a moment.".into());
            return;
        }
        let (tx, rx) = mpsc::sync_channel(1);
        let reader = Arc::clone(&self.image_paste.reader);
        let repaint = ctx.clone();
        let spawned = std::thread::Builder::new().name("clipboard-image".into()).spawn(move || {
            let result = reader().and_then(|picture| picture.map(|p| encode(&p)).transpose());
            let _ = tx.send(result);
            repaint.request_repaint();
        });
        if spawned.is_err() {
            self.notice = Some("The clipboard can't be read right now; nothing was pasted.".into());
            return;
        }
        let identity = crate::navigation::Geometry::identity(&mut self.session);
        self.image_paste.pending = Some(Pending {
            rx,
            identity,
            project: self.session.document().id(),
            canvas: self.canvas,
            revision: self.session.revision(),
            position: Position { x: centre.x - t.card_width * 0.5, y: centre.y - t.card_min_height * 0.5 },
        });
        self.notice = Some("Reading the copied image…".into());
    }

    #[cfg(test)]
    pub(super) fn image_paste_pending(&self) -> bool {
        self.image_paste.pending.is_some()
    }

    /// Imports a finished read, before canvas input.
    pub(super) fn poll_image_paste(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.image_paste.pending.take() else {
            return;
        };
        let result = match pending.rx.try_recv() {
            Err(mpsc::TryRecvError::Empty) => {
                self.image_paste.pending = Some(pending);
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => Err("Reading the clipboard failed unexpectedly; nothing was pasted.".to_owned()),
            Ok(result) => result,
        };
        let png = match result {
            Ok(Some(png)) => png,
            Ok(None) => {
                self.notice = Some("The clipboard holds no image or text to paste.".into());
                return;
            }
            Err(message) => {
                self.notice = Some(message);
                return;
            }
        };
        if !pending.current(self) {
            self.notice = Some("The document changed while reading the copied image; nothing was added. Paste again.".into());
            return;
        }
        let at = pending.position;
        let Some(id) = ids::node_id(self.session.document(), ("clipboard-image", self.session.revision(), at.x.to_bits(), at.y.to_bits())) else {
            self.notice = Some("Couldn't allocate a node id; try again.".into());
            return;
        };
        let incoming = [Incoming { name: "Pasted image.png", bytes: &png }];
        match nodecraft_engine::import::import_files(&mut self.session, pending.canvas, at, 0.0, &incoming, &[id]) {
            Ok(imported) => {
                if let Some((_, why)) = imported.skipped.first() {
                    self.notice = Some(format!("The copied image wasn't added: {why}."));
                } else {
                    self.selection.replace(imported.nodes.iter().copied().collect());
                    self.notice = None;
                }
            }
            Err(e) => self.notice = Some(e.message),
        }
    }
}

#[cfg(test)]
mod tests;

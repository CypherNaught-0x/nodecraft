//! DOM paste events carry no document authority. The next admitted canvas frame starts the read.
use super::{NodeCraftApp, paste_media::Target};
use crate::theme::Tokens;
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

#[cfg(target_arch = "wasm32")]
mod browser;
const READ_SECONDS: f64 = 10.0;
const READING: &str = "The browser is still reading the clipboard; close its permission request before pasting again.";
const DENIED: &str = "The browser couldn't read the clipboard. Allow clipboard access for this site and paste again.";
const NO_PNG: &str = "The clipboard holds no PNG image to paste.";

struct ReadState {
    reads: Rc<RefCell<Vec<(String, super::drop::web::Slot)>>>,
    #[cfg(target_arch = "wasm32")]
    _reader: Option<super::drop::web::reader::Reader>,
}
impl ReadState {
    fn new() -> Self {
        Self {
            reads: Rc::new(RefCell::new(vec![("Pasted image.png".into(), None)])),
            #[cfg(target_arch = "wasm32")]
            _reader: None,
        }
    }
    fn complete(state: &Weak<RefCell<Self>>, result: Result<Vec<u8>, String>) {
        if let Some(state) = state.upgrade()
            && let Ok(state) = state.try_borrow()
            && let Ok(mut reads) = state.reads.try_borrow_mut()
            && let Some((_, slot)) = reads.first_mut()
        {
            *slot = Some(result);
        }
    }
}
struct Pending {
    state: Rc<RefCell<ReadState>>,
    target: Target,
    deadline: f64,
}
#[cfg(not(target_arch = "wasm32"))]
type TestReader = Rc<dyn Fn(Weak<RefCell<ReadState>>, Rc<Cell<bool>>)>;

#[derive(Default)]
pub(super) struct ClipboardPaste {
    inbox: Rc<Cell<bool>>,
    requested: bool,
    pending: Option<Pending>,
    busy: Rc<Cell<bool>>,
    #[cfg(target_arch = "wasm32")]
    listener: Option<browser::Listener>,
    #[cfg(target_arch = "wasm32")]
    installed: bool,
    #[cfg(not(target_arch = "wasm32"))]
    reader: Option<TestReader>,
}
impl ClipboardPaste {
    fn start(&mut self, ctx: &egui::Context, target: Target) -> Result<(), String> {
        if self.pending.is_some() || self.busy.get() {
            return Err(READING.into());
        }
        let now = ctx.input(|i| i.time);
        if !now.is_finite() {
            return Err("The browser clock isn't available; paste again.".into());
        }
        let state = Rc::new(RefCell::new(ReadState::new()));
        self.busy.set(true);
        #[cfg(target_arch = "wasm32")]
        let started = browser::read(Rc::downgrade(&state), Rc::clone(&self.busy), ctx.clone());
        #[cfg(not(target_arch = "wasm32"))]
        let started = self.reader.as_ref().ok_or_else(|| DENIED.to_owned()).map(|reader| reader(Rc::downgrade(&state), Rc::clone(&self.busy)));
        if let Err(error) = started {
            self.busy.set(false);
            return Err(error);
        }
        self.pending = Some(Pending { state, target, deadline: now + READ_SECONDS });
        ctx.request_repaint();
        Ok(())
    }
}
impl NodeCraftApp {
    pub(super) fn browser_paste_frame(&mut self, _ctx: &egui::Context) {
        #[cfg(target_arch = "wasm32")]
        if !self.web_paste.installed {
            self.web_paste.installed = true;
            match browser::Listener::new(Rc::clone(&self.web_paste.inbox), _ctx.clone()) {
                Ok(listener) => self.web_paste.listener = Some(listener),
                Err(error) => self.notice = Some(error),
            }
        }
        // Always drain at frame entry, including frames where an editor owns canvas input.
        self.web_paste.requested = self.web_paste.inbox.replace(false);
        if self.on_sheet() {
            self.web_paste.requested = false;
            self.web_paste.pending = None;
        }
    }

    pub(super) fn poll_browser_paste(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.web_paste.pending.take() else { return };
        if !pending.target.current(self) {
            self.notice = Some("The document changed while reading the clipboard; nothing was added. Paste again.".into());
            return;
        }
        let result = pending
            .state
            .try_borrow()
            .ok()
            .and_then(|state| state.reads.try_borrow_mut().ok().and_then(|mut reads| reads.first_mut().and_then(|(_, slot)| slot.take())));
        let Some(result) = result else {
            // Read callbacks may finish while the browser suspends animation frames. Only
            // a still-unsettled read expires when the app next gets a frame.
            let now = ctx.input(|i| i.time);
            if !now.is_finite() || now >= pending.deadline {
                self.notice = Some("The browser didn't finish reading the clipboard; nothing was pasted. Close its permission request and try again.".into());
                return;
            }
            self.web_paste.pending = Some(pending);
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        };
        self.web_paste.busy.set(false);
        let Pending { target, state, .. } = pending;
        drop(state); // Abort/release the browser buffer before synchronous engine decoding.
        match result {
            Ok(png) => self.import_pasted_png(target, &png),
            Err(error) => self.notice = Some(error),
        }
    }

    pub(super) fn browser_paste_keys(&mut self, ui: &egui::Ui, t: &Tokens, ready: bool) -> bool {
        if !std::mem::take(&mut self.web_paste.requested) {
            return false;
        }
        if !ready || ui.input(|i| i.events.iter().any(|event| matches!(event, egui::Event::Paste(text) if !text.is_empty()))) {
            return false;
        }
        let Some(centre) = self.paste_target(ui, t) else {
            self.notice = Some("There is no unobscured canvas space for this paste.".into());
            return true;
        };
        let target = Target::capture(self, centre, t);
        self.notice = Some(match self.web_paste.start(ui.ctx(), target) {
            Ok(()) => "Reading the copied image…".into(),
            Err(error) => error,
        });
        true
    }
}

#[cfg(test)]
mod tests;

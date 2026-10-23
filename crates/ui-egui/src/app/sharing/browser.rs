//! The connection to a relay in the browser (#699): the page's `WebSocket`, whose frames come in
//! on the page's queue between the app's frames. As on the desktop it reconnects by itself,
//! waiting longer each time up to [`MAX_BACKOFF`], until the app drops it; each connection has a
//! number, and frames sent for an earlier one are dropped.
//!
//! The browser answers the relay's pings itself and never shows them, so a connection that dies
//! quietly is noticed when the browser closes it rather than after a set silence. Nor can it stop
//! reading: a relay that sends more than the app takes in is dropped, and connected to again.
//!
//! A hidden tab gets no frames, so it can't take in what the relay sends: the connection is closed
//! while the tab is hidden and opened again when it is shown.
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use eframe::wasm_bindgen::{JsCast as _, closure::Closure};
use nodecraft_sync::MAX_FRAME_BYTES;
use nodecraft_types::Error;
use web_sys::{BinaryType, CloseEvent, MessageEvent, WebSocket};

use super::Event;

/// The longest wait between tries.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Bytes of frames waiting for the app; a relay that sends more before the app takes them is
/// dropped, so a flood can't fill memory. The same as the desktop's.
const INBOX_BYTES: usize = 2 * MAX_FRAME_BYTES;
/// Bytes the app takes from the inbox in one frame; the rest wait for the next.
const TAKE_BYTES: usize = MAX_FRAME_BYTES;
/// What a frame costs in the inbox beyond its bytes, so empty frames count too.
const FRAME_COST: usize = 64;

/// The app's end. Dropping it closes the socket and stops the retries.
pub(super) struct Connection {
    state: Rc<RefCell<State>>,
    /// The page's `visibilitychange` handler, taken off when this is dropped.
    visibility: Option<(web_sys::Document, Closure<dyn FnMut()>)>,
}

struct State {
    relay: String,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// The number of the connection last opened.
    n: u64,
    /// The socket, opening or open; `open` once it is.
    socket: Option<Socket>,
    open: bool,
    /// A socket given up, kept until its handlers can no longer be running.
    retired: Option<Socket>,
    /// The next try.
    timer: Option<Timer>,
    backoff: Duration,
    events: VecDeque<Event>,
    /// Bytes of frames in `events`.
    queued: usize,
    /// The tab is hidden: no socket, and no tries until it is shown.
    hidden: bool,
}

/// A `WebSocket` and the handlers it calls, which hold only a weak reference to the state.
struct Socket {
    socket: WebSocket,
    _open: Closure<dyn FnMut()>,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _close: Closure<dyn FnMut(CloseEvent)>,
}

impl Socket {
    /// Closes it, calling nothing more. Its handlers stay alive until it is dropped.
    fn detach(&self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        // Closing an already closed socket is no error worth telling.
        let _ = self.socket.close();
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.detach();
    }
}

/// A `setTimeout`, cleared if it hasn't fired when dropped.
struct Timer {
    id: i32,
    _fire: Closure<dyn FnMut()>,
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(window) = web_sys::window() {
            window.clear_timeout_with_handle(self.id);
        }
    }
}

impl Connection {
    /// Connects to `relay` (a checked `ws://` or `wss://` address); `wake` asks for a frame when
    /// something comes in. A page served over https may reach only a `wss://` relay.
    pub(super) fn open(relay: String, wake: impl Fn() + Send + Sync + 'static) -> Result<Self, Error> {
        super::reachable(&relay)?;
        let document = web_sys::window().and_then(|w| w.document());
        let state = Rc::new(RefCell::new(State {
            relay,
            wake: Arc::new(wake),
            n: 0,
            socket: None,
            open: false,
            retired: None,
            timer: None,
            backoff: Duration::from_secs(1),
            events: VecDeque::new(),
            queued: 0,
            hidden: document.as_ref().is_some_and(web_sys::Document::hidden),
        }));
        let visibility = document.and_then(|document| {
            let weak = Rc::downgrade(&state);
            let changed = Closure::<dyn FnMut()>::new(move || shown_or_hidden(&weak));
            document.add_event_listener_with_callback("visibilitychange", changed.as_ref().unchecked_ref()).ok()?;
            Some((document, changed))
        });
        connect(&state);
        Ok(Self { state, visibility })
    }

    /// Sends `frame` on connection `n`, if it is still the open one.
    pub(super) fn send(&self, n: u64, frame: Vec<u8>) {
        let Ok(state) = self.state.try_borrow() else { return };
        let Some(socket) = state.socket.as_ref().filter(|_| state.open && state.n == n) else { return };
        if socket.socket.send_with_u8_array(&frame).is_err() {
            drop(state);
            lost(&self.state, "the connection to the relay was lost");
        }
    }

    /// What came in since the last call, up to [`TAKE_BYTES`] of frames; more asks for another
    /// frame.
    pub(super) fn events(&self) -> Vec<Event> {
        let Ok(mut state) = self.state.try_borrow_mut() else { return Vec::new() };
        let mut events = Vec::new();
        let mut taken = 0usize;
        while taken < TAKE_BYTES {
            let Some(event) = state.events.pop_front() else { return events };
            let cost = cost(&event);
            state.queued = state.queued.saturating_sub(cost);
            taken = taken.saturating_add(cost);
            events.push(event);
        }
        let wake = Arc::clone(&state.wake);
        drop(state);
        wake();
        events
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some((document, changed)) = &self.visibility {
            let _ = document.remove_event_listener_with_callback("visibilitychange", changed.as_ref().unchecked_ref());
        }
        if let Ok(mut state) = self.state.try_borrow_mut() {
            state.timer = None;
            state.socket = None;
            state.retired = None;
        }
    }
}

fn cost(event: &Event) -> usize {
    match event {
        Event::Frame(bytes) => bytes.len().saturating_add(FRAME_COST),
        _ => 0,
    }
}

/// Opens a socket to the relay, or tells why it can't and tries again later.
fn connect(state: &Rc<RefCell<State>>) {
    let Ok(mut held) = state.try_borrow_mut() else { return };
    held.retired = None;
    if held.hidden {
        return;
    }
    let socket = match WebSocket::new(&held.relay) {
        Ok(socket) => socket,
        Err(_) => {
            drop(held);
            return lost(state, "the browser won't connect to this relay address");
        }
    };
    socket.set_binary_type(BinaryType::Arraybuffer);
    let weak = Rc::downgrade(state);
    let on_open = Closure::<dyn FnMut()>::new(move || opened(&weak));
    let weak = Rc::downgrade(state);
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| received(&weak, &event));
    let weak = Rc::downgrade(state);
    let on_close = Closure::<dyn FnMut(CloseEvent)>::new(move |_: CloseEvent| {
        if let Some(state) = weak.upgrade() {
            let was_open = state.try_borrow().is_ok_and(|s| s.open);
            lost(&state, if was_open { "the relay closed the connection" } else { "the relay can't be reached" });
        }
    });
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    held.open = false;
    held.socket = Some(Socket { socket, _open: on_open, _message: on_message, _close: on_close });
}

fn opened(state: &Weak<RefCell<State>>) {
    let Some(state) = state.upgrade() else { return };
    let Ok(mut held) = state.try_borrow_mut() else { return };
    held.n = held.n.saturating_add(1);
    held.open = true;
    held.backoff = Duration::from_secs(1);
    let n = held.n;
    held.events.push_back(Event::Connected(n));
    let wake = Arc::clone(&held.wake);
    drop(held);
    wake();
}

fn received(state: &Weak<RefCell<State>>, event: &MessageEvent) {
    let Some(state) = state.upgrade() else { return };
    // Frames are binary; anything else isn't the relay's to send, and is passed over.
    let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() else { return };
    let array = js_sys::Uint8Array::new(&buffer);
    let len = usize::try_from(array.length()).unwrap_or(usize::MAX);
    if len > MAX_FRAME_BYTES {
        return lost(&state, "the relay sent more than a frame may hold");
    }
    let Ok(mut held) = state.try_borrow_mut() else { return };
    if !held.open {
        return;
    }
    let cost = len.saturating_add(FRAME_COST);
    if held.queued.saturating_add(cost) > INBOX_BYTES {
        drop(held);
        return lost(&state, "the relay sent more than NodeCraft could take in");
    }
    held.queued = held.queued.saturating_add(cost);
    held.events.push_back(Event::Frame(array.to_vec()));
    let wake = Arc::clone(&held.wake);
    drop(held);
    wake();
}

/// The tab was hidden or shown: closes the connection, or opens it again at once.
fn shown_or_hidden(state: &Weak<RefCell<State>>) {
    let Some(state) = state.upgrade() else { return };
    let hidden = web_sys::window().and_then(|w| w.document()).is_some_and(|d| d.hidden());
    let Ok(mut held) = state.try_borrow_mut() else { return };
    if held.hidden == hidden {
        return;
    }
    held.hidden = hidden;
    if hidden {
        let had = held.socket.is_some();
        drop(held);
        if had {
            lost(&state, "this tab is hidden; sharing picks up again when it is shown");
        }
    } else {
        held.timer = None;
        held.backoff = Duration::from_secs(1);
        drop(held);
        connect(&state);
    }
}

/// The socket closed or couldn't open: says why, and tries again after the backoff.
fn lost(state: &Rc<RefCell<State>>, why: &str) {
    let Ok(mut held) = state.try_borrow_mut() else { return };
    // Its handlers may be what is running: it is let go once they can't be.
    held.retired = held.socket.take();
    if let Some(socket) = &held.retired {
        socket.detach();
    }
    held.open = false;
    let retry = held.backoff;
    held.events.push_back(Event::Dropped { why: why.to_owned(), retry });
    held.backoff = held.backoff.saturating_mul(2).min(MAX_BACKOFF);
    if held.hidden {
        held.timer = None;
        let wake = Arc::clone(&held.wake);
        drop(held);
        return wake();
    }
    let weak = Rc::downgrade(state);
    let fire = Closure::<dyn FnMut()>::new(move || {
        if let Some(state) = weak.upgrade() {
            connect(&state);
        }
    });
    let ms = i32::try_from(retry.as_millis()).unwrap_or(i32::MAX);
    held.timer = web_sys::window()
        .and_then(|window| window.set_timeout_with_callback_and_timeout_and_arguments_0(fire.as_ref().unchecked_ref(), ms).ok())
        .map(|id| Timer { id, _fire: fire });
    let wake = Arc::clone(&held.wake);
    drop(held);
    wake();
}

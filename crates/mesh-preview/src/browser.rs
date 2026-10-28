//! One cancellable trusted module worker, lazy-loaded from the application base.
use crate::{ADOPT_BYTES, Adoption, Error, LOADER, Prepared, wire};
use js_sys::{Array, ArrayBuffer};
use std::{cell::RefCell, rc::Rc, sync::Arc};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{Event, MessageEvent, Url, Worker, WorkerOptions, WorkerType};
const START_MS: f64 = 15_000.0;
const DECODE_MS: f64 = 30_000.0;

#[derive(Default)]
pub struct Browser {
    active: Option<Active>,
    sequence: u32,
}
enum Signal {
    Message(JsValue),
    Failed,
}
struct Guard {
    worker: Worker,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _error: Closure<dyn FnMut(Event)>,
    _message_error: Closure<dyn FnMut(Event)>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.worker.set_onmessage(None);
        self.worker.set_onerror(None);
        self.worker.set_onmessageerror(None);
        self.worker.terminate();
    }
}
enum Phase {
    Starting(Arc<[u8]>),
    Sending { bytes: Arc<[u8]>, buffer: ArrayBuffer, at: usize },
    Decoding,
    Adopting { buffers: [ArrayBuffer; 3] },
}
struct Active {
    guard: Guard,
    inbox: Rc<RefCell<Option<Signal>>>,
    job: wire::Job,
    phase: Phase,
    // One fixed-size validator/hash state; no additional heap box for the large Hasher.
    adoption: Option<Adoption>,
    deadline: f64,
    waker: Rc<dyn Fn()>,
}
/// Fixed, bounded performance marks for browser conformance and profiling; no asset identity.
pub fn mark(name: &str) {
    if let Some(performance) = web_sys::window().and_then(|w| w.performance()) {
        performance.clear_marks_with_mark_name(name);
        let _ = performance.mark(name);
    }
}
fn now() -> Result<f64, Error> {
    web_sys::window().and_then(|w| w.performance()).map(|p| p.now()).filter(|t| t.is_finite()).ok_or(Error::Unavailable)
}
/// Resolves a fixed compiler-generated loader, with a build cache key. Relative deployment is
/// based on Document.baseURI, never a user-supplied URL or the origin root.
pub fn loader_url() -> Result<String, Error> {
    let window = web_sys::window().ok_or(Error::Unavailable)?;
    let document = window.document().ok_or(Error::Unavailable)?;
    let raw_base = js_sys::Reflect::get(&document, &"baseURI".into()).map_err(|_| Error::Unavailable)?;
    let base = wire::text(&raw_base, 4096).map_err(|_| Error::Unavailable)?;
    let url = Url::new_with_base(LOADER, &base).map_err(|_| Error::Unavailable)?;
    if url.origin() != window.location().origin().map_err(|_| Error::Unavailable)? || !matches!(url.protocol().as_str(), "http:" | "https:") {
        return Err(Error::Unavailable);
    }
    url.set_search(&format!("?build={}", crate::BUILD));
    Ok(url.href())
}
impl Browser {
    pub fn busy(&self) -> bool {
        self.active.is_some()
    }
    /// Cancellation actually terminates synchronous Wasm work and drops transfers/listeners.
    pub fn cancel(&mut self) {
        self.active = None;
    }
    pub fn start(&mut self, hash: [u8; 32], bytes: Arc<[u8]>, wake: impl Fn() + 'static) -> Result<(), Error> {
        if self.busy() {
            return Err(Error::Unavailable);
        }
        if bytes.is_empty() || bytes.len() > crate::MAX_INPUT {
            return Err(Error::TooLarge);
        }
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Unavailable)?;
        let job = wire::Job { generation: self.sequence, sequence: self.sequence, hash, length: u32::try_from(bytes.len()).map_err(|_| Error::TooLarge)? };
        let options = WorkerOptions::new();
        options.set_type(WorkerType::Module);
        options.set_name("NodeCraft 3D preview");
        let worker = Worker::new_with_options(&loader_url()?, &options).map_err(|_| Error::Unavailable)?;
        let inbox = Rc::new(RefCell::new(None));
        let waker: Rc<dyn Fn()> = Rc::new(wake);
        let (slot, repaint) = (inbox.clone(), waker.clone());
        let message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let mut slot = slot.borrow_mut();
            *slot = Some(if slot.is_none() { Signal::Message(event.data()) } else { Signal::Failed });
            repaint();
        });
        let error = error_listener(inbox.clone(), waker.clone());
        let message_error = error_listener(inbox.clone(), waker.clone());
        worker.set_onmessage(Some(message.as_ref().unchecked_ref()));
        worker.set_onerror(Some(error.as_ref().unchecked_ref()));
        worker.set_onmessageerror(Some(message_error.as_ref().unchecked_ref()));
        let guard = Guard { worker, _message: message, _error: error, _message_error: message_error };
        mark("nodecraft.mesh.started");
        self.active = Some(Active { guard, inbox, job, phase: Phase::Starting(bytes), adoption: None, deadline: now()? + START_MS, waker });
        Ok(())
    }
    /// At most one 256KiB copy/validation chunk per call; call once per frame. The final GPU
    /// upload is separate and still costs time proportional to the admitted prepared buffers.
    pub fn poll(&mut self) -> Option<Result<Prepared, Error>> {
        let active = self.active.as_mut()?;
        let result = active.step();
        if matches!(&result, Some(Ok(_))) {
            mark("nodecraft.mesh.ready");
        }
        if result.is_some() {
            self.active = None;
        }
        result
    }
}
impl Active {
    fn step(&mut self) -> Option<Result<Prepared, Error>> {
        match now() {
            Ok(t) if t <= self.deadline => {}
            _ => return Some(Err(Error::Timeout)),
        }
        let signal = self.inbox.borrow_mut().take();
        if let Some(signal) = signal {
            let Signal::Message(data) = signal else {
                return Some(Err(Error::Unavailable));
            };
            if let Err(error) = self.receive(data) {
                return Some(Err(error));
            }
        }
        match &mut self.phase {
            Phase::Sending { bytes, buffer, at } => {
                let end = at.saturating_add(ADOPT_BYTES).min(bytes.len());
                let chunk = match bytes.get(*at..end) {
                    Some(c) => c,
                    None => return Some(Err(Error::Protocol)),
                };
                let (Ok(start), Ok(stop)) = (u32::try_from(*at), u32::try_from(end)) else {
                    return Some(Err(Error::TooLarge));
                };
                match wire::view(buffer) {
                    Ok(view) => {
                        let slice = view.subarray(start, stop);
                        if usize::try_from(slice.length()).ok() != Some(chunk.len()) {
                            return Some(Err(Error::Protocol));
                        }
                        slice.copy_from(chunk);
                    }
                    Err(e) => return Some(Err(e)),
                };
                *at = end;
                if end == bytes.len() {
                    let message = wire::header("decode");
                    wire::append_job(&message, self.job);
                    message.push(buffer);
                    let transfers = Array::new();
                    transfers.push(buffer);
                    if self.guard.worker.post_message_with_transfer(&message, &transfers).is_err() {
                        return Some(Err(Error::Unavailable));
                    }
                    mark("nodecraft.mesh.decoding");
                    self.phase = Phase::Decoding;
                    self.deadline = match now() {
                        Ok(n) => n + DECODE_MS,
                        Err(e) => return Some(Err(e)),
                    };
                }
                (self.waker)();
                None
            }
            Phase::Adopting { buffers } => {
                let Some(decoder) = self.adoption.as_mut() else {
                    return Some(Err(Error::Protocol));
                };
                if let Some((index, at, total)) = decoder.progress() {
                    let record = if index == 0 { crate::VERTEX_BYTES } else { 4 };
                    let count = (total - at).min(ADOPT_BYTES / record * record);
                    let (Ok(start), Ok(end)) = (u32::try_from(at), u32::try_from(at + count)) else {
                        return Some(Err(Error::TooLarge));
                    };
                    let Some(buffer) = buffers.get(index) else {
                        return Some(Err(Error::Protocol));
                    };
                    let mut chunk = match crate::reserve(count) {
                        Ok(c) => c,
                        Err(e) => return Some(Err(e)),
                    };
                    chunk.resize(count, 0);
                    match wire::view(buffer) {
                        Ok(view) => {
                            let slice = view.subarray(start, end);
                            if usize::try_from(slice.length()).ok() != Some(chunk.len()) {
                                return Some(Err(Error::Protocol));
                            }
                            slice.copy_to(&mut chunk);
                        }
                        Err(e) => return Some(Err(e)),
                    };
                    if let Err(e) = decoder.push(index, &chunk) {
                        return Some(Err(e));
                    }
                    (self.waker)();
                    None
                } else {
                    let Some(decoder) = self.adoption.take() else {
                        return Some(Err(Error::Protocol));
                    };
                    Some(decoder.finish())
                }
            }
            Phase::Starting(_) | Phase::Decoding => None,
        }
    }
    fn receive(&mut self, data: JsValue) -> Result<(), Error> {
        match &self.phase {
            Phase::Starting(bytes) => {
                let message = wire::array(&data, 3)?;
                wire::check_header(&message, "ready")?;
                self.phase = Phase::Sending { bytes: bytes.clone(), buffer: wire::allocate(self.job.length)?, at: 0 };
                Ok(())
            }
            Phase::Decoding => {
                let length = wire::arity(&data, 14)?;
                let message: Array = data.clone().unchecked_into();
                if length == 8 {
                    wire::check_header(&message, "error")?;
                    if wire::job(&message)? != self.job {
                        return Err(Error::Cancelled);
                    }
                    return Err(match wire::number(&message.get(7), 3)? {
                        1 => Error::TooLarge,
                        2 => Error::Capacity,
                        3 => Error::Coordinates,
                        _ => Error::Invalid,
                    });
                }
                let message = wire::array(&data, 14)?;
                wire::check_header(&message, "done")?;
                // Stale identity is rejected before examining/copying any geometry buffer.
                if wire::job(&message)? != self.job {
                    return Err(Error::Cancelled);
                }
                let layout = wire::layout(&message)?;
                let sizes = layout.sizes(crate::Profile::Browser)?;
                let bounds = wire::bounds(&message.get(10))?;
                let [a, b, c] = sizes;
                let buffers = [wire::buffer(&message.get(11), a)?, wire::buffer(&message.get(12), b)?, wire::buffer(&message.get(13), c)?];
                let extents = buffers.each_ref().map(|b| b.byte_length() as usize);
                // Every raw count/product/extent is validated before Adoption reserves vectors.
                let decoder = Adoption::new(layout, bounds, extents)?;
                mark("nodecraft.mesh.adopting");
                self.adoption = Some(decoder);
                self.phase = Phase::Adopting { buffers };
                self.deadline = now()? + DECODE_MS;
                Ok(())
            }
            Phase::Sending { .. } | Phase::Adopting { .. } => Err(Error::Protocol),
        }
    }
}
fn error_listener(slot: Rc<RefCell<Option<Signal>>>, repaint: Rc<dyn Fn()>) -> Closure<dyn FnMut(Event)> {
    Closure::new(move |event: Event| {
        event.prevent_default();
        *slot.borrow_mut() = Some(Signal::Failed);
        repaint();
    })
}

/// Test-only probes compile into the separate conformance binary, never release packages.
#[cfg(feature = "conformance")]
#[derive(Clone, Copy)]
pub enum Fault {
    Timeout,
    WrongBuild,
    StaleReply,
    OversizedReply,
    MessageError,
}
#[cfg(feature = "conformance")]
impl Browser {
    pub fn phase_for_conformance(&self) -> &'static str {
        match self.active.as_ref().map(|a| &a.phase) {
            None => "idle",
            Some(Phase::Starting(_)) => "starting",
            Some(Phase::Sending { .. }) => "sending",
            Some(Phase::Decoding) => "decoding",
            Some(Phase::Adopting { .. }) => "adopting",
        }
    }
    pub fn fault_for_conformance(&mut self, fault: Fault) -> Result<(), Error> {
        let active = self.active.as_mut().ok_or(Error::Unavailable)?;
        match fault {
            Fault::Timeout => active.deadline = -1.0,
            Fault::MessageError => {
                let event = Event::new("messageerror").map_err(|_| Error::Unavailable)?;
                active.guard.worker.dispatch_event(&event).map_err(|_| Error::Unavailable)?;
            }
            Fault::WrongBuild => {
                let data = wire::header("ready");
                data.set(2, "other-build".into());
                *active.inbox.borrow_mut() = Some(Signal::Message(data.into()));
            }
            Fault::StaleReply | Fault::OversizedReply => {
                if !matches!(active.phase, Phase::Decoding) {
                    return Err(Error::Protocol);
                }
                let mut job = active.job;
                if matches!(fault, Fault::StaleReply) {
                    job.sequence = job.sequence.saturating_add(1);
                }
                let data = wire::header("done");
                wire::append_job(&data, job);
                data.push(&600_001u32.into());
                data.push(&1u32.into());
                data.push(&0u32.into());
                for _ in 0..4 {
                    data.push(&JsValue::UNDEFINED);
                }
                *active.inbox.borrow_mut() = Some(Signal::Message(data.into()));
            }
        }
        Ok(())
    }
}
#[cfg(feature = "conformance")]
pub fn protocol_for_conformance() -> Result<u32, Error> {
    let mut checks = 0;
    for value in [f64::NAN, f64::INFINITY, -1.0, 0.5, 4_294_967_296.0] {
        if wire::number(&value.into(), u32::MAX).is_ok() {
            return Err(Error::Protocol);
        }
        checks += 1;
    }
    if wire::text(&"oversized".into(), 3).is_ok() || wire::text(&17.into(), 3).is_ok() || wire::text(&"ok".into(), 2)? != "ok" {
        return Err(Error::Protocol);
    }
    checks += 1;
    let sparse = Array::new_with_length(1_000_000);
    if wire::array(&sparse.into(), 14).is_ok() {
        return Err(Error::Protocol);
    }
    checks += 1;
    let small = wire::allocate(4)?;
    if wire::buffer(&small, 3).is_ok() {
        return Err(Error::Protocol);
    }
    checks += 1;
    let fake = js_sys::Object::new();
    js_sys::Reflect::set(&fake, &"byteLength".into(), &4.0.into()).map_err(|_| Error::Protocol)?;
    if wire::buffer(&fake, 4).is_ok() {
        return Err(Error::Protocol);
    }
    checks += 1;
    let forged = wire::allocate(8)?;
    let descriptor = js_sys::Object::new();
    js_sys::Reflect::set(&descriptor, &"value".into(), &4.0.into()).map_err(|_| Error::Protocol)?;
    js_sys::Reflect::define_property(forged.unchecked_ref::<js_sys::Object>(), &"byteLength".into(), &descriptor).map_err(|_| Error::Protocol)?;
    if wire::buffer(&forged, 16).is_ok() {
        return Err(Error::Protocol);
    }
    checks += 1;
    Ok(checks)
}

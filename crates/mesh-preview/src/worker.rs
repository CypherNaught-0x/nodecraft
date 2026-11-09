//! Entry point for the generated Rust/Wasm worker; no paths, stores or provider authority.
use crate::{Error, Profile, prepare_checked, wire};
use js_sys::Array;
use std::cell::RefCell;
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};
type MessageHandler = Closure<dyn FnMut(MessageEvent)>;
thread_local! { static LISTENER: RefCell<Option<MessageHandler>> = const {RefCell::new(None)}; }

pub fn start(admit: fn([f32; 3], f32) -> bool) -> Result<(), Error> {
    let scope = js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>().map_err(|_| Error::Unavailable)?;
    let sender = scope.clone();
    let listener = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let result = handle(&event.data(), admit);
        let Ok((job, response)) = result else { return }; // malformed messages have no admitted identity
        let (message, transfers) = match response {
            Ok(model) => {
                let out = wire::header("done");
                wire::append_job(&out, job);
                let counts = model.layout();
                out.push(&counts.vertices.into());
                out.push(&counts.triangles.into());
                out.push(&counts.edge_words.into());
                let bounds = model.bounds();
                let list = Array::new();
                for n in bounds.min.into_iter().chain(bounds.max) {
                    list.push(&n.into());
                }
                out.push(&list);
                let transfers = Array::new();
                let mut failed = false;
                for bytes in model.into_buffers() {
                    match wire::owned(&bytes) {
                        Ok(buffer) => {
                            out.push(&buffer);
                            transfers.push(&buffer);
                        }
                        Err(_) => failed = true,
                    }
                }
                if failed { (error(job, Error::Capacity), Array::new()) } else { (out, transfers) }
            }
            Err(why) => (error(job, why), Array::new()),
        };
        let _ = sender.post_message_with_transfer(&message, &transfers);
    });
    scope.set_onmessage(Some(listener.as_ref().unchecked_ref()));
    LISTENER.with(|slot| *slot.borrow_mut() = Some(listener));
    scope.post_message(&wire::header("ready")).map_err(|_| Error::Unavailable)
}
pub(crate) fn error(job: wire::Job, why: Error) -> Array {
    let out = wire::header("error");
    wire::append_job(&out, job);
    let code = match why {
        Error::TooLarge => 1,
        Error::Capacity => 2,
        Error::Coordinates => 3,
        Error::Protocol => 4,
        _ => 0,
    };
    out.push(&code.into());
    out
}
fn handle(value: &wasm_bindgen::JsValue, admit: fn([f32; 3], f32) -> bool) -> Result<(wire::Job, Result<crate::Prepared, Error>), Error> {
    let data = wire::array(value, 8)?;
    wire::check_header(&data, "decode")?;
    let job = wire::job(&data)?;
    let buffer = wire::buffer(&data.get(7), crate::MAX_INPUT)?;
    // Exact, admitted size before materializing an owned Vec.
    if buffer.byte_length() != job.length || job.length == 0 {
        return Ok((job, Err(Error::Protocol)));
    }
    let bytes = wire::copy(&buffer, crate::MAX_INPUT)?;
    if blake3::hash(&bytes).as_bytes() != &job.hash {
        return Ok((job, Err(Error::Invalid)));
    }
    let result = prepare_checked(&bytes, Profile::Browser, admit);
    // Keep the original bytes out of the reply and allow their memory to be reclaimed first.
    drop(bytes);
    Ok((job, result))
}

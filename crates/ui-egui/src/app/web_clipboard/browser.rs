use super::*;
use eframe::wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;

pub(super) struct Listener {
    document: web_sys::Document,
    paste: Closure<dyn FnMut(web_sys::ClipboardEvent)>,
}
impl Listener {
    pub(super) fn new(inbox: Rc<Cell<bool>>, ctx: egui::Context) -> Result<Self, String> {
        let document = web_sys::window().and_then(|window| window.document()).ok_or_else(|| DENIED.to_owned())?;
        let paste = Closure::new(move |event: web_sys::ClipboardEvent| {
            if event.is_trusted()
                && event
                    .clipboard_data()
                    .is_some_and(|data| data.types().iter().take(32).any(|kind| kind == JsValue::from_str("Files") || kind == JsValue::from_str("image/png")))
            {
                inbox.set(true);
                ctx.request_repaint();
            }
        });
        document.add_event_listener_with_callback_and_bool("paste", paste.as_ref().unchecked_ref(), true).map_err(|_| DENIED.to_owned())?;
        Ok(Self { document, paste })
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.document.remove_event_listener_with_callback_and_bool("paste", self.paste.as_ref().unchecked_ref(), true);
    }
}
struct Permit(Rc<Cell<bool>>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

fn property(object: &JsValue, key: &str) -> Result<JsValue, String> {
    js_sys::Reflect::get(object, &JsValue::from_str(key)).map_err(|_| DENIED.to_owned())
}
fn promise_call(object: &JsValue, key: &str, argument: Option<&JsValue>) -> Result<js_sys::Promise, String> {
    let function = property(object, key)?.dyn_into::<js_sys::Function>().map_err(|_| DENIED.to_owned())?;
    let result = match argument {
        Some(argument) => function.call1(object, argument),
        None => function.call0(object),
    }
    .map_err(|_| DENIED.to_owned())?;
    result.dyn_into::<js_sys::Promise>().map_err(|_| DENIED.to_owned())
}
pub(super) fn read(state: Weak<RefCell<ReadState>>, busy: Rc<Cell<bool>>, ctx: egui::Context) -> Result<(), String> {
    let window = web_sys::window().ok_or_else(|| DENIED.to_owned())?;
    let clipboard = property(window.navigator().as_ref(), "clipboard")?;
    // This call is made only by a user paste admitted in the app frame, before awaiting anything.
    let promise = promise_call(&clipboard, "read", None)?;
    let permit = Permit(busy);
    wasm_bindgen_futures::spawn_local(async move {
        let _permit = permit;
        let result = async {
            let value = JsFuture::from(promise).await.map_err(|_| DENIED.to_owned())?;
            if state.upgrade().is_none() {
                return Ok(());
            }
            let items = value.dyn_into::<js_sys::Array>().map_err(|_| DENIED.to_owned())?;
            let mut chosen = None;
            for item in items.iter().take(32) {
                let types = property(&item, "types")?.dyn_into::<js_sys::Array>().map_err(|_| DENIED.to_owned())?;
                if types.iter().take(32).any(|kind| kind == JsValue::from_str("image/png")) {
                    chosen = Some(item);
                    break;
                }
            }
            let item = chosen.ok_or_else(|| NO_PNG.to_owned())?;
            let png = JsValue::from_str("image/png");
            let value = JsFuture::from(promise_call(&item, "getType", Some(&png))?).await.map_err(|_| DENIED.to_owned())?;
            let Some(owner) = state.upgrade() else { return Ok(()) };
            let blob = value.dyn_into::<web_sys::Blob>().map_err(|_| NO_PNG.to_owned())?;
            if blob.type_() != "image/png" || !blob.size().is_finite() || blob.size() < 0.0 || blob.size() > nodecraft_engine::import::MAX_MEDIA_BYTES as f64 {
                return Err("The copied PNG is over 16 MiB or couldn't be read; nothing was pasted.".into());
            }
            let mut owner = owner.try_borrow_mut().map_err(|_| DENIED.to_owned())?;
            let reader = super::super::drop::web::reader::Reader::start(&blob, Rc::downgrade(&owner.reads), 0, ctx.clone())?;
            owner._reader = Some(reader);
            Ok(())
        }
        .await;
        if let Err(error) = result {
            ReadState::complete(&state, Err(error));
        }
        ctx.request_repaint();
    });
    Ok(())
}

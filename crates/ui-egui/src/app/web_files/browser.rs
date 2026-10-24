//! The browser's file APIs for projects (#118): the File System Access pickers where the browser
//! has them (Chromium-based browsers), and otherwise an `<input type="file">` to open and a
//! download to save. The pickers are called through `js_sys` reflection, because `web-sys` only
//! offers them behind an unstable flag.

use super::{Named, PickFuture, Picked, Port};
use eframe::wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use std::collections::BTreeMap;
use wasm_bindgen_futures::JsFuture;

/// How long a download's object URL stays valid after the click, in milliseconds.
const DOWNLOAD_URL_MS: i32 = 60_000;

/// The browser port: file handles stay here, under the ids the flow refers to them by.
#[derive(Default)]
pub(crate) struct Browser {
    handles: std::rc::Rc<std::cell::RefCell<BTreeMap<u64, JsValue>>>,
    next: std::rc::Rc<std::cell::Cell<u64>>,
}

fn window() -> Result<web_sys::Window, String> {
    web_sys::window().ok_or_else(|| "The page has no window.".to_owned())
}

/// `target.method(...args)`.
fn call(target: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let function: js_sys::Function = js_sys::Reflect::get(target, &JsValue::from_str(method))?.dyn_into()?;
    let list = js_sys::Array::new();
    for arg in args {
        list.push(arg);
    }
    function.apply(target, &list)
}

/// `await target.method(...args)`.
async fn call_async(target: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let promise: js_sys::Promise = call(target, method, args)?.dyn_into()?;
    JsFuture::from(promise).await
}

/// The DOMException name of a failure (`AbortError` when the person cancelled a picker).
fn error_name(error: &JsValue) -> String {
    js_sys::Reflect::get(error, &JsValue::from_str("name")).ok().and_then(|n| n.as_string()).unwrap_or_default()
}

fn has_pickers() -> bool {
    let Ok(window) = window() else { return false };
    ["showOpenFilePicker", "showSaveFilePicker"].iter().all(|name| js_sys::Reflect::get(&window, &JsValue::from_str(name)).is_ok_and(|f| f.is_function()))
}

/// `{types: [{description, accept: {"application/octet-stream": [".ncraft"]}}], suggestedName}`.
fn picker_options(suggested: Option<&str>) -> Result<JsValue, JsValue> {
    let accept = js_sys::Object::new();
    js_sys::Reflect::set(&accept, &JsValue::from_str("application/octet-stream"), &js_sys::Array::of1(&JsValue::from_str(".ncraft")))?;
    let kind = js_sys::Object::new();
    js_sys::Reflect::set(&kind, &JsValue::from_str("description"), &JsValue::from_str("NodeCraft project"))?;
    js_sys::Reflect::set(&kind, &JsValue::from_str("accept"), &accept)?;
    let options = js_sys::Object::new();
    js_sys::Reflect::set(&options, &JsValue::from_str("types"), &js_sys::Array::of1(&kind))?;
    if let Some(name) = suggested {
        js_sys::Reflect::set(&options, &JsValue::from_str("suggestedName"), &JsValue::from_str(name))?;
    }
    Ok(options.into())
}

/// Reads `file` (a `what`: "project") whole, refusing one over `max` bytes before reading it.
async fn read(file: web_sys::File, max: u64, what: &str) -> Result<(String, Vec<u8>), String> {
    let size = file.size();
    // A size the browser reports wrongly still can't get past the check after reading.
    if !size.is_finite() || size < 0.0 || size > max as f64 {
        return Err(too_large(max, what));
    }
    let buffer = JsFuture::from(file.array_buffer()).await.map_err(|_| format!("The browser couldn't read the {what} file."))?;
    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
    if bytes.len() as u64 > max {
        return Err(too_large(max, what));
    }
    Ok((file.name(), bytes))
}

fn too_large(max: u64, what: &str) -> String {
    format!("The {what} is larger than the browser build opens ({} MiB).", max / (1024 * 1024))
}

/// Waits for the person to choose a file in a hidden `<input type="file">`. Browsers that never
/// report a cancelled choice leave this pending; the next open replaces it.
async fn input_file(accept: &str) -> Result<Option<web_sys::File>, String> {
    let failed = |_| "The browser couldn't open the file picker.".to_owned();
    let document = window()?.document().ok_or_else(|| "The page has no document.".to_owned())?;
    let input: web_sys::HtmlInputElement =
        document.create_element("input").map_err(failed)?.dyn_into().map_err(|_| "The page refused a file field.".to_owned())?;
    input.set_type("file");
    input.set_accept(accept);
    let target = input.clone();
    let chosen = js_sys::Promise::new(&mut |resolve, _reject| {
        let done = resolve.clone();
        target.set_onchange(Some(
            Closure::once_into_js(move || {
                let _ = done.call0(&JsValue::NULL);
            })
            .unchecked_ref(),
        ));
        let _ = target.add_event_listener_with_callback(
            "cancel",
            Closure::once_into_js(move || {
                let _ = resolve.call0(&JsValue::NULL);
            })
            .unchecked_ref(),
        );
    });
    input.click();
    JsFuture::from(chosen).await.map_err(failed)?;
    Ok(input.files().and_then(|files| files.get(0)))
}

/// Asks for a plugin package file and reads it (#823); `None` when the person cancels.
pub(crate) async fn pick_package(max: u64) -> Result<Option<(String, Vec<u8>)>, String> {
    let Some(file) = input_file(".ncraft-plugin").await? else { return Ok(None) };
    read(file, max, "plugin package").await.map(Some)
}

impl Browser {
    fn keep(&self, handle: JsValue) -> u64 {
        let id = self.next.get().wrapping_add(1);
        self.next.set(id);
        self.handles.borrow_mut().insert(id, handle);
        id
    }
}

impl Port for Browser {
    fn open(&mut self, max: u64) -> PickFuture<Option<Picked>> {
        let port = Self { handles: std::rc::Rc::clone(&self.handles), next: std::rc::Rc::clone(&self.next) };
        Box::pin(async move {
            if !has_pickers() {
                let Some(file) = input_file(".ncraft").await? else { return Ok(None) };
                let (name, bytes) = read(file, max, "project").await?;
                return Ok(Some(Picked { name, bytes, target: None }));
            }
            let window: JsValue = window()?.into();
            let options = picker_options(None).map_err(|_| "The browser couldn't open the file picker.".to_owned())?;
            let handles = match call_async(&window, "showOpenFilePicker", &[options]).await {
                Ok(handles) => handles,
                Err(error) if error_name(&error) == "AbortError" => return Ok(None),
                // Refused here (a framed page, say): a file field still works.
                Err(_) => {
                    let Some(file) = input_file(".ncraft").await? else { return Ok(None) };
                    let (name, bytes) = read(file, max, "project").await?;
                    return Ok(Some(Picked { name, bytes, target: None }));
                }
            };
            let handle = js_sys::Array::from(&handles).get(0);
            if handle.is_undefined() {
                return Ok(None);
            }
            let file: web_sys::File = call_async(&handle, "getFile", &[])
                .await
                .ok()
                .and_then(|file| file.dyn_into().ok())
                .ok_or_else(|| "The browser couldn't read the chosen file.".to_owned())?;
            let (name, bytes) = read(file, max, "project").await?;
            Ok(Some(Picked { name, bytes, target: Some(port.keep(handle)) }))
        })
    }

    fn save_as(&mut self, suggested: &str) -> PickFuture<Option<Named>> {
        let port = Self { handles: std::rc::Rc::clone(&self.handles), next: std::rc::Rc::clone(&self.next) };
        let suggested = suggested.to_owned();
        Box::pin(async move {
            if !has_pickers() {
                // Saved as a download; the browser decides where it goes.
                return Ok(Some(Named { name: suggested, target: None }));
            }
            let window: JsValue = window()?.into();
            let options = picker_options(Some(&suggested)).map_err(|_| "The browser couldn't open the save picker.".to_owned())?;
            let handle = match call_async(&window, "showSaveFilePicker", &[options]).await {
                Ok(handle) => handle,
                Err(error) if error_name(&error) == "AbortError" => return Ok(None),
                // Refused here (a framed page, say): save as a download instead.
                Err(_) => return Ok(Some(Named { name: suggested, target: None })),
            };
            let name = js_sys::Reflect::get(&handle, &JsValue::from_str("name")).ok().and_then(|n| n.as_string()).unwrap_or(suggested);
            Ok(Some(Named { name, target: Some(port.keep(handle)) }))
        })
    }

    fn write(&mut self, target: Option<u64>, name: &str, bytes: Vec<u8>) -> PickFuture<()> {
        let Some(handle) = target.and_then(|id| self.handles.borrow().get(&id).cloned()) else {
            let result = download(name, &bytes);
            return Box::pin(std::future::ready(result));
        };
        Box::pin(async move {
            let refused = |error: JsValue| match error_name(&error).as_str() {
                "NotAllowedError" => "The browser didn't allow writing the file; save it again and allow access.".to_owned(),
                "QuotaExceededError" => "There isn't enough space to write the file.".to_owned(),
                _ => "The browser couldn't write the file.".to_owned(),
            };
            let stream = call_async(&handle, "createWritable", &[]).await.map_err(refused)?;
            let data: JsValue = js_sys::Uint8Array::from(bytes.as_slice()).into();
            if let Err(error) = call_async(&stream, "write", &[data]).await {
                // Discard the partial write; the file keeps its earlier contents.
                let _ = call_async(&stream, "abort", &[]).await;
                return Err(refused(error));
            }
            call_async(&stream, "close", &[]).await.map(|_| ()).map_err(refused)
        })
    }
}

/// Saves `bytes` as a download named `name`.
fn download(name: &str, bytes: &[u8]) -> Result<(), String> {
    let failed = |_| "The browser couldn't start the download.".to_owned();
    let window = window()?;
    let document = window.document().ok_or_else(|| "The page has no document.".to_owned())?;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let bag = web_sys::BlobPropertyBag::new();
    bag.set_type("application/octet-stream");
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &bag).map_err(failed)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(failed)?;
    let link: web_sys::HtmlAnchorElement = document.create_element("a").map_err(failed)?.dyn_into().map_err(|_| "The page refused a link.".to_owned())?;
    link.set_href(&url);
    link.set_download(name);
    link.click();
    // The download has started from the URL by now; free the copy a while later.
    let revoke = Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), DOWNLOAD_URL_MS);
    Ok(())
}

/// Asks the browser to confirm leaving the page while `unsaved` says there is work to lose.
pub(crate) fn guard_unload(unsaved: std::rc::Rc<std::cell::Cell<bool>>) {
    let Ok(window) = window() else { return };
    let guard = Closure::<dyn Fn(web_sys::BeforeUnloadEvent)>::new(move |event: web_sys::BeforeUnloadEvent| {
        if unsaved.get() {
            event.prevent_default();
            // Older browsers need a return value as well.
            event.set_return_value("unsaved");
        }
    });
    if window.add_event_listener_with_callback("beforeunload", guard.as_ref().unchecked_ref()).is_ok() {
        // Lives as long as the page.
        guard.forget();
    }
}

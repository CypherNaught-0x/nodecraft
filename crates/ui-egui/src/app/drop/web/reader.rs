//! A browser read is owned by its pending batch; dropping it aborts the browser operation.
use super::{MAX_MEDIA_BYTES, MAX_PROJECT_MEDIA_BYTES, Slot};
use eframe::wasm_bindgen::{JsCast, closure::Closure};
use std::{cell::RefCell, rc::Weak};

pub(in crate::app) struct Reader {
    reader: web_sys::FileReader,
    _done: Closure<dyn FnMut(web_sys::Event)>,
}
impl Reader {
    pub(in crate::app) fn start(blob: &web_sys::Blob, reads: Weak<RefCell<Vec<(String, Slot)>>>, slot: usize, ctx: egui::Context) -> Result<Self, String> {
        let reader = web_sys::FileReader::new().map_err(|_| "the browser can't read this file".to_owned())?;
        let done = Closure::new(move |event: web_sys::Event| {
            // Cancellation/document replacement must not allocate or publish late bytes.
            let Some(reads) = reads.upgrade() else { return };
            let Ok(mut reads) = reads.try_borrow_mut() else { return };
            let result = (|| {
                let reader = event.target().and_then(|target| target.dyn_into::<web_sys::FileReader>().ok()).ok_or("the file can't be read")?;
                if reader.error().is_some() {
                    return Err("the file can't be read");
                }
                let buffer = reader.result().ok().and_then(|value| value.dyn_into::<js_sys::ArrayBuffer>().ok()).ok_or("the file can't be read")?;
                let bytes = js_sys::Uint8Array::new(&buffer);
                let len = usize::try_from(bytes.length()).map_err(|_| "the file is too large")?;
                if len > MAX_MEDIA_BYTES {
                    return Err("the file is over 16 MiB");
                }
                let retained = reads
                    .iter()
                    .filter_map(|(_, result)| result.as_ref().and_then(|result| result.as_ref().ok()))
                    .try_fold(0usize, |total, bytes| total.checked_add(bytes.len()))
                    .ok_or("the file batch is too large")?;
                if retained.checked_add(len).is_none_or(|total| total > MAX_PROJECT_MEDIA_BYTES) {
                    return Err("the file batch is over 48 MiB");
                }
                let mut out = Vec::new();
                out.try_reserve_exact(len).map_err(|_| "there isn't enough memory to read this file")?;
                out.resize(len, 0);
                bytes.copy_to(&mut out);
                Ok(out)
            })()
            .map_err(str::to_owned);
            if let Some((_, entry)) = reads.get_mut(slot) {
                *entry = Some(result);
            }
            ctx.request_repaint();
        });
        reader.set_onloadend(Some(done.as_ref().unchecked_ref()));
        let owned = Self { reader, _done: done };
        owned.reader.read_as_array_buffer(blob).map_err(|_| "the browser can't read this file".to_owned())?;
        Ok(owned)
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.reader.set_onloadend(None);
        self.reader.abort();
    }
}

//! The browser's [`Shelf`]: one IndexedDB object store for the origin, keyed `<slot>/c` for a
//! slot's checkpoint, `<slot>/h` for its hold and `<slot>/f/<seq>` for its frames, and a Web
//! Lock per slot, held by its tab until the page goes. Web Locks are reached through `js_sys`
//! reflection, since `web-sys` only offers them behind an unstable flag.

use super::{Inventory, PickFuture, Shelf, Slot, Stored, Write};
use eframe::wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use nodecraft_format::journal::MAX_JOURNAL_BYTES;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use wasm_bindgen_futures::JsFuture;

const DATABASE: &str = "nodecraft-recovery";
const STORE: &str = "journal";
const LOCK_PREFIX: &str = "nodecraft.journal.";
/// Keys inspected in one complete inventory; larger stores refuse partial cleanup.
const MAX_KEYS: u32 = 1 << 20;

/// A name for this tab's slot: when it opened, then a random part, so later tabs sort after it.
pub(crate) fn fresh_slot() -> String {
    let now = js_sys::Date::now();
    let now = if now.is_finite() && now > 0.0 { now as u64 } else { 0 };
    let random = (js_sys::Math::random() * f64::from(u32::MAX)) as u32;
    format!("{now:013x}-{random:08x}")
}

/// A slot name this build makes: hex and one dash.
fn is_slot(name: &str) -> bool {
    name.len() == 22 && name.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

#[derive(Default)]
pub(crate) struct IndexedDb {
    database: Rc<RefCell<Option<web_sys::IdbDatabase>>>,
    held: Rc<RefCell<BTreeMap<String, js_sys::Function>>>,
}

/// A cancelled request must not leave a later-granted temporary lock held for the page's life.
struct PendingClaim {
    live: Rc<Cell<bool>>,
    held: Rc<RefCell<BTreeMap<String, js_sys::Function>>>,
    slot: String,
    keep: bool,
}

impl Drop for PendingClaim {
    fn drop(&mut self) {
        self.live.set(false);
        if !self.keep
            && let Some(release) = self.held.borrow_mut().remove(&self.slot)
        {
            let _ = release.call0(&JsValue::NULL);
        }
    }
}

impl Drop for IndexedDb {
    fn drop(&mut self) {
        for (_, release) in std::mem::take(&mut *self.held.borrow_mut()) {
            let _ = release.call0(&JsValue::NULL);
        }
    }
}

fn unavailable() -> String {
    "the site's storage isn't available; allow site data, then reload".into()
}

/// The DOMException name of a failure.
fn error_name(error: &JsValue) -> String {
    js_sys::Reflect::get(error, &JsValue::from_str("name")).ok().and_then(|n| n.as_string()).unwrap_or_default()
}

fn refused(name: &str) -> String {
    match name {
        "QuotaExceededError" => "this site's browser storage is full".into(),
        _ => "the browser couldn't write to this site's storage".into(),
    }
}

type Handler = Closure<dyn FnMut()>;

/// A handler settling a promise with `settle`; dropped once the promise is awaited, so none
/// outlives its request.
fn handler(settle: js_sys::Function) -> Handler {
    Closure::new(move || {
        let _ = settle.call0(&JsValue::NULL);
    })
}

/// Waits for `request` to succeed, with its result.
async fn done(request: &web_sys::IdbRequest) -> Result<JsValue, String> {
    let mut handlers: Option<(Handler, Handler)> = None;
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let (ok, failed) = (handler(resolve), handler(reject));
        request.set_onsuccess(Some(ok.as_ref().unchecked_ref()));
        request.set_onerror(Some(failed.as_ref().unchecked_ref()));
        handlers = Some((ok, failed));
    });
    let settled = JsFuture::from(promise).await;
    request.set_onsuccess(None);
    request.set_onerror(None);
    drop(handlers);
    settled.map_err(|_| "the browser couldn't read this site's storage".to_owned())?;
    request.result().map_err(|_| "the browser couldn't read this site's storage".to_owned())
}

/// Waits for `transaction` to commit; the failure's DOMException name if it doesn't.
async fn committed(transaction: &web_sys::IdbTransaction) -> Result<(), String> {
    let mut handlers: Option<(Handler, Handler, Handler)> = None;
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let (complete, error, abort) = (handler(resolve), handler(reject.clone()), handler(reject));
        transaction.set_oncomplete(Some(complete.as_ref().unchecked_ref()));
        transaction.set_onerror(Some(error.as_ref().unchecked_ref()));
        transaction.set_onabort(Some(abort.as_ref().unchecked_ref()));
        handlers = Some((complete, error, abort));
    });
    let settled = JsFuture::from(promise).await;
    transaction.set_oncomplete(None);
    transaction.set_onerror(None);
    transaction.set_onabort(None);
    drop(handlers);
    match settled {
        Ok(_) => Ok(()),
        Err(_) => Err(transaction.error().map(|e| e.name()).unwrap_or_default()),
    }
}

async fn open(cached: Rc<RefCell<Option<web_sys::IdbDatabase>>>) -> Result<web_sys::IdbDatabase, String> {
    if let Some(database) = cached.borrow().as_ref() {
        return Ok(database.clone());
    }
    let factory = web_sys::window().and_then(|w| w.indexed_db().ok().flatten()).ok_or_else(unavailable)?;
    let request = factory.open_with_u32(DATABASE, 1).map_err(|_| unavailable())?;
    let upgrading = request.clone();
    let upgrade = Closure::<dyn FnMut()>::new(move || {
        if let Ok(result) = upgrading.result()
            && let Ok(database) = result.dyn_into::<web_sys::IdbDatabase>()
            && !database.object_store_names().contains(STORE)
        {
            let _ = database.create_object_store(STORE);
        }
    });
    request.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
    let opened = done(&request).await;
    request.set_onupgradeneeded(None);
    drop(upgrade);
    let database: web_sys::IdbDatabase = opened.map_err(|_| unavailable())?.dyn_into().map_err(|_| unavailable())?;
    *cached.borrow_mut() = Some(database.clone());
    Ok(database)
}

fn store(database: &web_sys::IdbDatabase, mode: web_sys::IdbTransactionMode) -> Result<(web_sys::IdbTransaction, web_sys::IdbObjectStore), String> {
    let transaction = database.transaction_with_str_and_mode(STORE, mode).map_err(|_| unavailable())?;
    let store = transaction.object_store(STORE).map_err(|_| unavailable())?;
    Ok((transaction, store))
}

/// Every key of `slot`.
fn range(slot: &str) -> Result<web_sys::IdbKeyRange, String> {
    web_sys::IdbKeyRange::bound(&JsValue::from_str(&format!("{slot}/")), &JsValue::from_str(&format!("{slot}/\u{ffff}"))).map_err(|_| unavailable())
}

/// A stored value's bytes; none if it isn't bytes (so not NodeCraft's).
fn bytes(value: &JsValue) -> Option<&js_sys::Uint8Array> {
    value.dyn_ref::<js_sys::Uint8Array>()
}

impl Shelf for IndexedDb {
    fn slots(&mut self) -> PickFuture<Inventory> {
        let cached = Rc::clone(&self.database);
        Box::pin(async move {
            let database = open(cached).await?;
            let (_, store) = store(&database, web_sys::IdbTransactionMode::Readonly)?;
            // One value at a time: an origin's inventory must not materialize all journals
            // together merely to count bytes. Refuse an incomplete inventory at the key cap.
            let request = store.open_cursor().map_err(|_| unavailable())?;
            let mut slots = Inventory::new();
            let mut seen = 0u32;
            loop {
                let value = done(&request).await?;
                if value.is_null() || value.is_undefined() {
                    return Ok(slots);
                }
                if seen >= MAX_KEYS {
                    return Err("this site's recovery storage has too many records to inspect safely".into());
                }
                seen = seen.saturating_add(1);
                let cursor: web_sys::IdbCursorWithValue = value.dyn_into().map_err(|_| unavailable())?;
                if let Some(key) = cursor.key().ok().and_then(|key| key.as_string())
                    && let Some((slot, part)) = key.split_once('/')
                    && is_slot(slot)
                {
                    let entry: &mut Slot = slots.entry(slot.to_owned()).or_default();
                    let value = cursor.value().map_err(|_| unavailable())?;
                    let payload_bytes = bytes(&value).map_or(0, |bytes| u64::from(bytes.length()));
                    entry.bytes = entry.bytes.saturating_add(payload_bytes);
                    if part == "h" {
                        entry.hold_bytes = Some(payload_bytes);
                    }
                    entry.dirty |= part == "h" || part.starts_with("f/");
                    entry.checkpoint |= part == "c";
                }
                cursor.continue_().map_err(|_| unavailable())?;
            }
        })
    }

    fn read(&mut self, slot: &str) -> PickFuture<Stored> {
        let cached = Rc::clone(&self.database);
        let slot = slot.to_owned();
        Box::pin(async move {
            let database = open(cached).await?;
            let (_, store) = store(&database, web_sys::IdbTransactionMode::Readonly)?;
            let range = range(&slot)?;
            // Keys and values come in the same order, from the same transaction.
            let keys = store.get_all_keys_with_key(&range).map_err(|_| unavailable())?;
            let values = store.get_all_with_key(&range).map_err(|_| unavailable())?;
            let keys = js_sys::Array::from(&done(&keys).await?);
            let values = js_sys::Array::from(&done(&values).await?);
            let mut stored = Stored::default();
            let mut total = 0usize;
            for (key, value) in keys.iter().zip(values.iter()) {
                let Some(key) = key.as_string() else { continue };
                let Some(part) = key.strip_prefix(&slot).and_then(|rest| rest.strip_prefix('/')) else { continue };
                if part == "h" {
                    stored.held = true;
                    continue;
                }
                let Some(value) = bytes(&value) else { continue };
                total = total.saturating_add(value.length() as usize);
                // More than a journal holds wasn't written by this build; what fits is replayed,
                // and flagged as cut short.
                if total > MAX_JOURNAL_BYTES {
                    stored.truncated = true;
                    break;
                }
                match part {
                    "c" => stored.checkpoint = Some(value.to_vec()),
                    frame => {
                        if let Some(seq) = frame.strip_prefix("f/").and_then(|seq| seq.parse::<u64>().ok()) {
                            stored.frames.push((seq, value.to_vec()));
                        }
                    }
                }
            }
            stored.frames.sort_by_key(|(seq, _)| *seq);
            Ok(stored)
        })
    }

    fn write(&mut self, writes: Vec<Write>) -> PickFuture<()> {
        let cached = Rc::clone(&self.database);
        Box::pin(async move {
            let database = open(cached).await?;
            let (transaction, store) = store(&database, web_sys::IdbTransactionMode::Readwrite)?;
            let put = |key: String, value: &[u8]| store.put_with_key(&js_sys::Uint8Array::from(value), &JsValue::from_str(&key)).map(|_| ());
            for write in writes {
                let queued = match write {
                    Write::Checkpoint { slot, bytes } => {
                        let range = range(&slot)?;
                        store.delete(&range).and_then(|_| put(format!("{slot}/c"), &bytes))
                    }
                    Write::Frame { slot, seq, bytes } => put(format!("{slot}/f/{seq:020}"), &bytes),
                    Write::Hold { slot } => put(format!("{slot}/h"), &[1]),
                    Write::Remove { slot } => {
                        let range = range(&slot)?;
                        store.delete(&range).map(|_| ())
                    }
                };
                if let Err(error) = queued {
                    let _ = transaction.abort();
                    return Err(refused(&error_name(&error)));
                }
            }
            committed(&transaction).await.map_err(|name| refused(&name))
        })
    }

    fn claim(&mut self, slot: &str) -> PickFuture<bool> {
        if self.held.borrow().contains_key(slot) {
            return Box::pin(std::future::ready(Ok(false)));
        }
        let name = format!("{LOCK_PREFIX}{slot}");
        let slot = slot.to_owned();
        let held = Rc::clone(&self.held);
        Box::pin(async move {
            let live = Rc::new(Cell::new(true));
            let mut pending = PendingClaim { live: Rc::clone(&live), held: Rc::clone(&held), slot: slot.clone(), keep: false };
            let missing = || "this browser can't tell its tabs apart (no Web Locks)".to_owned();
            let navigator: JsValue = web_sys::window().ok_or_else(missing)?.navigator().into();
            let locks = js_sys::Reflect::get(&navigator, &JsValue::from_str("locks")).ok().filter(|locks| locks.is_object()).ok_or_else(missing)?;
            let request: js_sys::Function =
                js_sys::Reflect::get(&locks, &JsValue::from_str("request")).ok().and_then(|f| f.dyn_into().ok()).ok_or_else(missing)?;
            let options = js_sys::Object::new();
            js_sys::Reflect::set(&options, &JsValue::from_str("ifAvailable"), &JsValue::TRUE).map_err(|_| missing())?;
            // A successful claim stays held until release (or page exit). Keep its resolver so
            // temporary scan/pruning claims cannot reserve abandoned work for this page's life.
            let granted = js_sys::Promise::new(&mut |resolve, _reject| {
                let refused = resolve.clone();
                let (live, held, slot) = (Rc::clone(&live), Rc::clone(&held), slot.clone());
                let callback = Closure::once_into_js(move |lock: JsValue| -> JsValue {
                    let free = live.get() && !lock.is_null() && !lock.is_undefined();
                    let lifetime: JsValue = if free {
                        js_sys::Promise::new(&mut |release, _| {
                            held.borrow_mut().insert(slot.clone(), release);
                        })
                        .into()
                    } else {
                        JsValue::UNDEFINED
                    };
                    let _ = resolve.call1(&JsValue::NULL, &JsValue::from_bool(free));
                    lifetime
                });
                let args = js_sys::Array::of3(&JsValue::from_str(&name), &options, &callback);
                if request.apply(&locks, &args).is_err() {
                    let _ = refused.call1(&JsValue::NULL, &JsValue::FALSE);
                }
            });
            let free = JsFuture::from(granted).await.map_err(|_| missing())?;
            pending.keep = free.as_bool() == Some(true);
            Ok(pending.keep)
        })
    }

    fn release(&mut self, slot: &str) {
        if let Some(release) = self.held.borrow_mut().remove(slot) {
            let _ = release.call0(&JsValue::NULL);
        }
    }
}

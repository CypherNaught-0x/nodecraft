//! Image previews on cards (#386): thumbnails decoded off the frame and kept as GPU textures in a
//! bounded cache.
//!
//! Thumbnails are keyed by asset hash and a size bucket, so zooming in asks for a sharper one
//! while the smaller one keeps showing. Natively, one worker thread decodes (within `media`'s
//! default limits) and the card shows a placeholder until it's ready. Browsers
//! have no worker here yet, so small images decode inline and larger ones say so.
//!
//! # What the cache bounds (#396)
//!
//! - **Texture bytes.** The RGBA bytes of every texture the cache holds, plus a worst-case
//!   reservation (`edge × edge × 4`) for every decode in flight, never exceed
//!   [`MAX_TEXTURE_BYTES`]. A decode is admitted against that budget *before* its asset is read
//!   or decoded. Room is made only from least-recently-used textures drawn neither this frame nor
//!   the last, so a card drawn early in a frame can't push out one still on screen that is drawn
//!   later. When that can't make room, the card shows "Preview cache full" (or a smaller ready
//!   size of the same image, pinned before the sharper size is admitted) and asks for one more
//!   frame, in which cards that left the view have become evictable. A stable visible set larger
//!   than the budget stays partly uncached rather than churning.
//! - **Metadata.** At most [`MAX_ENTRIES`] entries of any kind, of which at most [`MAX_FAILED`]
//!   are remembered failures (in flight decodes hold a failure slot, so a completion always
//!   fits; while every slot belongs to failures drawn in the last two frames, no new preview,
//!   good or broken, is admitted). A failure is remembered while it is still asked for, so a
//!   visible broken image is decoded once; one nobody asked for in the last frame is forgotten
//!   after [`FAILED_IDLE_SECS`], and the cache schedules the repaints that make that happen
//!   without further input, a finite number of them per input frame.
//! - **Reservations** belong to one request ticket: a completion releases its reservation only
//!   when its ticket matches the pending entry, so a stale result can't free another request's
//!   budget. A worker that dies releases them all.
//!
//! This is a cache ownership limit, not a process memory limit: the decoder's own working memory
//! (bounded separately by its decoding limits), egui's copy of the pixels while it uploads them, the GPU
//! driver's allocations and the encoded bytes being read are not counted.
use egui::{ColorImage, TextureHandle, TextureOptions};
use nodecraft_media::{Limits, decode_image, thumbnail};
use nodecraft_types::Blake3;
use std::collections::HashMap;

/// Edge lengths, in pixels, thumbnails are made at.
pub const BUCKETS: [u32; 3] = [128, 256, 512];
/// Texture memory previews may hold at once (RGBA bytes), counting reservations for decodes in
/// flight.
pub const MAX_TEXTURE_BYTES: usize = 96 * 1024 * 1024;
/// Thumbnails being decoded at once; more requests wait for a later frame.
pub const MAX_IN_FLIGHT: usize = 4;
/// Largest image decoded inline where there's no worker (the browser build).
pub const MAX_INLINE_BYTES: usize = 512 * 1024;

/// Decoding limits for previews: `media`'s, which admit a 16k×16k RGBA image (1 GiB). The worker
/// decodes one at a time, so that is the most a preview holds at once.
fn limits() -> Limits {
    Limits::default()
}
/// Decoded size above which the browser build doesn't decode inline (it has no worker).
pub const MAX_INLINE_PIXEL_BYTES: u64 = 64 * 1024 * 1024;
/// Times a worker that died is started again before previews give up for the session.
pub const MAX_RESTARTS: u32 = 3;
/// Entries of any kind the cache keeps: textures, decodes in flight and remembered failures.
pub const MAX_ENTRIES: usize = 1024;
/// Remembered failures, decodes in flight included (each holds the slot its failure would take).
pub const MAX_FAILED: usize = 256;
/// Seconds a failure nobody asks for is remembered, so the asset is tried again after it changes.
pub const FAILED_IDLE_SECS: f64 = 1.0;
/// What a card says when the budget has no room for its preview this frame.
pub const CACHE_FULL: &str = "Preview cache full";

/// The smallest bucket at least `edge` pixels, or the largest.
pub fn bucket(edge: f32) -> u32 {
    let edge = if edge.is_finite() { edge.max(0.0) } else { 0.0 };
    BUCKETS.into_iter().find(|b| *b as f32 >= edge).or(BUCKETS.last().copied()).unwrap_or(512)
}

/// Edge lengths, in pixels, a PDF reader's pages are drawn at: thumbnails, then the page.
pub const PAGE_BUCKETS: [u32; 3] = [256, 1024, 2048];

/// An asset, the edge it is drawn at and its page (from 0; always 0 for an image).
type Key = (Blake3, u32, u32);

enum Input {
    Resident(std::sync::Arc<[u8]>),
    Asset(nodecraft_format::PreviewAssetRead),
    #[cfg(test)]
    Panic {
        entered: std::sync::mpsc::SyncSender<()>,
        proceed: std::sync::mpsc::Receiver<()>,
    },
}
#[cfg(not(target_arch = "wasm32"))]
enum Completion {
    Finished(Result<DecodedPreview, &'static str>),
    Busy,
}
enum CaptureError {
    Missing,
    Failed(&'static str),
}
fn read_message(reason: nodecraft_format::PreviewReadError) -> &'static str {
    use nodecraft_format::PreviewReadError as E;
    match reason {
        E::Missing => "This image's data is missing",
        E::TooLarge => "Preview not available here",
        E::Busy => "Preview busy; try again",
        E::Corrupt => "Can't preview this image",
        E::Cancelled => "Preview cancelled",
        E::Unavailable | E::Unreadable | E::Capacity => "Image preview unavailable",
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn consume(input: Input, edge: u32, page: u32, cancel: &nodecraft_format::asset_source::TransferCancel) -> Completion {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &input {
        Input::Resident(bytes) => Completion::Finished(decode(bytes, edge, page)),
        #[cfg(test)]
        Input::Panic { entered, proceed } => tests::panic_attempt(entered, proceed),
        Input::Asset(source) => match source.read_verified(cancel, &Default::default()) {
            Ok(bytes) => Completion::Finished(decode(&bytes, edge, page)),
            Err(nodecraft_format::PreviewReadError::Busy) => Completion::Busy,
            Err(why) => Completion::Finished(Err(read_message(why))),
        },
    }))
    .unwrap_or(Completion::Finished(Err("Can't preview this image")))
    // Input and any bounded encoded buffer are dropped before the result is enqueued.
}
fn inline_input(input: Input, edge: u32, page: u32) -> Result<DecodedPreview, &'static str> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let bytes = match &input {
            Input::Resident(bytes) => bytes.as_ref(),
            #[cfg(test)]
            Input::Panic { entered, proceed } => tests::panic_attempt(entered, proceed),
            Input::Asset(source) => source.resident().ok_or("Preview not available here")?,
        };
        if bytes.len() > MAX_INLINE_BYTES {
            return Err("Preview not available here");
        }
        // Byte size does not bound SVG raster work. Native SVG uses the worker; browsers
        // retain/export the original until a bounded off-frame renderer is available.
        #[cfg(target_arch = "wasm32")]
        if nodecraft_media::svg::recognizes(bytes) {
            return Err("SVG preview needs a native worker; the original can still be exported");
        }
        if let Input::Asset(source) = &input {
            source.validate_image(bytes).map_err(read_message)?;
        }
        let decoded = nodecraft_media::probe_image(bytes, &limits()).map(|m| u64::from(m.width).saturating_mul(u64::from(m.height)).saturating_mul(4));
        if decoded.is_ok_and(|n| n > MAX_INLINE_PIXEL_BYTES) {
            return Err("Preview not available here");
        }
        decode(bytes, edge, page)
    }))
    .unwrap_or(Err("Can't preview this image"))
}
enum Entry {
    /// A decode in flight for request `ticket`, holding `reserved` bytes of the texture budget.
    /// Browsers have no worker, so they never have one in flight.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Pending {
        #[cfg(not(target_arch = "wasm32"))]
        ticket: u64,
        reserved: usize,
    },
    Ready {
        texture: TextureHandle,
        size: [u32; 2],
        source_size: [u32; 2],
        bytes: usize,
        used: u64,
    },
    /// A remembered failure, last asked for on frame `used` at `seen` seconds.
    Failed {
        why: &'static str,
        used: u64,
        seen: f64,
    },
}

/// What a card can show for its image right now.
#[derive(Clone, Copy)]
pub enum Preview {
    Ready { texture: egui::TextureId, size: [u32; 2], source_size: [u32; 2] },
    Pending,
    Unavailable(&'static str),
}

/// Thumbnail pixels and the decoded source's upright dimensions have distinct purposes.
struct DecodedPreview {
    image: ColorImage,
    source_size: [u32; 2],
}

/// A thumbnail decoded from `bytes` at most `edge` pixels on its longer side (a PDF's page `page`,
/// from 0). A panic inside the decoder is caught and reported as a failure, so one bad file can't
/// stop previews.
fn decode(bytes: &[u8], edge: u32, page: u32) -> Result<DecodedPreview, &'static str> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode_unguarded(bytes, edge, page))).unwrap_or(Err("Can't preview this image"))
}

fn decode_unguarded(bytes: &[u8], edge: u32, page: u32) -> Result<DecodedPreview, &'static str> {
    // A PDF's card preview is its first page (#113), the reader's any page (#590), drawn over
    // white at the thumbnail's size.
    if bytes.starts_with(b"%PDF-") {
        let document = nodecraft_media::Document::open(bytes, &Default::default()).map_err(|_| "Can't preview this PDF")?;
        let page = document.render(page, edge, [255, 255, 255, 255]).map_err(|_| "Can't preview this PDF")?;
        let size = [usize::try_from(page.width).map_err(|_| "Can't preview this PDF")?, usize::try_from(page.height).map_err(|_| "Can't preview this PDF")?];
        return Ok(DecodedPreview { image: ColorImage::from_rgba_unmultiplied(size, &page.rgba), source_size: [page.width, page.height] });
    }
    if page != 0 {
        return Err("Can't preview this image");
    }
    if nodecraft_media::svg::recognizes(bytes) {
        let raster = nodecraft_media::svg::render(bytes, edge, &limits()).map_err(|_| "SVG original retained; this preview is unavailable")?;
        let size = [raster.width as usize, raster.height as usize];
        return Ok(DecodedPreview { image: ColorImage::from_rgba_unmultiplied(size, &raster.rgba), source_size: [raster.source_width, raster.source_height] });
    }
    let (meta, image) = decode_image(bytes, &limits()).map_err(|_| "Can't preview this image")?;
    let thumb = thumbnail(&image, edge).map_err(|_| "Can't preview this image")?;
    let size =
        [usize::try_from(thumb.width()).map_err(|_| "Can't preview this image")?, usize::try_from(thumb.height()).map_err(|_| "Can't preview this image")?];
    Ok(DecodedPreview { image: ColorImage::from_rgba_unmultiplied(size, thumb.rgba()), source_size: [meta.width, meta.height] })
}

/// The most texture a decode at `edge` pixels can produce: thumbnails and PDF pages fit within
/// `edge` on their longer side.
fn reservation(edge: u32) -> usize {
    let edge = usize::try_from(edge).unwrap_or(usize::MAX);
    edge.saturating_mul(edge).saturating_mul(4)
}

#[cfg(not(target_arch = "wasm32"))]
mod worker {
    use super::{Completion, Input, Key, consume};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    };

    /// A finished decode: what for, the request ticket it answers and how it went.
    pub(super) type Done = (Key, u64, Completion);
    /// A decode to do: what for, its request ticket, the encoded bytes and the edge length.
    type Job = (Key, u64, Input, u32);

    /// One thread that decodes thumbnails; it ends when the cache drops its sender.
    pub(super) struct Worker {
        jobs: Option<SyncSender<Job>>,
        pub(super) done: Receiver<Done>,
        handle: Option<std::thread::JoinHandle<()>>,
        /// Set on shutdown: queued jobs are skipped, so closing waits for one decode at most.
        cancelled: Arc<AtomicBool>,
        read_cancel: nodecraft_format::asset_source::TransferCancel,
    }

    impl Worker {
        pub(super) fn start(ctx: egui::Context) -> Option<Self> {
            let (jobs, queue) = sync_channel::<Job>(super::MAX_IN_FLIGHT);
            let (send, done) = sync_channel::<Done>(super::MAX_IN_FLIGHT.saturating_mul(2));
            let cancelled = Arc::new(AtomicBool::new(false));
            let stop = Arc::clone(&cancelled);
            let read_cancel = nodecraft_format::asset_source::TransferCancel::default();
            let active_cancel = read_cancel.clone();
            let handle = std::thread::Builder::new()
                .name("nodecraft-previews".into())
                .spawn(move || {
                    for (key, ticket, input, edge) in queue {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let result = consume(input, edge, key.2, &active_cancel);
                        let wake = !matches!(result, Completion::Busy);
                        if send.send((key, ticket, result)).is_err() {
                            break;
                        }
                        if wake {
                            ctx.request_repaint();
                        }
                    }
                })
                .ok()?;
            Some(Self { jobs: Some(jobs), done, handle: Some(handle), cancelled, read_cancel })
        }

        pub(super) fn cancel(&self) {
            self.cancelled.store(true, Ordering::Relaxed);
            self.read_cancel.cancel();
        }

        /// Closes the job queue, so the thread ends once it has drained it, as a dying one would.
        #[cfg(test)]
        pub(super) fn close_queue(&mut self) {
            self.jobs = None;
        }

        /// Queues a decode; `false` when the worker is busy (try again next frame) or gone.
        pub(super) fn submit(&self, key: Key, ticket: u64, input: Input, edge: u32) -> bool {
            match self.jobs.as_ref().map(|jobs| jobs.try_send((key, ticket, input, edge))) {
                Some(Ok(())) => true,
                Some(Err(TrySendError::Full(_) | TrySendError::Disconnected(_))) | None => false,
            }
        }
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            // Closing the queue and cancelling ends the thread after its current decode, skipping
            // anything still queued.
            self.cancel();
            self.jobs = None;
            while self.done.try_recv().is_ok() {}
            if let Some(handle) = self.handle.take() {
                // A decode that panicked is already over; nothing more to do with it.
                let _ = handle.join();
            }
        }
    }
}

/// Asks for a frame about `due` seconds (at most [`FAILED_IDLE_SECS`]) from now. egui brings a
/// delayed repaint forward by a frame, so this asks a frame later; that also keeps a request due
/// now a delayed one, which egui runs once rather than twice.
fn wake_after(ctx: &egui::Context, due: f64) {
    let frame = f64::from(ctx.input(|i| i.predicted_dt));
    let frame = if frame.is_finite() { frame.clamp(0.001, FAILED_IDLE_SECS) } else { 0.001 };
    let due = if due.is_finite() { due.clamp(0.0, FAILED_IDLE_SECS) } else { 0.0 };
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(due + frame));
}

/// How a request's attempt to start a decode went.
enum Start {
    /// Started, queued, recorded as failed, or waiting for a free decode slot.
    Tried,
    /// The asset's data is missing; nothing is remembered, so it previews once it's back.
    Missing,
    /// The budget has no room for it this frame; nothing was read or decoded.
    Full,
}

/// Which entries [`Previews::evict_lru`] may drop.
#[derive(Clone, Copy)]
enum Victims {
    Textures,
    Failures,
    Either,
}

/// The preview cache. One per app.
#[derive(Default)]
pub struct Previews {
    entries: HashMap<Key, Entry>,
    /// Bytes of the textures held.
    bytes: usize,
    /// Bytes reserved by pending decodes.
    reserved: usize,
    /// Remembered failures.
    failed: usize,
    clock: u64,
    /// The frame's `egui` time, in seconds.
    now: f64,
    #[cfg(not(target_arch = "wasm32"))]
    next_ticket: u64,
    in_flight: usize,
    busy_this_frame: [Option<Key>; MAX_IN_FLIGHT],
    /// The smallest reservation refused this frame: nothing frees capacity until the next poll,
    /// so anything as large is refused without scanning again. `Some(0)`: metadata is full.
    refused: Option<usize>,
    /// A capacity refusal asked for another frame, which this poll may be.
    retry_asked: bool,
    /// This frame may be the one a refusal asked for.
    retrying: bool,
    /// When the repaint scheduled to look for failures that stopped being asked for is due.
    check_at: Option<f64>,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<worker::Worker>,
    #[cfg(not(target_arch = "wasm32"))]
    worker_failed: bool,
    #[cfg(not(target_arch = "wasm32"))]
    restarts: u32,
}

impl Previews {
    /// Texture bytes held now, not counting reservations for decodes in flight.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Takes finished decodes into textures and forgets idle failures; call once per frame,
    /// before any request.
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.clock = self.clock.wrapping_add(1);
        self.busy_this_frame.fill(None);
        self.refused = None;
        self.retrying = std::mem::take(&mut self.retry_asked);
        let now = ctx.input(|i| i.time);
        if now.is_finite() {
            self.now = now;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::sync::mpsc::TryRecvError;
            let mut finished = Vec::new();
            let mut died = false;
            if let Some(worker) = &self.worker {
                loop {
                    match worker.done.try_recv() {
                        Ok(done) => finished.push(done),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            died = true;
                            break;
                        }
                    }
                }
            }
            for (key, ticket, result) in finished {
                self.complete(ctx, key, ticket, result);
            }
            if died {
                self.worker_died();
            }
        }
        self.expire(ctx);
    }

    /// Applies a worker's result, if it answers the request still pending for `key`.
    #[cfg(not(target_arch = "wasm32"))]
    fn complete(&mut self, ctx: &egui::Context, key: Key, ticket: u64, result: Completion) {
        let reserved = match self.entries.get(&key) {
            Some(Entry::Pending { ticket: pending, reserved }) if *pending == ticket => *reserved,
            // Stale: that request is gone, and its reservation with it.
            _ => return,
        };
        self.remove(&key);
        match result {
            Completion::Finished(result) => self.finish(ctx, key, reserved, result),
            Completion::Busy => {
                if let Some(slot) = self.busy_this_frame.iter_mut().find(|slot| slot.is_none()) {
                    *slot = Some(key);
                }
            }
        }
    }

    /// Forgets failures nobody asked for last frame once they're [`FAILED_IDLE_SECS`] old, and
    /// asks for the repaints that let that happen without input.
    fn expire(&mut self, ctx: &egui::Context) {
        let (now, last) = (self.now, self.clock.wrapping_sub(1));
        let is_check = self.check_at.is_some_and(|due| now >= due);
        if is_check {
            self.check_at = None;
        }
        // A clock that went backwards (or isn't a number) counts as long idle.
        let idle_for = |seen: f64| {
            let idle = now - seen;
            if idle.is_finite() && idle >= 0.0 { idle } else { f64::INFINITY }
        };
        let expired: Vec<Key> = self
            .entries
            .iter()
            .filter(|(_, entry)| matches!(entry, Entry::Failed { used, seen, .. } if *used != last && idle_for(*seen) >= FAILED_IDLE_SECS))
            .map(|(key, _)| *key)
            .collect();
        for key in &expired {
            self.remove(key);
        }
        let mut wake: Option<f64> = None;
        let mut asked_last_frame = false;
        for entry in self.entries.values() {
            if let Entry::Failed { used, seen, .. } = entry {
                if *used == last {
                    asked_last_frame = true;
                } else {
                    let due = (FAILED_IDLE_SECS - idle_for(*seen)).max(0.0);
                    wake = Some(wake.map_or(due, |w| w.min(due)));
                }
            }
        }
        // A failure asked for last frame may stop being asked for this one, which only a later
        // frame can see. Look once, a second after each frame that wasn't itself that look; a
        // stable view then needs no more frames.
        if asked_last_frame && !is_check && self.check_at.is_none() {
            self.check_at = Some(now + FAILED_IDLE_SECS);
        }
        if let Some(due) = self.check_at {
            let due = (due - now).max(0.0);
            wake = Some(wake.map_or(due, |w| w.min(due)));
        }
        if let Some(due) = wake.filter(|due| due.is_finite()) {
            // A wake due within a frame would become immediate, which a test harness (or a
            // pointer held down) would then run through frame after frame.
            wake_after(ctx, due);
        }
    }

    /// The worker ended without being asked: forget what it was decoding, releasing its
    /// reservations, so those images are asked for again, and allow a fresh worker a few times
    /// before giving up for the session.
    #[cfg(not(target_arch = "wasm32"))]
    fn worker_died(&mut self) {
        self.worker = None;
        let pending: Vec<Key> = self.entries.iter().filter(|(_, entry)| matches!(entry, Entry::Pending { .. })).map(|(key, _)| *key).collect();
        for key in &pending {
            self.remove(key);
        }
        self.in_flight = 0;
        self.reserved = 0;
        self.restarts = self.restarts.saturating_add(1);
        self.worker_failed = self.restarts > MAX_RESTARTS;
    }

    /// Removes `key`, releasing what it held.
    fn remove(&mut self, key: &Key) -> Option<Entry> {
        let entry = self.entries.remove(key)?;
        match &entry {
            Entry::Pending { reserved, .. } => {
                self.reserved = self.reserved.saturating_sub(*reserved);
                self.in_flight = self.in_flight.saturating_sub(1);
            }
            Entry::Ready { bytes, .. } => self.bytes = self.bytes.saturating_sub(*bytes),
            Entry::Failed { .. } => self.failed = self.failed.saturating_sub(1),
        }
        Some(entry)
    }

    /// Stores `entry` at `key`, replacing (and releasing) what was there.
    fn put(&mut self, key: Key, entry: Entry) {
        self.remove(&key);
        match &entry {
            Entry::Pending { reserved, .. } => {
                self.reserved = self.reserved.saturating_add(*reserved);
                self.in_flight = self.in_flight.saturating_add(1);
            }
            Entry::Ready { bytes, .. } => self.bytes = self.bytes.saturating_add(*bytes),
            Entry::Failed { .. } => self.failed = self.failed.saturating_add(1),
        }
        self.entries.insert(key, entry);
    }

    fn fail(&mut self, key: Key, why: &'static str) {
        self.put(key, Entry::Failed { why, used: self.clock, seen: self.now });
    }

    /// Turns a decode admitted with `allowed` bytes into a texture or a remembered failure.
    fn finish(&mut self, ctx: &egui::Context, key: Key, allowed: usize, result: Result<DecodedPreview, &'static str>) {
        match result {
            Ok(DecodedPreview { image, source_size }) => {
                let bytes = image.size[0].saturating_mul(image.size[1]).saturating_mul(4);
                // Never more than was admitted, nor past the budget: a decoder that ignored its
                // edge is a failure, not a reason to overrun.
                if bytes > allowed || self.bytes.saturating_add(self.reserved).saturating_add(bytes) > MAX_TEXTURE_BYTES {
                    return self.fail(key, "Preview not available here");
                }
                let size = [u32::try_from(image.size[0]).unwrap_or(u32::MAX), u32::try_from(image.size[1]).unwrap_or(u32::MAX)];
                let texture = ctx.load_texture(format!("preview-{:?}-{}-{}", key.0, key.1, key.2), image, TextureOptions::LINEAR);
                self.put(key, Entry::Ready { texture, size, source_size, bytes, used: self.clock });
            }
            Err(why) => self.fail(key, why),
        }
    }

    /// Makes room for one more entry that may hold `need` texture bytes, and a failure slot, by
    /// dropping least recently used entries drawn neither this frame nor the last; `false` when
    /// it can't.
    ///
    /// A refusal asks for one more frame: entries drawn last frame but not this one (cards that
    /// left the view) are evictable then. A frame that was itself that retry, without input, asks
    /// for no more, so a stable view larger than the budget doesn't repaint forever.
    fn admit(&mut self, ctx: &egui::Context, need: usize) -> bool {
        if self.refused.is_some_and(|refused| need >= refused) {
            return false;
        }
        loop {
            let (victims, refused) = if self.bytes.saturating_add(self.reserved).saturating_add(need) > MAX_TEXTURE_BYTES {
                (Victims::Textures, need)
            } else if self.entries.len() >= MAX_ENTRIES {
                (Victims::Either, 0)
            } else if self.failed.saturating_add(self.in_flight) >= MAX_FAILED {
                (Victims::Failures, 0)
            } else {
                return true;
            };
            if !self.evict_lru(victims) {
                self.refused = Some(self.refused.map_or(refused, |r| r.min(refused)));
                let input = ctx.input(|i| !i.events.is_empty());
                if !self.retry_asked && (!self.retrying || input) {
                    self.retry_asked = true;
                    // Exactly one more frame: an immediate request would make egui run two.
                    wake_after(ctx, 0.0);
                }
                return false;
            }
        }
    }

    /// Drops the least recently used of `victims` drawn neither this frame nor the last; `false`
    /// if none.
    fn evict_lru(&mut self, victims: Victims) -> bool {
        let recent = self.clock.saturating_sub(1);
        let oldest = self
            .entries
            .iter()
            .filter_map(|(key, entry)| match (entry, victims) {
                (Entry::Ready { used, .. }, Victims::Textures | Victims::Either) | (Entry::Failed { used, .. }, Victims::Failures | Victims::Either)
                    if *used < recent =>
                {
                    Some((*used, *key))
                }
                _ => None,
            })
            .min();
        match oldest {
            Some((_, key)) => self.remove(&key).is_some(),
            None => false,
        }
    }

    /// The preview for asset `hash` at about `edge` pixels. `bytes` reads the asset, and is only
    /// called when a decode is about to start.
    pub fn request(&mut self, ctx: &egui::Context, hash: Blake3, edge: f32, bytes: impl FnOnce() -> Option<Vec<u8>>) -> Preview {
        self.request_input(ctx, (hash, bucket(edge), 0), &BUCKETS, || {
            let data = bytes().ok_or(CaptureError::Missing)?;
            if data.len() as u64 > limits().max_input_bytes {
                return Err(CaptureError::Failed("Preview not available here"));
            }
            Ok(Input::Resident(std::sync::Arc::from(data)))
        })
    }
    /// Native/lazy-safe input capture. The factory runs only after cache/queue admission;
    /// source bodies are consumed in the worker, never by this frame.
    pub fn request_asset(
        &mut self,
        ctx: &egui::Context,
        hash: Blake3,
        edge: f32,
        source: impl FnOnce() -> Result<nodecraft_format::PreviewAssetRead, nodecraft_format::PreviewReadError>,
    ) -> Preview {
        self.request_input(ctx, (hash, bucket(edge), 0), &BUCKETS, || match source() {
            Ok(input) if input.hash() == hash => Ok(Input::Asset(input)),
            Ok(_) => Err(CaptureError::Failed("Can't preview this image")),
            Err(nodecraft_format::PreviewReadError::Missing) => Err(CaptureError::Missing),
            Err(why) => Err(CaptureError::Failed(read_message(why))),
        })
    }
    /// Page `page` (from 0) of the PDF asset `hash`, drawn at about `edge` pixels on its longer
    /// side (one of [`PAGE_BUCKETS`]), for the PDF reader (#590).
    pub fn request_page(
        &mut self,
        ctx: &egui::Context,
        hash: Blake3,
        page: u32,
        edge: f32,
        source: impl FnOnce() -> Result<nodecraft_format::PreviewAssetRead, nodecraft_format::PreviewReadError>,
    ) -> Preview {
        let edge = if edge.is_finite() { edge.max(0.0) } else { 0.0 };
        let size = PAGE_BUCKETS.into_iter().find(|b| *b as f32 >= edge).unwrap_or(2048);
        self.request_input(ctx, (hash, size, page), &PAGE_BUCKETS, || match source() {
            Ok(input) if input.hash() == hash => Ok(Input::Asset(input)),
            Ok(_) => Err(CaptureError::Failed("Can't preview this PDF")),
            Err(nodecraft_format::PreviewReadError::Missing) => Err(CaptureError::Missing),
            Err(why) => Err(CaptureError::Failed(read_message(why))),
        })
    }
    fn request_input(&mut self, ctx: &egui::Context, wanted: Key, sizes: &[u32], source: impl FnOnce() -> Result<Input, CaptureError>) -> Preview {
        let (hash, _, page) = wanted;
        let (clock, now) = (self.clock, self.now);
        match self.entries.get_mut(&wanted) {
            Some(Entry::Ready { texture, size, source_size, used, .. }) => {
                *used = clock;
                return Preview::Ready { texture: texture.id(), size: *size, source_size: *source_size };
            }
            Some(Entry::Failed { why, used, seen }) => {
                (*used, *seen) = (clock, now);
                return Preview::Unavailable(why);
            }
            Some(Entry::Pending { .. }) | None => {}
        }
        // While a sharper one decodes, any other size of the same image will do. It is pinned
        // first, so making room for the sharper one can't evict it.
        let mut show = None;
        for bucket in sizes.iter().rev() {
            if let Some(Entry::Ready { texture, size, source_size, used, .. }) = self.entries.get_mut(&(hash, *bucket, page)) {
                *used = clock;
                show = Some(Preview::Ready { texture: texture.id(), size: *size, source_size: *source_size });
                break;
            }
        }
        let busy = self.busy_this_frame.contains(&Some(wanted));
        let started = if self.entries.contains_key(&wanted) || busy { Start::Tried } else { self.start(ctx, wanted, source) };
        match self.entries.get(&wanted) {
            // Decoded inline just now.
            Some(Entry::Ready { texture, size, source_size, .. }) => Preview::Ready { texture: texture.id(), size: *size, source_size: *source_size },
            Some(Entry::Failed { why, .. }) => show.unwrap_or(Preview::Unavailable(why)),
            Some(Entry::Pending { .. }) => show.unwrap_or(Preview::Pending),
            // Nothing started: the data is missing, the cache is full, or decoding is busy until a
            // later frame.
            None => show.unwrap_or(match started {
                Start::Missing => Preview::Unavailable("This image's data is missing"),
                Start::Full => Preview::Unavailable(CACHE_FULL),
                Start::Tried if busy => Preview::Unavailable("Preview busy; try again"),
                Start::Tried => Preview::Pending,
            }),
        }
    }

    /// Starts decoding `key` once the budget admits it, before anything is read.
    fn start(&mut self, ctx: &egui::Context, key: Key, source: impl FnOnce() -> Result<Input, CaptureError>) -> Start {
        if self.in_flight >= MAX_IN_FLIGHT {
            return Start::Tried;
        }
        let need = reservation(key.1);
        if !self.admit(ctx, need) {
            return Start::Full;
        }
        let input = match source() {
            Ok(input) => input,
            Err(CaptureError::Missing) => return Start::Missing,
            Err(CaptureError::Failed(why)) => {
                self.fail(key, why);
                return Start::Tried;
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.worker.is_none() && !self.worker_failed {
                self.worker = worker::Worker::start(ctx.clone());
                self.worker_failed = self.worker.is_none();
            }
            if self.worker.is_some() {
                let ticket = self.next_ticket;
                self.next_ticket = self.next_ticket.wrapping_add(1);
                if self.worker.as_ref().is_some_and(|w| w.submit(key, ticket, input, key.1)) {
                    self.put(key, Entry::Pending { ticket, reserved: need });
                }
                return Start::Tried;
            }
        }
        // No worker: only immutable resident bytes may decode inline. Lazy content
        // is unsupported here, regardless of how small its descriptor is.
        self.finish(ctx, key, need, inline_input(input, key.1, key.2));
        Start::Tried
    }

    #[cfg(test)]
    pub(crate) fn test_pending_and_failed(&self) -> (usize, usize) {
        (self.in_flight, self.failed)
    }

    /// Decodes inline, for tests: no worker, so results are ready in the same frame.
    #[cfg(test)]
    pub(crate) fn inline(ctx: &egui::Context, hash: Blake3, edge: u32, bytes: &[u8]) -> Self {
        let mut previews = Self::default();
        #[cfg(not(target_arch = "wasm32"))]
        {
            previews.worker_failed = true;
        }
        previews.finish(ctx, (hash, edge, 0), reservation(edge), decode(bytes, edge, 0));
        previews
    }
}

#[cfg(test)]
mod tests;

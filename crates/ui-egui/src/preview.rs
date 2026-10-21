//! Image previews on cards (#386): thumbnails decoded off the frame and kept as GPU textures in a
//! bounded cache.
//!
//! Thumbnails are keyed by asset hash and a size bucket, so zooming in asks for a sharper one
//! while the smaller one keeps showing. Natively, one worker thread decodes (within tighter
//! limits than `media`'s defaults) and the card shows a placeholder until it's ready. Browsers
//! have no worker here yet, so small images decode inline and larger ones say so. Textures are
//! evicted least recently used past [`MAX_TEXTURE_BYTES`]; decode failures are remembered, so a
//! broken image isn't decoded again every frame.
use egui::{ColorImage, TextureHandle, TextureOptions};
use nodecraft_media::{Limits, decode_image, thumbnail};
use nodecraft_types::Blake3;
use std::collections::HashMap;

/// Edge lengths, in pixels, thumbnails are made at.
pub const BUCKETS: [u32; 3] = [128, 256, 512];
/// Texture memory previews may hold at once (RGBA bytes).
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

/// The smallest bucket at least `edge` pixels, or the largest.
pub fn bucket(edge: f32) -> u32 {
    let edge = if edge.is_finite() { edge.max(0.0) } else { 0.0 };
    BUCKETS.into_iter().find(|b| *b as f32 >= edge).or(BUCKETS.last().copied()).unwrap_or(512)
}

type Key = (Blake3, u32);

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
fn consume(input: Input, edge: u32, cancel: &nodecraft_format::asset_source::TransferCancel) -> Completion {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &input {
        Input::Resident(bytes) => Completion::Finished(decode(bytes, edge)),
        #[cfg(test)]
        Input::Panic { entered, proceed } => tests::panic_attempt(entered, proceed),
        Input::Asset(source) => match source.read_verified(cancel, &Default::default()) {
            Ok(bytes) => Completion::Finished(decode(&bytes, edge)),
            Err(nodecraft_format::PreviewReadError::Busy) => Completion::Busy,
            Err(why) => Completion::Finished(Err(read_message(why))),
        },
    }))
    .unwrap_or(Completion::Finished(Err("Can't preview this image")))
    // Input and any bounded encoded buffer are dropped before the result is enqueued.
}
fn inline_input(input: Input, edge: u32) -> Result<DecodedPreview, &'static str> {
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
        if let Input::Asset(source) = &input {
            source.validate_image(bytes).map_err(read_message)?;
        }
        let decoded = nodecraft_media::probe_image(bytes, &limits()).map(|m| u64::from(m.width).saturating_mul(u64::from(m.height)).saturating_mul(4));
        if decoded.is_ok_and(|n| n > MAX_INLINE_PIXEL_BYTES) {
            return Err("Preview not available here");
        }
        decode(bytes, edge)
    }))
    .unwrap_or(Err("Can't preview this image"))
}

enum Entry {
    Pending,
    Ready { texture: TextureHandle, size: [u32; 2], source_size: [u32; 2], bytes: usize, used: u64 },
    Failed(&'static str),
}

/// What a card can show for its image right now.
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

/// A thumbnail decoded from `bytes` at most `edge` pixels on its longer side. A panic inside the
/// image decoder is caught and reported as a failure, so one bad file can't stop previews.
fn decode(bytes: &[u8], edge: u32) -> Result<DecodedPreview, &'static str> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode_unguarded(bytes, edge))).unwrap_or(Err("Can't preview this image"))
}

fn decode_unguarded(bytes: &[u8], edge: u32) -> Result<DecodedPreview, &'static str> {
    // A PDF's preview is its first page (#113), drawn over white at the thumbnail's size.
    if bytes.starts_with(b"%PDF-") {
        let document = nodecraft_media::Document::open(bytes, &Default::default()).map_err(|_| "Can't preview this PDF")?;
        let page = document.render(0, edge, [255, 255, 255, 255]).map_err(|_| "Can't preview this PDF")?;
        let size = [usize::try_from(page.width).map_err(|_| "Can't preview this PDF")?, usize::try_from(page.height).map_err(|_| "Can't preview this PDF")?];
        return Ok(DecodedPreview { image: ColorImage::from_rgba_unmultiplied(size, &page.rgba), source_size: [page.width, page.height] });
    }
    let (meta, image) = decode_image(bytes, &limits()).map_err(|_| "Can't preview this image")?;
    let thumb = thumbnail(&image, edge).map_err(|_| "Can't preview this image")?;
    let size =
        [usize::try_from(thumb.width()).map_err(|_| "Can't preview this image")?, usize::try_from(thumb.height()).map_err(|_| "Can't preview this image")?];
    Ok(DecodedPreview { image: ColorImage::from_rgba_unmultiplied(size, thumb.rgba()), source_size: [meta.width, meta.height] })
}

#[cfg(not(target_arch = "wasm32"))]
mod worker {
    use super::{Completion, Input, Key, consume};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    };

    pub(super) type Done = (Key, Completion);
    /// A decode to do: what for, the encoded bytes and the edge length.
    type Job = (Key, Input, u32);

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
                    for (key, input, edge) in queue {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let result = consume(input, edge, &active_cancel);
                        let wake = !matches!(result, Completion::Busy);
                        if send.send((key, result)).is_err() {
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

        /// Queues a decode; `false` when the worker is busy (try again next frame) or gone.
        pub(super) fn submit(&self, key: Key, input: Input, edge: u32) -> bool {
            match self.jobs.as_ref().map(|jobs| jobs.try_send((key, input, edge))) {
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

/// The preview cache. One per app.
#[derive(Default)]
pub struct Previews {
    entries: HashMap<Key, Entry>,
    bytes: usize,
    clock: u64,
    in_flight: usize,
    busy_this_frame: [Option<Key>; MAX_IN_FLIGHT],
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<worker::Worker>,
    #[cfg(not(target_arch = "wasm32"))]
    worker_failed: bool,
    #[cfg(not(target_arch = "wasm32"))]
    restarts: u32,
}

impl Previews {
    /// Texture bytes held now.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Takes finished decodes into textures; call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.clock = self.clock.wrapping_add(1);
        self.busy_this_frame.fill(None);
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
            for (key, result) in finished {
                if !matches!(self.entries.get(&key), Some(Entry::Pending)) {
                    continue;
                }
                self.in_flight = self.in_flight.saturating_sub(1);
                match result {
                    Completion::Finished(result) => self.finish(ctx, key, result),
                    Completion::Busy => {
                        self.entries.remove(&key);
                        if let Some(slot) = self.busy_this_frame.iter_mut().find(|slot| slot.is_none()) {
                            *slot = Some(key);
                        }
                    }
                }
            }
            if died {
                self.worker_died();
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = ctx;
    }

    /// The worker ended without being asked: forget what it was decoding, so those images are
    /// asked for again, and allow a fresh worker a few times before giving up for the session.
    #[cfg(not(target_arch = "wasm32"))]
    fn worker_died(&mut self) {
        self.worker = None;
        self.in_flight = 0;
        self.entries.retain(|_, entry| !matches!(entry, Entry::Pending));
        self.restarts = self.restarts.saturating_add(1);
        self.worker_failed = self.restarts > MAX_RESTARTS;
    }

    fn finish(&mut self, ctx: &egui::Context, key: Key, result: Result<DecodedPreview, &'static str>) {
        let entry = match result {
            Ok(DecodedPreview { image, source_size }) => {
                let size = [u32::try_from(image.size[0]).unwrap_or(u32::MAX), u32::try_from(image.size[1]).unwrap_or(u32::MAX)];
                let bytes = image.size[0].saturating_mul(image.size[1]).saturating_mul(4);
                let texture = ctx.load_texture(format!("preview-{:?}-{}", key.0, key.1), image, TextureOptions::LINEAR);
                self.bytes = self.bytes.saturating_add(bytes);
                Entry::Ready { texture, size, source_size, bytes, used: self.clock }
            }
            Err(why) => Entry::Failed(why),
        };
        if let Some(Entry::Ready { bytes, .. }) = self.entries.insert(key, entry) {
            self.bytes = self.bytes.saturating_sub(bytes);
        }
        self.evict();
    }

    /// Drops least recently used textures until under budget, never one used this frame.
    fn evict(&mut self) {
        while self.bytes > MAX_TEXTURE_BYTES {
            let oldest = self
                .entries
                .iter()
                .filter_map(|(key, entry)| match entry {
                    Entry::Ready { used, .. } if *used != self.clock => Some((*used, *key)),
                    _ => None,
                })
                .min();
            let Some((_, key)) = oldest else {
                return;
            };
            if let Some(Entry::Ready { bytes, .. }) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(bytes);
            }
        }
    }

    /// The preview for asset `hash` at about `edge` pixels. `bytes` reads the asset, and is only
    /// called when a decode is about to start.
    pub fn request(&mut self, ctx: &egui::Context, hash: Blake3, edge: f32, bytes: impl FnOnce() -> Option<Vec<u8>>) -> Preview {
        self.request_input(ctx, hash, edge, || {
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
        self.request_input(ctx, hash, edge, || match source() {
            Ok(input) if input.hash() == hash => Ok(Input::Asset(input)),
            Ok(_) => Err(CaptureError::Failed("Can't preview this image")),
            Err(nodecraft_format::PreviewReadError::Missing) => Err(CaptureError::Missing),
            Err(why) => Err(CaptureError::Failed(read_message(why))),
        })
    }
    fn request_input(&mut self, ctx: &egui::Context, hash: Blake3, edge: f32, source: impl FnOnce() -> Result<Input, CaptureError>) -> Preview {
        let wanted = (hash, bucket(edge));
        let busy = self.busy_this_frame.contains(&Some(wanted));
        let clock = self.clock;
        let mut show = None;
        let mut missing = false;
        match self.entries.get_mut(&wanted) {
            Some(Entry::Ready { texture, size, source_size, used, .. }) => {
                *used = clock;
                return Preview::Ready { texture: texture.id(), size: *size, source_size: *source_size };
            }
            Some(Entry::Failed(why)) => return Preview::Unavailable(why),
            Some(Entry::Pending) => {}
            None if busy => {}
            None => missing = self.start(ctx, wanted, source),
        }
        // While a sharper one decodes, any other size of the same image will do.
        for bucket in BUCKETS.into_iter().rev() {
            if let Some(Entry::Ready { texture, size, source_size, used, .. }) = self.entries.get_mut(&(hash, bucket)) {
                *used = clock;
                show = Some(Preview::Ready { texture: texture.id(), size: *size, source_size: *source_size });
                break;
            }
        }
        match self.entries.get(&wanted) {
            Some(Entry::Failed(why)) => show.unwrap_or(Preview::Unavailable(why)),
            Some(_) => show.unwrap_or(Preview::Pending),
            // Nothing started: the data is missing, or decoding is busy until a later frame.
            None => show.unwrap_or(if missing {
                Preview::Unavailable("This image's data is missing")
            } else if busy {
                Preview::Unavailable("Preview busy; try again")
            } else {
                Preview::Pending
            }),
        }
    }

    /// Starts decoding `key`; `true` when the asset's data is missing.
    fn start(&mut self, ctx: &egui::Context, key: Key, source: impl FnOnce() -> Result<Input, CaptureError>) -> bool {
        if self.in_flight >= MAX_IN_FLIGHT {
            return false;
        }
        let input = match source() {
            Ok(input) => input,
            Err(CaptureError::Missing) => return true,
            Err(CaptureError::Failed(why)) => {
                self.entries.insert(key, Entry::Failed(why));
                return false;
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.worker.is_none() && !self.worker_failed {
                self.worker = worker::Worker::start(ctx.clone());
                self.worker_failed = self.worker.is_none();
            }
            if self.worker.is_some() {
                let submitted = self.worker.as_ref().is_some_and(|w| w.submit(key, input, key.1));
                if submitted {
                    self.in_flight = self.in_flight.saturating_add(1);
                    self.entries.insert(key, Entry::Pending);
                }
                return false;
            }
        }
        // No worker: only immutable resident bytes may decode inline. Lazy content
        // is unsupported here, regardless of how small its descriptor is.
        self.finish(ctx, key, inline_input(input, key.1));
        false
    }

    #[cfg(test)]
    pub(crate) fn test_pending_and_failed(&self) -> (usize, usize) {
        (self.in_flight, self.entries.values().filter(|entry| matches!(entry, Entry::Failed(_))).count())
    }

    /// Decodes inline, for tests: no worker, so results are ready in the same frame.
    #[cfg(test)]
    pub(crate) fn inline(ctx: &egui::Context, hash: Blake3, edge: u32, bytes: &[u8]) -> Self {
        let mut previews = Self::default();
        #[cfg(not(target_arch = "wasm32"))]
        {
            previews.worker_failed = true;
        }
        previews.finish(ctx, (hash, edge), decode(bytes, edge));
        previews
    }
}

#[cfg(test)]
mod tests;

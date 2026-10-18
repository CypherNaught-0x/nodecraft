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

/// Decoding limits for previews: a preview never needs more than a 16k image at 512 MiB.
fn limits() -> Limits {
    Limits { max_alloc: 512 * 1024 * 1024, ..Limits::default() }
}

/// The smallest bucket at least `edge` pixels, or the largest.
pub fn bucket(edge: f32) -> u32 {
    let edge = if edge.is_finite() { edge.max(0.0) } else { 0.0 };
    BUCKETS.into_iter().find(|b| *b as f32 >= edge).or(BUCKETS.last().copied()).unwrap_or(512)
}

type Key = (Blake3, u32);

enum Entry {
    Pending,
    Ready { texture: TextureHandle, size: [u32; 2], bytes: usize, used: u64 },
    Failed(&'static str),
}

/// What a card can show for its image right now.
pub enum Preview {
    Ready { texture: egui::TextureId, size: [u32; 2] },
    Pending,
    Unavailable(&'static str),
}

/// A thumbnail decoded from `bytes` at most `edge` pixels on its longer side.
fn decode(bytes: &[u8], edge: u32) -> Result<ColorImage, &'static str> {
    let (_, image) = decode_image(bytes, &limits()).map_err(|_| "Can't preview this image")?;
    let thumb = thumbnail(&image, edge).map_err(|_| "Can't preview this image")?;
    let size =
        [usize::try_from(thumb.width()).map_err(|_| "Can't preview this image")?, usize::try_from(thumb.height()).map_err(|_| "Can't preview this image")?];
    Ok(ColorImage::from_rgba_unmultiplied(size, thumb.rgba()))
}

#[cfg(not(target_arch = "wasm32"))]
mod worker {
    use super::{Key, decode};
    use egui::ColorImage;
    use std::sync::{
        Arc,
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    };

    pub(super) type Done = (Key, Result<ColorImage, &'static str>);
    /// A decode to do: what for, the encoded bytes and the edge length.
    type Job = (Key, Arc<[u8]>, u32);

    /// One thread that decodes thumbnails; it ends when the cache drops its sender.
    pub(super) struct Worker {
        jobs: Option<SyncSender<Job>>,
        pub(super) done: Receiver<Done>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl Worker {
        pub(super) fn start(ctx: egui::Context) -> Option<Self> {
            let (jobs, queue) = sync_channel::<Job>(super::MAX_IN_FLIGHT);
            let (send, done) = sync_channel::<Done>(super::MAX_IN_FLIGHT.saturating_mul(2));
            let handle = std::thread::Builder::new()
                .name("nodecraft-previews".into())
                .spawn(move || {
                    for (key, bytes, edge) in queue {
                        if send.send((key, decode(&bytes, edge))).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                })
                .ok()?;
            Some(Self { jobs: Some(jobs), done, handle: Some(handle) })
        }

        /// Queues a decode; `false` when the worker is busy (try again next frame) or gone.
        pub(super) fn submit(&self, key: Key, bytes: Arc<[u8]>, edge: u32) -> bool {
            match self.jobs.as_ref().map(|jobs| jobs.try_send((key, bytes, edge))) {
                Some(Ok(())) => true,
                Some(Err(TrySendError::Full(_) | TrySendError::Disconnected(_))) | None => false,
            }
        }
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            // Closing the queue ends the thread after its current decode.
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
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<worker::Worker>,
    #[cfg(not(target_arch = "wasm32"))]
    worker_failed: bool,
}

impl Previews {
    /// Texture bytes held now.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Takes finished decodes into textures; call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.clock = self.clock.wrapping_add(1);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let finished: Vec<worker::Done> = self.worker.as_ref().map(|w| w.done.try_iter().collect()).unwrap_or_default();
            for (key, result) in finished {
                self.in_flight = self.in_flight.saturating_sub(1);
                self.finish(ctx, key, result);
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = ctx;
    }

    fn finish(&mut self, ctx: &egui::Context, key: Key, result: Result<ColorImage, &'static str>) {
        let entry = match result {
            Ok(image) => {
                let size = [u32::try_from(image.size[0]).unwrap_or(u32::MAX), u32::try_from(image.size[1]).unwrap_or(u32::MAX)];
                let bytes = image.size[0].saturating_mul(image.size[1]).saturating_mul(4);
                let texture = ctx.load_texture(format!("preview-{:?}-{}", key.0, key.1), image, TextureOptions::LINEAR);
                self.bytes = self.bytes.saturating_add(bytes);
                Entry::Ready { texture, size, bytes, used: self.clock }
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
        let wanted = (hash, bucket(edge));
        let clock = self.clock;
        let mut show = None;
        match self.entries.get_mut(&wanted) {
            Some(Entry::Ready { texture, size, used, .. }) => {
                *used = clock;
                return Preview::Ready { texture: texture.id(), size: *size };
            }
            Some(Entry::Failed(why)) => return Preview::Unavailable(why),
            Some(Entry::Pending) => {}
            None => self.start(ctx, wanted, bytes),
        }
        // While a sharper one decodes, any other size of the same image will do.
        for bucket in BUCKETS.into_iter().rev() {
            if let Some(Entry::Ready { texture, size, used, .. }) = self.entries.get_mut(&(hash, bucket)) {
                *used = clock;
                show = Some(Preview::Ready { texture: texture.id(), size: *size });
                break;
            }
        }
        match self.entries.get(&wanted) {
            Some(Entry::Failed(why)) => show.unwrap_or(Preview::Unavailable(why)),
            _ => show.unwrap_or(Preview::Pending),
        }
    }

    fn start(&mut self, ctx: &egui::Context, key: Key, bytes: impl FnOnce() -> Option<Vec<u8>>) {
        if self.in_flight >= MAX_IN_FLIGHT {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.worker.is_none() && !self.worker_failed {
                self.worker = worker::Worker::start(ctx.clone());
                self.worker_failed = self.worker.is_none();
            }
            if self.worker.is_some() {
                let Some(data) = bytes() else {
                    self.entries.insert(key, Entry::Failed("This image's data is missing"));
                    return;
                };
                let submitted = self.worker.as_ref().is_some_and(|w| w.submit(key, std::sync::Arc::from(data), key.1));
                if submitted {
                    self.in_flight = self.in_flight.saturating_add(1);
                    self.entries.insert(key, Entry::Pending);
                }
                return;
            }
        }
        // No worker: decode small images now, and say so for larger ones.
        let Some(data) = bytes() else {
            self.entries.insert(key, Entry::Failed("This image's data is missing"));
            return;
        };
        if data.len() > MAX_INLINE_BYTES {
            self.entries.insert(key, Entry::Failed("Preview not available here"));
            return;
        }
        self.finish(ctx, key, decode(&data, key.1));
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

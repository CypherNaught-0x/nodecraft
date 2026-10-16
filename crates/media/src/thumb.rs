//! Thumbnails and an in-memory cache keyed by content hash.

use std::collections::HashMap;
use std::sync::Arc;

use image::DynamicImage;

use crate::{ContentHash, Limits, MediaError, decode_image};

/// A small RGBA8 (straight alpha) preview of an image, ready to upload as a texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major.
    pub rgba: Vec<u8>,
}

impl Thumbnail {
    /// Memory the pixels take, used for the cache budget.
    pub fn byte_size(&self) -> usize {
        self.rgba.len()
    }
}

/// The size of a `width`×`height` image scaled down (never up) so its longer edge is at most
/// `max_edge`, keeping the aspect ratio and at least one pixel per side.
pub fn fit_within(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= max_edge || long == 0 {
        return (width, height);
    }
    let scale = |side: u32| {
        let scaled = (u64::from(side) * u64::from(max_edge) + u64::from(long) / 2) / u64::from(long);
        u32::try_from(scaled).unwrap_or(max_edge).clamp(1, max_edge.max(1))
    };
    (scale(width), scale(height))
}

/// Scales `image` to fit within `max_edge` pixels on its longer side.
pub fn thumbnail(image: &DynamicImage, max_edge: u32) -> Result<Thumbnail, MediaError> {
    if max_edge == 0 {
        return Err(MediaError::InvalidParameter("thumbnail size must be at least 1 pixel"));
    }
    let (width, height) = fit_within(image.width(), image.height(), max_edge);
    let small = if (width, height) == (image.width(), image.height()) { image.to_rgba8() } else { image.thumbnail_exact(width, height).into_rgba8() };
    Ok(Thumbnail { width: small.width(), height: small.height(), rgba: small.into_raw() })
}

struct Entry {
    thumb: Arc<Thumbnail>,
    last_used: u64,
}

/// Thumbnails by content hash and size, evicting the least recently used past a memory budget.
///
/// Not thread-safe on its own: the shell owns one and makes thumbnails off the UI thread.
pub struct ThumbnailCache {
    budget_bytes: usize,
    used_bytes: usize,
    clock: u64,
    entries: HashMap<(ContentHash, u32), Entry>,
}

impl ThumbnailCache {
    /// A cache holding at most `budget_bytes` of thumbnail pixels.
    pub fn new(budget_bytes: usize) -> Self {
        Self { budget_bytes, used_bytes: 0, clock: 0, entries: HashMap::new() }
    }

    /// The cached thumbnail of the media with `hash` at `max_edge`, if present.
    pub fn get(&mut self, hash: &ContentHash, max_edge: u32) -> Option<Arc<Thumbnail>> {
        let now = self.tick();
        let entry = self.entries.get_mut(&(*hash, max_edge))?;
        entry.last_used = now;
        Some(Arc::clone(&entry.thumb))
    }

    /// The cached thumbnail, or the one `make` builds, which is then cached. A thumbnail larger
    /// than the whole budget is returned but not kept.
    pub fn get_or_insert_with(
        &mut self,
        hash: &ContentHash,
        max_edge: u32,
        make: impl FnOnce() -> Result<Thumbnail, MediaError>,
    ) -> Result<Arc<Thumbnail>, MediaError> {
        if let Some(hit) = self.get(hash, max_edge) {
            return Ok(hit);
        }
        let thumb = Arc::new(make()?);
        self.insert(*hash, max_edge, Arc::clone(&thumb));
        Ok(thumb)
    }

    /// The thumbnail of encoded image `bytes`, hashing them and decoding only on a miss.
    pub fn thumbnail_for(&mut self, bytes: &[u8], max_edge: u32, limits: &Limits) -> Result<Arc<Thumbnail>, MediaError> {
        let hash = ContentHash::of(bytes);
        self.get_or_insert_with(&hash, max_edge, || {
            let (_, image) = decode_image(bytes, limits)?;
            thumbnail(&image, max_edge)
        })
    }

    /// Number of cached thumbnails.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes of pixels currently cached.
    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Drops every cached thumbnail.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.used_bytes = 0;
    }

    fn insert(&mut self, hash: ContentHash, max_edge: u32, thumb: Arc<Thumbnail>) {
        let size = thumb.byte_size();
        if size > self.budget_bytes {
            return;
        }
        while self.used_bytes.saturating_add(size) > self.budget_bytes {
            let Some(oldest) = self.entries.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| *k) else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.used_bytes = self.used_bytes.saturating_sub(evicted.thumb.byte_size());
            }
        }
        let last_used = self.tick();
        if let Some(old) = self.entries.insert((hash, max_edge), Entry { thumb, last_used }) {
            self.used_bytes = self.used_bytes.saturating_sub(old.thumb.byte_size());
        }
        self.used_bytes = self.used_bytes.saturating_add(size);
    }

    fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_io::tests::png_rgba;

    #[test]
    fn fit_keeps_aspect_and_never_upscales() {
        assert_eq!(fit_within(1000, 500, 256), (256, 128));
        assert_eq!(fit_within(500, 1000, 256), (128, 256));
        assert_eq!(fit_within(100, 50, 256), (100, 50));
        assert_eq!(fit_within(10_000, 1, 256), (256, 1));
        assert_eq!(fit_within(0, 0, 256), (0, 0));
        assert_eq!(fit_within(u32::MAX, u32::MAX, 64), (64, 64));
        assert_eq!(fit_within(u32::MAX, 3, 1), (1, 1));
    }

    #[test]
    fn thumbnail_pixels_match_size() {
        let (_, img) = decode_image(&png_rgba(300, 150), &Limits::default()).unwrap();
        let t = thumbnail(&img, 64).unwrap();
        assert_eq!((t.width, t.height), (64, 32));
        assert_eq!(t.rgba.len(), 64 * 32 * 4);
        assert!(thumbnail(&img, 0).is_err());
    }

    #[test]
    fn cache_hits_by_content_and_size() {
        let bytes = png_rgba(64, 64);
        let mut cache = ThumbnailCache::new(1 << 20);
        let a = cache.thumbnail_for(&bytes, 16, &Limits::default()).unwrap();
        let b = cache.thumbnail_for(&bytes, 16, &Limits::default()).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "second request is a cache hit");
        let c = cache.thumbnail_for(&bytes, 32, &Limits::default()).unwrap();
        assert_eq!((c.width, c.height), (32, 32));
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.used_bytes(), 16 * 16 * 4 + 32 * 32 * 4);
        let mut made = false;
        let hit = cache
            .get_or_insert_with(&ContentHash::of(&bytes), 16, || {
                made = true;
                Err(MediaError::Unrecognized)
            })
            .unwrap();
        assert!(!made && Arc::ptr_eq(&a, &hit));
    }

    #[test]
    fn cache_evicts_least_recently_used() {
        let thumb = |n: u8| Thumbnail { width: 1, height: 1, rgba: vec![n; 4] };
        let key = |n: u8| ContentHash::of(&[n]);
        let mut cache = ThumbnailCache::new(8);
        cache.get_or_insert_with(&key(1), 1, || Ok(thumb(1))).unwrap();
        cache.get_or_insert_with(&key(2), 1, || Ok(thumb(2))).unwrap();
        cache.get(&key(1), 1).unwrap();
        cache.get_or_insert_with(&key(3), 1, || Ok(thumb(3))).unwrap();
        assert!(cache.get(&key(1), 1).is_some());
        assert!(cache.get(&key(2), 1).is_none(), "2 was least recently used");
        assert!(cache.get(&key(3), 1).is_some());
        assert_eq!(cache.used_bytes(), 8);

        let big = cache.get_or_insert_with(&key(4), 1, || Ok(Thumbnail { width: 3, height: 1, rgba: vec![0; 12] })).unwrap();
        assert_eq!(big.width, 3, "an over-budget thumbnail is still returned");
        assert!(cache.get(&key(4), 1).is_none() && cache.len() == 2);
        cache.clear();
        assert!(cache.is_empty() && cache.used_bytes() == 0);
    }

    #[test]
    fn errors_are_not_cached() {
        let mut cache = ThumbnailCache::new(1 << 20);
        assert!(cache.thumbnail_for(b"\x89PNG\r\n\x1a\nbroken", 16, &Limits::default()).is_err());
        assert!(cache.is_empty());
    }
}

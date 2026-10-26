//! One shared payload allowance for card previews, independent of raster scale.
use super::super::layer_fonts::{self, FontFiles, HeldFont};
use nodecraft_actions::layers::{MAX_FONT_BYTES, MAX_FONTS};
use nodecraft_format::AssetStore;
use nodecraft_types::{AssetRef, Blake3};
use std::{collections::VecDeque, sync::Arc};

const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = super::MAX_CARDS * MAX_FONTS;

struct Entry {
    hash: Blake3,
    bytes: Option<Arc<[u8]>>,
    resident: bool,
    used: u64,
}

pub(super) struct Payloads {
    entries: VecDeque<Entry>,
    bytes: u64,
    clock: Option<u64>,
    read_bytes: u64,
    limit: u64,
    entry_limit: usize,
}

impl Default for Payloads {
    fn default() -> Self {
        Self { entries: VecDeque::new(), bytes: 0, clock: None, read_bytes: 0, limit: MAX_BYTES, entry_limit: MAX_ENTRIES }
    }
}

impl Payloads {
    #[cfg(any(test, not(target_arch = "wasm32")))]
    pub(super) fn forget_missing(&mut self) {
        self.entries.retain(|entry| entry.bytes.is_some());
    }

    pub(super) fn files(&mut self, assets: &AssetStore, held: &[HeldFont], clock: u64) -> FontFiles {
        if self.clock != Some(clock) {
            self.clock = Some(clock);
            self.read_bytes = 0;
        }
        let mut files = FontFiles::default();
        for font in held.iter().take(MAX_FONTS) {
            let bytes = self.payload(assets, &font.font, clock);
            files.bytes.insert(font.font.hash, bytes);
        }
        files
    }

    fn payload(&mut self, assets: &AssetStore, font: &AssetRef, clock: u64) -> Option<Arc<[u8]>> {
        let size = assets.descriptor(&font.hash)?.bytes;
        if size > MAX_FONT_BYTES as u64 || size > self.limit {
            return None;
        }
        let resident = assets.get(&font.hash).is_some();
        if let Some(index) = self.entries.iter().position(|entry| entry.hash == font.hash) {
            let entry = self.entries.get_mut(index)?;
            let gap = entry.used.saturating_add(1) < clock;
            entry.used = clock;
            if entry.bytes.is_some() || entry.resident == resident && !gap {
                return entry.bytes.clone();
            }
            // Retry a missing file on returning to view, or when resident bytes arrive.
            self.entries.remove(index);
        }
        if self.read_bytes.saturating_add(size) > self.limit || !self.make_room(size, clock) {
            return None;
        }
        self.read_bytes = self.read_bytes.saturating_add(size);
        let bytes = layer_fonts::read(assets, font).filter(|bytes| bytes.len() as u64 <= size);
        self.bytes = self.bytes.saturating_add(bytes.as_ref().map_or(0, |bytes| bytes.len() as u64));
        self.entries.push_back(Entry { hash: font.hash, bytes: bytes.clone(), resident, used: clock });
        bytes
    }

    fn make_room(&mut self, needed: u64, clock: u64) -> bool {
        while self.bytes.saturating_add(needed) > self.limit || self.entries.len() >= self.entry_limit {
            // Never cycle through the same visible fonts on every zoom step. The current
            // and previous frame remain resident; overflow draws the normal Lato fallback.
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.used.saturating_add(1) < clock)
                .min_by_key(|(_, entry)| entry.used)
                .map(|(index, _)| index);
            let Some(entry) = oldest.and_then(|index| self.entries.remove(index)) else { return false };
            self.bytes = self.bytes.saturating_sub(entry.bytes.as_ref().map_or(0, |bytes| bytes.len() as u64));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font(assets: &mut AssetStore, suffix: u8) -> HeldFont {
        let mut bytes = include_bytes!("../../../../../assets/fonts/Lato-Regular.ttf").to_vec();
        bytes.push(suffix);
        let hash = assets.insert(&bytes).unwrap();
        HeldFont { name: format!("Public {suffix}"), font: AssetRef { hash, mime: "font/ttf".parse().unwrap(), meta: Default::default() } }
    }

    #[test]
    fn deck695_shared_font_payloads_stay_bounded_without_visible_overflow_churn() {
        let mut assets = AssetStore::default();
        let a = font(&mut assets, 1);
        let b = font(&mut assets, 2);
        let limit = assets.descriptor(&a.font.hash).unwrap().bytes;
        let mut cache = Payloads { limit, ..Default::default() };
        layer_fonts::PAYLOAD_READS.with(|reads| reads.set(0));
        for clock in 1..=12 {
            let files = cache.files(&assets, &[a.clone(), b.clone()], clock);
            assert!(files.readable(std::slice::from_ref(&a), &a.name));
            assert!(!files.readable(std::slice::from_ref(&b), &b.name));
            assert_eq!(cache.bytes, limit);
        }
        assert_eq!(layer_fonts::PAYLOAD_READS.with(std::cell::Cell::get), 1, "overflow must not reread visible fonts on each frame");
        let files = cache.files(&assets, std::slice::from_ref(&b), 15);
        assert!(files.readable(std::slice::from_ref(&b), &b.name));
        assert_eq!(layer_fonts::PAYLOAD_READS.with(std::cell::Cell::get), 2);
        assert_eq!(cache.bytes, limit);
    }

    #[test]
    fn deck695_failed_font_payloads_have_a_metadata_cap_and_do_not_retry_on_zoom() {
        let mut assets = AssetStore::default();
        let mut held = Vec::new();
        for n in 0..4 {
            let hash = assets.insert(&[n; 8]).unwrap();
            held.push(HeldFont { name: format!("Invalid {n}"), font: AssetRef { hash, mime: "font/ttf".parse().unwrap(), meta: Default::default() } });
        }
        let mut cache = Payloads { entry_limit: 2, ..Default::default() };
        layer_fonts::PAYLOAD_READS.with(|reads| reads.set(0));
        for clock in 1..=12 {
            cache.files(&assets, &held, clock);
            assert_eq!(cache.entries.len(), 2);
            assert_eq!(cache.bytes, 0);
        }
        assert_eq!(layer_fonts::PAYLOAD_READS.with(std::cell::Cell::get), 2);
        cache.forget_missing();
        cache.files(&assets, &held, 13);
        assert_eq!(layer_fonts::PAYLOAD_READS.with(std::cell::Cell::get), 4);
    }
}

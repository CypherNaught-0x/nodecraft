//! Rendered media retained for chains, with admission shared by concurrent read-backs.
use nodecraft_types::Blake3;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

// Preserve the existing ceiling for one Split Video operation, but share it across the run.
pub(super) const MAX_STAGED_BYTES: u64 = 4 * super::timeline::MAX_RENDER_BYTES;
pub(super) const MAX_STAGED_RESULTS: usize = 4096;
const CHUNK: u64 = 1024 * 1024;
const CANCELLED: &str = "the render was cancelled";

impl super::LocalActions {
    /// All native read-back adapters enter the same run-owned staging budget here. Keeping
    /// the transport generic also lets ordinary CI prove that wiring without installed tools.
    pub(in crate::action_exec) fn retain_video<C, R, B>(&self, length: u64, read: R, bytes_of: B, cancelled: &dyn Fn() -> bool) -> Result<Blake3, String>
    where
        R: FnMut(u64, usize) -> Result<C, String>,
        B: for<'a> Fn(&'a C) -> &'a [u8],
    {
        read_back(&self.videos, length, read, bytes_of, cancelled)
    }

    /// A split keeps each part provisional until its whole result can be published.
    pub(in crate::action_exec) fn retain_video_pending<C, R, B>(
        &self,
        length: u64,
        read: R,
        bytes_of: B,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Provisional<'_>, String>
    where
        R: FnMut(u64, usize) -> Result<C, String>,
        B: for<'a> Fn(&'a C) -> &'a [u8],
    {
        read_pending(&self.videos, length, read, bytes_of, cancelled)
    }
}

struct Entry {
    bytes: Arc<[u8]>,
    published: bool,
    holds: usize,
}

pub(super) struct Videos {
    entries: BTreeMap<Blake3, Entry>,
    retained: u64,
    reserved: u64,
    pending: usize,
    holds: usize,
    byte_limit: u64,
    count_limit: usize,
}

impl Default for Videos {
    fn default() -> Self {
        Self { entries: BTreeMap::new(), retained: 0, reserved: 0, pending: 0, holds: 0, byte_limit: MAX_STAGED_BYTES, count_limit: MAX_STAGED_RESULTS }
    }
}

impl Videos {
    pub fn iter(&self) -> impl Iterator<Item = (&Blake3, &Arc<[u8]>)> {
        self.entries.iter().filter_map(|(hash, entry)| entry.published.then_some((hash, &entry.bytes)))
    }

    pub fn get(&self, hash: &Blake3) -> Option<&Arc<[u8]>> {
        self.entries.get(hash).filter(|entry| entry.published).map(|entry| &entry.bytes)
    }

    #[cfg(test)]
    pub fn limited(bytes: u64, count: usize) -> Self {
        Self { byte_limit: bytes.min(MAX_STAGED_BYTES), count_limit: count.min(MAX_STAGED_RESULTS), ..Self::default() }
    }

    fn full(&self) -> String {
        format!(
            "this run's rendered media reached its staging limit ({} MiB or {} results); render fewer nodes at a time, split into fewer parts or use a lower render quality",
            self.byte_limit / (1024 * 1024),
            self.count_limit,
        )
    }
}

/// One admitted part, inaccessible to the host or a chain until publication. Dropping its last
/// unpublished hold releases only that entry; another successful consumer marks it published.
pub(super) struct Provisional<'a> {
    videos: &'a Mutex<Videos>,
    hash: Blake3,
    length: u64,
}

impl Provisional<'_> {
    pub fn hash(&self) -> Blake3 {
        self.hash
    }

    pub fn length(&self) -> u64 {
        self.length
    }
}

impl Drop for Provisional<'_> {
    fn drop(&mut self) {
        let mut state = self.videos.lock().unwrap_or_else(PoisonError::into_inner);
        let remove = state.entries.get_mut(&self.hash).is_some_and(|entry| {
            entry.holds = entry.holds.saturating_sub(1);
            !entry.published && entry.holds == 0
        });
        state.holds = state.holds.saturating_sub(1);
        if remove && let Some(entry) = state.entries.remove(&self.hash) {
            state.retained = state.retained.saturating_sub(entry.bytes.len() as u64);
        }
    }
}

/// Publish a complete result under one lock. Validate the whole set before changing visibility;
/// its opaque, non-Clone receipts keep every entry alive throughout publication.
pub(super) fn publish(parts: &[Provisional<'_>]) -> Result<(), String> {
    let Some(first) = parts.first() else { return Ok(()) };
    if parts.iter().any(|part| !std::ptr::eq(part.videos, first.videos)) {
        return Err("the rendered parts belong to different runs".into());
    }
    let mut state = first.videos.lock().unwrap_or_else(PoisonError::into_inner);
    if parts.iter().any(|part| !state.entries.contains_key(&part.hash)) {
        return Err("a rendered part is no longer retained by this run".into());
    }
    for part in parts {
        // The validation above and this mutation share the same lock.
        if let Some(entry) = state.entries.get_mut(&part.hash) {
            entry.published = true;
        }
    }
    Ok(())
}

struct Reservation<'a> {
    videos: &'a Mutex<Videos>,
    bytes: u64,
    active: bool,
}

impl<'a> Reservation<'a> {
    fn new(videos: &'a Mutex<Videos>, bytes: u64) -> Result<Self, String> {
        let mut state = videos.lock().unwrap_or_else(PoisonError::into_inner);
        let total = state.retained.checked_add(state.reserved).and_then(|n| n.checked_add(bytes));
        let count = state.entries.len().checked_add(state.pending);
        let holds = state.holds.checked_add(state.pending);
        if total.is_none_or(|n| n > state.byte_limit) || count.is_none_or(|n| n >= state.count_limit) || holds.is_none_or(|n| n >= state.count_limit) {
            return Err(state.full());
        }
        state.reserved = state.reserved.saturating_add(bytes);
        state.pending = state.pending.saturating_add(1);
        Ok(Self { videos, bytes, active: true })
    }

    fn refund(&self, state: &mut Videos) {
        state.reserved = state.reserved.saturating_sub(self.bytes);
        state.pending = state.pending.saturating_sub(1);
    }

    fn commit(mut self, hash: Blake3, bytes: Arc<[u8]>) -> Result<Provisional<'a>, String> {
        if bytes.len() as u64 != self.bytes {
            return Err("the rendered file changed size while it was read".into());
        }
        let mut state = self.videos.lock().unwrap_or_else(PoisonError::into_inner);
        let holds = state.holds.checked_add(1).ok_or_else(|| state.full())?;
        let entry_holds = state.entries.get(&hash).map_or(0, |entry| entry.holds).checked_add(1).ok_or_else(|| state.full())?;
        // A duplicate releases its provisional reservation without replacing shared storage.
        if let Some(entry) = state.entries.get_mut(&hash) {
            entry.holds = entry_holds;
        } else {
            state.retained = state.retained.saturating_add(self.bytes);
            state.entries.insert(hash, Entry { bytes, published: false, holds: entry_holds });
        }
        state.holds = holds;
        self.refund(&mut state);
        self.active = false;
        Ok(Provisional { videos: self.videos, hash, length: self.bytes })
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.active {
            let mut state = self.videos.lock().unwrap_or_else(PoisonError::into_inner);
            self.refund(&mut state);
        }
    }
}

/// Read-back admission precedes allocation and any chunk request. The accessor borrows the
/// service's accounted chunk, so the native adapter adds no intermediate chunk copy.
pub(super) fn read_back<C, R, B>(videos: &Mutex<Videos>, length: u64, read: R, bytes_of: B, cancelled: &dyn Fn() -> bool) -> Result<Blake3, String>
where
    R: FnMut(u64, usize) -> Result<C, String>,
    B: for<'a> Fn(&'a C) -> &'a [u8],
{
    let part = read_pending(videos, length, read, bytes_of, cancelled)?;
    publish(std::slice::from_ref(&part))?;
    Ok(part.hash())
}

pub(super) fn read_pending<'videos, C, R, B>(
    videos: &'videos Mutex<Videos>,
    length: u64,
    mut read: R,
    bytes_of: B,
    cancelled: &dyn Fn() -> bool,
) -> Result<Provisional<'videos>, String>
where
    R: FnMut(u64, usize) -> Result<C, String>,
    B: for<'a> Fn(&'a C) -> &'a [u8],
{
    if cancelled() {
        return Err(CANCELLED.into());
    }
    if length == 0 {
        return Err("the rendered file is empty".into());
    }
    if length > super::timeline::MAX_RENDER_BYTES {
        return Err(format!("the rendered file is larger than {} MiB; make it shorter or smaller", super::timeline::MAX_RENDER_BYTES / (1024 * 1024),));
    }
    let reservation = Reservation::new(videos, length)?;
    let too_large = |_| "the rendered file is too large".to_owned();
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(usize::try_from(length).map_err(too_large)?).map_err(|_| "the rendered file is too large".to_owned())?;
    while (bytes.len() as u64) < length {
        if cancelled() {
            return Err(CANCELLED.into());
        }
        let requested = usize::try_from(CHUNK.min(length.saturating_sub(bytes.len() as u64))).map_err(too_large)?;
        let chunk = read(bytes.len() as u64, requested)?;
        if cancelled() {
            return Err(CANCELLED.into());
        }
        let chunk = bytes_of(&chunk);
        if chunk.is_empty() {
            return Err("the rendered file ended early".into());
        }
        if chunk.len() > requested {
            return Err("the rendered file changed size while it was read".into());
        }
        bytes.extend_from_slice(chunk);
    }
    if cancelled() {
        return Err(CANCELLED.into());
    }
    let hash = Blake3::from_bytes(*blake3::hash(&bytes).as_bytes());
    if cancelled() {
        return Err(CANCELLED.into());
    }
    let bytes = Arc::from(bytes);
    if cancelled() {
        return Err(CANCELLED.into());
    }
    reservation.commit(hash, bytes)
}

#[cfg(test)]
mod tests;

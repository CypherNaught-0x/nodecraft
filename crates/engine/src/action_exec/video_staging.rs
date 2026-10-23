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
}

pub(super) struct Videos {
    entries: BTreeMap<Blake3, Arc<[u8]>>,
    retained: u64,
    reserved: u64,
    pending: usize,
    byte_limit: u64,
    count_limit: usize,
}

impl Default for Videos {
    fn default() -> Self {
        Self { entries: BTreeMap::new(), retained: 0, reserved: 0, pending: 0, byte_limit: MAX_STAGED_BYTES, count_limit: MAX_STAGED_RESULTS }
    }
}

impl Videos {
    pub fn iter(&self) -> impl Iterator<Item = (&Blake3, &Arc<[u8]>)> {
        self.entries.iter()
    }

    pub fn keys(&self) -> impl Iterator<Item = &Blake3> {
        self.entries.keys()
    }

    pub fn get(&self, hash: &Blake3) -> Option<&Arc<[u8]>> {
        self.entries.get(hash)
    }

    pub fn remove(&mut self, hash: &Blake3) -> Option<Arc<[u8]>> {
        let bytes = self.entries.remove(hash)?;
        self.retained = self.retained.saturating_sub(bytes.len() as u64);
        Some(bytes)
    }

    #[cfg(test)]
    pub fn limited(bytes: u64, count: usize) -> Self {
        Self { byte_limit: bytes.min(MAX_STAGED_BYTES), count_limit: count.min(MAX_STAGED_RESULTS), ..Self::default() }
    }

    fn full(&self) -> String {
        format!(
            "this run's rendered media reached its staging limit ({} MiB or {} results); render fewer nodes at a time or use a lower render quality",
            self.byte_limit / (1024 * 1024),
            self.count_limit,
        )
    }
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
        if total.is_none_or(|n| n > state.byte_limit) || count.is_none_or(|n| n >= state.count_limit) {
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

    fn commit(mut self, hash: Blake3, bytes: Arc<[u8]>) -> Result<(), String> {
        if bytes.len() as u64 != self.bytes {
            return Err("the rendered file changed size while it was read".into());
        }
        let mut state = self.videos.lock().unwrap_or_else(PoisonError::into_inner);
        // A duplicate releases its provisional reservation without replacing shared storage.
        if !state.entries.contains_key(&hash) {
            state.retained = state.retained.saturating_add(self.bytes);
            state.entries.insert(hash, bytes);
        }
        self.refund(&mut state);
        self.active = false;
        Ok(())
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
pub(super) fn read_back<C, R, B>(videos: &Mutex<Videos>, length: u64, mut read: R, bytes_of: B, cancelled: &dyn Fn() -> bool) -> Result<Blake3, String>
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
    reservation.commit(hash, bytes)?;
    Ok(hash)
}

#[cfg(test)]
mod tests;

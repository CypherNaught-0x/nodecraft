use super::{Result, VideoError};
use std::sync::{Arc, Mutex};
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub bytes: usize,
    pub disk: u64,
    pub jobs: usize,
    pub records: usize,
}
pub(super) struct Budget {
    pub used: Mutex<Usage>,
    pub limits: super::VideoLimits,
}
pub(super) struct Lease {
    budget: Arc<Budget>,
    amount: Usage,
}
impl Budget {
    pub fn reserve(self: &Arc<Self>, a: Usage) -> Result<Lease> {
        let mut u = self.used.lock().map_err(|_| VideoError::Closed)?;
        let next = Usage {
            bytes: u.bytes.checked_add(a.bytes).ok_or(VideoError::Limit)?,
            disk: u.disk.checked_add(a.disk).ok_or(VideoError::Limit)?,
            jobs: u.jobs.checked_add(a.jobs).ok_or(VideoError::Limit)?,
            records: u.records.checked_add(a.records).ok_or(VideoError::Limit)?,
        };
        if next.jobs > self.limits.jobs {
            return Err(VideoError::Busy);
        }
        if next.bytes > self.limits.retained || next.disk > self.limits.disk || next.records > self.limits.records {
            return Err(VideoError::Limit);
        }
        *u = next;
        Ok(Lease { budget: Arc::clone(self), amount: a })
    }
}
impl Lease {
    pub fn shrink_disk(&mut self, bytes: u64) -> Result<()> {
        if bytes > self.amount.disk {
            return Err(VideoError::Invalid);
        }
        let mut used = self.budget.used.lock().map_err(|_| VideoError::Closed)?;
        used.disk = used.disk.saturating_sub(self.amount.disk - bytes);
        self.amount.disk = bytes;
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut u) = self.budget.used.lock() {
            u.bytes = u.bytes.saturating_sub(self.amount.bytes);
            u.disk = u.disk.saturating_sub(self.amount.disk);
            u.jobs = u.jobs.saturating_sub(self.amount.jobs);
            u.records = u.records.saturating_sub(self.amount.records)
        }
    }
}

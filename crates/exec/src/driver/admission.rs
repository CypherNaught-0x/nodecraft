//! Pending-command capacity, independent of scheduler storage and outcome delivery.
use super::{Command, JobId};
use std::mem::size_of;
use std::sync::{Arc, Mutex, PoisonError};

/// Driver admission or acknowledgement failure. Errors never contain payload values.
/// `Full` and `PayloadLimit` mean definite rejection. `Closed` may instead mean an accepted
/// command lost its acknowledgement; it does not prove non-acceptance or make retry safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverError {
    Closed,
    Full,
    PayloadLimit,
}
impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Closed => "the run queue has stopped",
            Self::Full => "the run queue command budget is full",
            Self::PayloadLimit => "the run queue command exceeds its resource policy",
        })
    }
}
impl std::error::Error for DriverError {}

/// Lower-only pending-command limits. Zero disables the corresponding pool.
/// Defaults are also hard ceilings. Byte accounting includes allocated payload capacities and
/// conservative per-command overhead, not allocator bookkeeping or the caller's reply futures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionLimits {
    pub data_commands: usize,
    pub data_bytes: usize,
    pub data_payload_bytes: usize,
    pub control_commands: usize,
    pub control_bytes: usize,
    pub control_payload_bytes: usize,
}
impl Default for AdmissionLimits {
    fn default() -> Self {
        Self {
            data_commands: 64,
            data_bytes: 2 * DATA_PAYLOAD_BYTES,
            data_payload_bytes: DATA_PAYLOAD_BYTES,
            control_commands: 16,
            control_bytes: 2 * CONTROL_PAYLOAD_BYTES,
            control_payload_bytes: CONTROL_PAYLOAD_BYTES,
        }
    }
}
impl AdmissionLimits {
    pub(super) fn valid(self) -> bool {
        let max = Self::default();
        self.data_commands <= max.data_commands
            && self.data_bytes <= max.data_bytes
            && self.data_payload_bytes <= max.data_payload_bytes
            && self.control_commands <= max.control_commands
            && self.control_bytes <= max.control_bytes
            && self.control_payload_bytes <= max.control_payload_bytes
            && self.data_payload_bytes <= self.data_bytes
            && self.control_payload_bytes <= self.control_bytes
    }
}

// Fixed command/channel/reply allowance plus one full supported owned batch. These maxima
// are only policies; charges and runtime allocations use actual incoming buffer capacities.
const ENVELOPE_BYTES: usize = size_of::<Command>() + size_of::<Permit>() + 256;
const DATA_PAYLOAD_BYTES: usize =
    ENVELOPE_BYTES + if crate::payload::ADD_BYTES > crate::payload::REQUEUE_BYTES { crate::payload::ADD_BYTES } else { crate::payload::REQUEUE_BYTES };
const CONTROL_PAYLOAD_BYTES: usize = ENVELOPE_BYTES + crate::payload::CANCEL_BYTES;

#[derive(Clone, Copy)]
enum Pool {
    Data,
    Control,
}

#[derive(Default)]
struct Usage {
    commands: usize,
    bytes: usize,
}
#[derive(Default)]
struct Pools {
    data: Usage,
    control: Usage,
}
pub(super) struct Admission {
    limits: AdmissionLimits,
    usage: Mutex<Pools>,
}
impl Admission {
    pub(super) fn new(limits: AdmissionLimits) -> Arc<Self> {
        Arc::new(Self { limits, usage: Mutex::new(Pools::default()) })
    }
    pub(super) fn reserve(self: &Arc<Self>, command: &Command) -> Result<Permit, DriverError> {
        let pool = if matches!(command, Command::Add(..) | Command::Requeue(..)) { Pool::Data } else { Pool::Control };
        let (slots, bytes, payload) = match pool {
            Pool::Data => (self.limits.data_commands, self.limits.data_bytes, self.limits.data_payload_bytes),
            Pool::Control => (self.limits.control_commands, self.limits.control_bytes, self.limits.control_payload_bytes),
        };
        if slots == 0 {
            return Err(DriverError::PayloadLimit);
        }
        let charge = command_bytes(command).ok_or(DriverError::PayloadLimit)?;
        if charge > payload || charge > bytes {
            return Err(DriverError::PayloadLimit);
        }
        {
            let mut usage = self.usage.lock().unwrap_or_else(PoisonError::into_inner);
            let used = match pool {
                Pool::Data => &mut usage.data,
                Pool::Control => &mut usage.control,
            };
            let total = used.bytes.checked_add(charge).ok_or(DriverError::Full)?;
            if used.commands >= slots || total > bytes {
                return Err(DriverError::Full);
            }
            used.commands += 1;
            used.bytes = total;
        }
        Ok(Permit { admission: Arc::clone(self), pool, bytes: charge })
    }
}
pub(super) struct Permit {
    admission: Arc<Admission>,
    pool: Pool,
    bytes: usize,
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut usage = self.admission.usage.lock().unwrap_or_else(PoisonError::into_inner);
        let used = match self.pool {
            Pool::Data => &mut usage.data,
            Pool::Control => &mut usage.control,
        };
        used.commands = used.commands.saturating_sub(1);
        used.bytes = used.bytes.saturating_sub(self.bytes);
    }
}

// Fixed envelope/channel/reply allowance, in addition to the actual owned vector/string
// capacities. Even empty commands consume bytes. No allocator-layout precision is promised.
pub(super) fn command_bytes(command: &Command) -> Option<usize> {
    // Bound raw counts and outer capacity BEFORE any payload walk; rejecting a huge raw list
    // must not first inspect all its nested buffers. No payload is cloned or normalized here.
    match command {
        Command::Add(jobs, _) => {
            if jobs.len() > crate::scheduler::MAX_JOBS {
                return None;
            }
            crate::payload::outer(jobs.capacity()).ok()?;
            crate::payload::add(jobs).ok()?;
        }
        Command::Requeue(jobs, _) => {
            if jobs.len() > crate::scheduler::MAX_JOBS {
                return None;
            }
            crate::payload::outer(jobs.capacity()).ok()?;
            crate::payload::requeue(jobs).ok()?;
        }
        Command::Cancel(super::CancelScope::Jobs(jobs)) => {
            if jobs.len() > crate::scheduler::MAX_JOBS {
                return None;
            }
            crate::payload::outer(jobs.capacity()).ok()?;
        }
        _ => {}
    }
    let mut total = ENVELOPE_BYTES;
    match command {
        Command::Add(jobs, _) => {
            total = total.checked_add(jobs.capacity().checked_mul(size_of::<super::JobSpec>())?)?;
            for job in jobs {
                total = total
                    .checked_add(job.deps.capacity().checked_mul(size_of::<JobId>())?)?
                    .checked_add(job.lane.0.capacity())?
                    .checked_add(job.idempotency_key.capacity())?;
            }
        }
        Command::Requeue(jobs, _) => {
            total = total.checked_add(jobs.capacity().checked_mul(size_of::<(JobId, String)>())?)?;
            for (_, key) in jobs {
                total = total.checked_add(key.capacity())?;
            }
        }
        Command::Cancel(super::CancelScope::Jobs(jobs)) => {
            total = total.checked_add(jobs.capacity().checked_mul(size_of::<JobId>())?)?;
        }
        _ => {}
    }
    Some(total)
}

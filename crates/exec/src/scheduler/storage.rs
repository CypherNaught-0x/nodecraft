//! Job-spec storage bounds, separate from concurrency and pending driver commands.
use super::{JobSpec, MAX_DEPS, SchedulerError};

/// Hard ceilings and defaults for one scheduler's retained job specs. Lower limits may be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageLimits {
    /// Aggregate forward slots; duplicate dependencies count individually.
    pub dependencies: usize,
    /// Aggregate retained UTF-8 lane/key lengths, excluding incoming spare capacity.
    pub metadata_bytes: usize,
    pub lane_bytes: usize,
    pub key_bytes: usize,
}
impl Default for StorageLimits {
    fn default() -> Self {
        Self::HARD
    }
}
impl StorageLimits {
    pub(crate) const HARD: Self = Self { dependencies: 1_000_000, metadata_bytes: 16 * 1024 * 1024, lane_bytes: 2048, key_bytes: 1024 };
    pub(super) fn valid(self) -> bool {
        let max = Self::default();
        self.dependencies <= max.dependencies
            && self.metadata_bytes <= max.metadata_bytes
            && self.lane_bytes <= max.lane_bytes
            && self.key_bytes <= max.key_bytes
    }
    /// Borrow before allocating topology or normalization scratch. Caller capacities are hostile.
    pub(super) fn preflight(self, used: Usage, specs: &[JobSpec]) -> Result<Usage, SchedulerError> {
        let mut retained = used;
        for spec in specs {
            if spec.deps.len() > MAX_DEPS {
                return Err(SchedulerError::TooManyDeps { job: spec.id, limit: MAX_DEPS });
            }
            self.lane(spec.lane.0.len())?;
            self.key(spec.idempotency_key.len())?;
            retained.dependencies = retained.dependencies.checked_add(spec.deps.len()).ok_or(SchedulerError::DependencyBudget)?;
            retained.metadata = retained
                .metadata
                .checked_add(spec.lane.0.len())
                .and_then(|n| n.checked_add(spec.idempotency_key.len()))
                .ok_or(SchedulerError::MetadataBudget)?;
            self.check(retained)?;
        }
        Ok(retained)
    }
    pub(super) fn check(self, used: Usage) -> Result<(), SchedulerError> {
        if used.dependencies > self.dependencies {
            return Err(SchedulerError::DependencyBudget);
        }
        if used.metadata > self.metadata_bytes {
            return Err(SchedulerError::MetadataBudget);
        }
        Ok(())
    }
    pub(super) fn lane(self, bytes: usize) -> Result<(), SchedulerError> {
        if bytes > self.lane_bytes { Err(SchedulerError::LaneBudget) } else { Ok(()) }
    }
    pub(super) fn key(self, bytes: usize) -> Result<(), SchedulerError> {
        if bytes > self.key_bytes { Err(SchedulerError::KeyBudget) } else { Ok(()) }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Usage {
    pub dependencies: usize,
    pub metadata: usize,
}

#[derive(Debug)]
pub(super) struct StoredSpec {
    pub deps: Box<[super::JobId]>,
    pub lane: Box<str>,
    pub retry: super::RetryPolicy,
    pub idempotency_key: Box<str>,
}
impl From<JobSpec> for StoredSpec {
    fn from(spec: JobSpec) -> Self {
        Self {
            deps: spec.deps.into_boxed_slice(),
            lane: spec.lane.0.into_boxed_str(),
            retry: spec.retry,
            idempotency_key: spec.idempotency_key.into_boxed_str(),
        }
    }
}

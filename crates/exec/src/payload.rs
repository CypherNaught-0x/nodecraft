//! Hard incoming allocation policy, separate from logical retained scheduler budgets.
use crate::scheduler::{JobId, JobSpec, MAX_JOBS, SchedulerError, StorageLimits};

/// Normal push-built vectors grow geometrically. Allow finite slack independently of lower
/// retained limits; this is a bound on incoming owned buffers, not process memory.
pub(crate) const CAPACITY_FACTOR: usize = 4;

pub(crate) fn bounded_capacity(capacity: usize, logical_hard_limit: usize) -> Result<(), SchedulerError> {
    if logical_hard_limit.checked_mul(CAPACITY_FACTOR).is_none_or(|limit| capacity > limit) {
        return Err(SchedulerError::InputCapacityBudget);
    }
    Ok(())
}

/// Call before traversing specs or allocating scratch. Length validation stays with the caller
/// so its existing count diagnostic takes precedence.
pub(crate) fn outer(capacity: usize) -> Result<(), SchedulerError> {
    bounded_capacity(capacity, MAX_JOBS)
}

pub(crate) fn add(specs: &[JobSpec]) -> Result<(), SchedulerError> {
    let hard = StorageLimits::HARD;
    let mut dependencies = 0usize;
    let mut metadata = 0usize;
    for spec in specs {
        bounded_capacity(spec.lane.0.capacity(), hard.lane_bytes)?;
        bounded_capacity(spec.idempotency_key.capacity(), hard.key_bytes)?;
        dependencies = dependencies.checked_add(spec.deps.capacity()).ok_or(SchedulerError::InputCapacityBudget)?;
        metadata = metadata
            .checked_add(spec.lane.0.capacity())
            .and_then(|n| n.checked_add(spec.idempotency_key.capacity()))
            .ok_or(SchedulerError::InputCapacityBudget)?;
        bounded_capacity(dependencies, hard.dependencies)?;
        bounded_capacity(metadata, hard.metadata_bytes)?;
    }
    Ok(())
}

/// Worst-case owned buffers under this policy; target layouts, not measured native sizes.
/// These fixed compile-time ceilings fit usize on native and wasm32; no allocation uses them.
pub(crate) const ADD_BYTES: usize = CAPACITY_FACTOR
    * (MAX_JOBS * std::mem::size_of::<JobSpec>() + StorageLimits::HARD.dependencies * std::mem::size_of::<JobId>() + StorageLimits::HARD.metadata_bytes);
pub(crate) const REQUEUE_BYTES: usize = CAPACITY_FACTOR * (MAX_JOBS * std::mem::size_of::<(JobId, String)>() + StorageLimits::HARD.metadata_bytes);
pub(crate) const CANCEL_BYTES: usize = CAPACITY_FACTOR * MAX_JOBS * std::mem::size_of::<JobId>();

/// The owned driver input keeps capacity until consumed. The direct borrowed scheduler API
/// instead charges logical key lengths, since it never retains these caller buffers.
pub(crate) fn requeue(jobs: &[(JobId, String)]) -> Result<(), SchedulerError> {
    let mut metadata = 0usize;
    for (_, key) in jobs {
        bounded_capacity(key.capacity(), StorageLimits::HARD.key_bytes)?;
        metadata = metadata.checked_add(key.capacity()).ok_or(SchedulerError::InputCapacityBudget)?;
        bounded_capacity(metadata, StorageLimits::HARD.metadata_bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_capacity_boundary_and_overflow_are_checked_without_allocating() {
        assert_eq!(bounded_capacity(40, 10), Ok(()));
        assert_eq!(bounded_capacity(41, 10), Err(SchedulerError::InputCapacityBudget));
        assert_eq!(bounded_capacity(usize::MAX, usize::MAX), Err(SchedulerError::InputCapacityBudget));
        assert_eq!(outer(MAX_JOBS * CAPACITY_FACTOR), Ok(()));
        assert_eq!(outer(MAX_JOBS * CAPACITY_FACTOR + 1), Err(SchedulerError::InputCapacityBudget));
    }
}

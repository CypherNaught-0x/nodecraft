# Driver command admission

`exec::driver::Handle` bounds pending commands across all cloned handles. `driver` uses the
hard defaults below; `driver_with_admission` accepts only lower `AdmissionLimits` (zero can
close a pool to admission). This does not bound retained scheduler jobs (#185), event payloads,
runner outcomes, caller allocations before submission, or caller-retained acknowledgement
futures. Outcome notifications remain reliable; dropping them to limit memory could strand jobs. On
common native layouts the default data pool allows approximately 2 × 134 MiB of charged owned
payload capacity, including commands in synchronous consumption. Allocator overhead and temporary
scheduler normalization/storage overlap are outside that figure. Hosts with less memory can use
`driver_with_admission` to lower the limits.

## Acceptance and compatibility

**Calling `add` or `requeue` immediately attempts to enqueue work, before the returned future is
polled. Awaiting only observes the scheduler verdict. Dropping the future is not cancellation.**
This replaces the old lazy `async fn` behavior. There are currently no production handle callers;
all driver test callers retain `.add(...).await` / `.requeue(...).await` nested-result ergonomics.
The outer error changes from `DriverGone` to `DriverError`. `Full` means a nonzero shared pool is temporarily occupied; `PayloadLimit` means a permanent
resource-policy rejection: a disabled pool, excessive raw count or allocated capacity, a charge
exceeding either per-command or aggregate byte ceiling, or invalid constructor limits. Both prove the command was not accepted. `Closed` from an
add/requeue future is ambiguous: the receiver may have been closed before enqueue, or an accepted
command may have lost its verdict after enqueue. Never retry paid work under a fresh key just
because that future returns `Closed`; reconcile its state first. Synchronous controls return
`Closed` only on definite rejection. Errors contain no submitted IDs, keys or lane names. The
inner `SchedulerError` remains the scheduler's validation verdict.

Rejected payloads are dropped synchronously before returning the future. Such a future retains
only its error and a closed reply receiver, never its jobs or a channel sender. An accepted
request future holds only a reply receiver and does not keep the command stream open. Last-handle
shutdown therefore proceeds even if an acknowledgement remains unpolled. Accepted commands remain
accepted when their acknowledgement is abandoned; retrying with fresh paid keys is not safe on
that basis. Use explicit cancellation for accepted work. Controls are synchronous/nonblocking
and return the same explicit errors; a rejected control has not changed scheduler state.

## Limits and ordering

| Pool | Pending commands | Aggregate charged bytes | Per-command charged bytes |
|---|---:|---:|---:|
| Add/requeue | 64 | 2 × data ceiling | data ceiling |
| Cancel/pause/forget | 16 | 2 × control ceiling | control ceiling |

The data ceiling is the greater of the full owned Add and Requeue input bounds from the shared
[scheduler input policy](scheduler-storage.md), plus the envelope allowance below. The Add bound
is `4 × (MAX_JOBS × size_of::<JobSpec>() + 1,000,000 × size_of::<JobId>() + 16 MiB)`;
the Requeue bound is `4 × (MAX_JOBS × size_of::<(JobId, String)>() + 16 MiB)`.
The control ceiling is `4 × MAX_JOBS × size_of::<JobId>()` plus the envelope allowance.
On native targets with 104-byte JobSpec and 8-byte JobId, the Add buffers alone allow
140,708,864 bytes (about 134 MiB); the aggregate data pool allows twice a full maximum command.
Target-specific Rust sizes derive the actual policy, including wasm. These ceilings are never
allocation requests. Actual charges use submitted capacities, so ordinary batches cost much less.

One Handle::add submits the complete batch atomically with one permit and one scheduler verdict;
there is no chunking or prefix that can start paid work before validation. Regressions cover
10,000 jobs with eight inputs, push/collect-built 10,000 with up to 90 inputs, and 100,000
independent jobs. A push-built MAX_JOBS cancel selection fits reserved control admission without
expanding to CancelAll. Lower per-command ceilings must not exceed their aggregate ceiling.
Zero slots permanently disable a pool and return PayloadLimit; retrying cannot free such a pool.

Both pools feed one FIFO, ordered by actual successful enqueue. There are no unsent reservations
waiting for a future's first poll, so an unpolled request cannot leave a FIFO hole. Concurrent
handle calls have no ordering promise before enqueue. Reserved control capacity permits cancellation
when the data pool is full; a full control pool still returns `Full` rather than blocking,
coalescing, reordering or silently losing cancellation. Callers must handle rejection explicitly.

Charging includes actual capacities, even spare capacity: the outer job/requeue vector, dependency
vectors, lane and idempotency-key strings, and cancel-ID vectors. Each envelope also charges its
inline command/permit sizes plus 256 bytes for channel/reply overhead. This is a conservative
logical byte budget, not an exact process-memory or allocator-bookkeeping measurement. No
`shrink_to_fit` guarantee is assumed. Checked arithmetic rejects overflow. Raw Add/Requeue/Cancel counts and outer vector capacity are
checked before any nested-buffer inspection. The shared four-times-hard allocation policy bounds
nested spare capacity separately from retained logical lengths. Cost inspection then borrows the
bounded payloads and performs no clone or normalization before acceptance.

A permit remains charged while queued and through synchronous scheduler consumption, including
reply delivery. It releases exactly once after consumption, a failed send, or receiver destruction.
No admission lock is held across channel send, acknowledgement, callback, payload destruction or
await. A reentrant acknowledgement wake may briefly see the consuming command still charged and
receive `Full`; it must handle that result like any other saturation. Poisoned accounting locks
are recovered without panic. Scheduler storage after consumption has separate ownership/limits.

The existing paused-close cancellation and unpaused-close drain policies remain unchanged.

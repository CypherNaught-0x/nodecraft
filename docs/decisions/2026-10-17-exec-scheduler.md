## 2026-10-17 · exec scheduler (#74)

- **The scheduler is a deterministic state machine** (`exec::scheduler`) with no clock or I/O:
  the driver passes `now_ms` in. That makes limits, retries and cancellation property-testable
  with random DAGs and outcomes.
- **Runtime-agnostic driver** (`exec::driver`): the apps implement `Runtime` (spawn, sleep,
  monotonic clock) with tokio or wasm-bindgen-futures; `exec` depends only on `futures`. Futures
  are `Send` on native and not in the browser (`TaskFuture`, `MaybeSendSync`).
- **One scheduler per session, keys per job.** The planner gives every job an idempotency key
  (`nodecraft:<run uuid>:<job>`), used on every attempt so a provider retry after a lost reply
  never creates or charges for a second generation; limits apply across all runs. Finished jobs
  can be forgotten, failed ones requeued, and the queue paused (the budget gate).
- **No scans over all jobs:** ready jobs are queued per lane and backoffs kept in time order, so
  starting, finishing and waking are cheap with 100k jobs.
- **Backoff** doubles from the job's base delay up to its cap, with deterministic jitter of up to
  −25 % so a failed batch doesn't retry in lockstep.
- **Failure and cancellation propagate downstream as `Skipped`/`Cancelled`**; independent branches
  keep running. A failure reported after a cancel request counts as a cancellation (no retry); work
  that finished despite the request counts as done. A panicking runner fails its job instead of
  stalling the queue.


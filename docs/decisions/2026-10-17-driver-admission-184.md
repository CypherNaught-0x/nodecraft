# 2026-10-17 · Driver admission (#184)

- **Handle request construction enqueues immediately.** The returned future observes only the
  scheduler verdict and retains no sender. Abandoning an acknowledgement never cancels accepted
  work; last-handle shutdown does not wait for acknowledgement owners.
- **Shared data/control capacity feeds one FIFO.** Actual payload capacities and fixed envelope
  overhead are charged before enqueue, with separately reserved control bytes and slots.
  Default byte ceilings derive from a full supported scheduler batch with finite allocation
  slack; one Add is never split. Disabled pools reject permanently; Full means temporary usage.
  Permits cover queued commands and synchronous consumption; no accounting lock surrounds
  sends, payload drops or wakes. See `driver-admission.md` for ceilings and caller migration.
- **This bounds pending commands only.** Scheduler aggregate storage and outcome-string budgets
  remain separate. Outcome notifications cannot be dropped to enforce command admission.

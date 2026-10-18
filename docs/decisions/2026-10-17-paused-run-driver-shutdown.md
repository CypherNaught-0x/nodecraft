## 2026-10-17 · Paused run-driver shutdown (#150)

- Dropping the last run-queue handle drains an unpaused queue normally, including dependencies
  and retries. Closing a paused queue cancels all unfinished jobs instead of resuming potentially
  paid work: queued and retry-waiting jobs end immediately, and running jobs receive their
  cooperative cancellation token. The driver waits for their reports and delivers terminal
  events before returning the scheduler; as with explicit cancellation, successful completion
  despite cancellation still counts as done. Runners must cooperate for shutdown to complete.
- The driver stops polling the command stream after observing closure; only job outcomes and
  retry timers can then make progress.


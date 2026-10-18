## 2026-10-18 · The accepted amounts and unknown-price job count are a cumulative per-run ceiling (#366)

- **What the caller accepts.** `run.start`'s `accept.amounts` are, per currency, the most the
  run may reserve; `accept.unpriced_jobs` is how many jobs of unknown price it may admit. The
  quote prices a deferred step (job count known only at run time) as one job and can't foresee
  retries, so neither can stay a one-time check: the authorized run keeps both as its ceiling.
  A currency with no amount has a ceiling of zero, a missing count is zero, and neither ever
  means unlimited. An invalid currency code is refused rather than ignored. When the quote has
  unknown-price steps, `run.start` requires `partial: true` and a count covering the quote's
  jobs of those steps (a deferred step counts as one).
- **Authority.** The ceiling is created only in `authorize`, from the prepared run's own
  acceptance (already bound to the quote token, the live execution, catalog and routing
  witness), and lives in the `AuthorizedRun`. `queued_job` binds each job's admitted price, with
  the ceiling, into a private field of `QueuedJob`; budgets use that bound price whatever the
  public `price` is changed to, replacements clone it, and a runner with no budget refuses the
  run's paid jobs before the provider. Nothing outside the engine can create, attach, widen or
  refresh a ceiling. `QueuedJob::new` and `JobPrice::new` remain for work outside an
  authorized run, which only the budgets bound: that is the trusted host's statement, as before.
- **Priced reservations.** Every new priced reservation (first attempts, deferred 1→many
  expansion, concurrent jobs, retries and requeues under a new key, replacements on another
  endpoint) is debited at its estimate inside `ProviderRunner`'s reservation, under the usage
  log's lock, after the per-call/session/project checks. One that would take the run's reserved
  total past the accepted amount is refused with a budget refusal before the provider is
  invoked. A retry under the same key reuses its reservation and is not debited again.
- **Unknown-price admissions.** Every new unknown-price admission (a provider and key the run
  hasn't admitted: deferred expansion, concurrent jobs, new-key retries, requeues and
  replacements) uses one of the accepted count, under the same lock; past it, the job is refused
  before the provider. A same-key retry uses nothing more. A refusal before admission (a budget
  cap, the count itself) uses nothing. Once admitted, a job's count is never given back: without
  a price a release proves nothing worth refunding. The count is not a price; budgets apply on
  their own, and any cap still refuses unknown-price work.
- **Release.** A priced reservation released on evidence that nothing reached the provider
  (refused before invocation, cancelled before submission, an audited local failure) is credited
  back once, and only by the reservation that took the debit: a job that finds its key already
  reserved by another never credits it. One that may have been billed (unresolved) or completed
  keeps its debit, even if its key is reused.
- **Adopted work.** A job adopted after it was already submitted is debited or counted even past
  the ceiling, never refused, and never refunded: failing to record its charge in the usage log
  is not evidence that nothing was delivered.
- **Estimates, not bills.** The debit stays at the estimate when the charge settles; a cheaper
  actual never frees room. A dearer actual is recorded in the usage log as billed; the ceiling
  bounds new admission and claims nothing about provider pricing, refunds or cancellation.
- **Crash and reload.** The ceiling is in memory with the authorized run and isn't saved. A
  reloaded session has no authorized run; continuing needs a new quote and acceptance. Restart
  deduplication stays with the usage log's idempotency keys.
- **Bounds.** A run records at most 100,000 priced debits and 100,000 unknown-price admissions by
  provider and key (a run has at most 100,000 jobs). Settled entries stay recorded, so past that
  new work is refused and needs a new quote; adopted work past it is still accounted, in an
  aggregate that is never refunded.

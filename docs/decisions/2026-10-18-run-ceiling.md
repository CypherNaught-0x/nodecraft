## 2026-10-18 · The accepted amounts are a cumulative per-run ceiling (#366)

- **What the amounts mean.** `run.start`'s `accept.amounts` are, per currency, the most the
  caller agreed the run may reserve. The quote prices a deferred step (job count known only at
  run time) as one job and can't foresee retries, so the amounts can't stay a one-time check:
  the authorized run keeps them as a ceiling. A currency with no amount has a ceiling of zero;
  an absent amount never authorizes spending, and an invalid currency code is refused rather
  than ignored.
- **Authority.** The ceiling is created only in `authorize`, from the prepared run's own
  acceptance (already bound to the quote token, the live execution, catalog and routing
  witness), and lives in the `AuthorizedRun`. `queued_job` attaches it to each priced
  `JobPrice`; replacements clone the job and so share it. Nothing outside the engine can create,
  widen or refresh one, and host JSON or a copied `Quote` can't stand in for it.
- **Reservation.** Every new priced reservation of the run (first attempts, deferred 1→many
  expansion, concurrent jobs, retries and requeues under a new key, replacements on another
  endpoint) is debited at its estimate inside `ProviderRunner`'s reservation, under the usage
  log's lock, after the per-call/session/project checks. One that would take the run's reserved
  total past the accepted amount is refused with a budget refusal before the provider is
  invoked. A retry under the same key reuses its reservation and is not debited again.
- **Release.** A reservation released on evidence that nothing reached the provider (refused
  before invocation, cancelled before submission, a definite no-delivery failure) is credited
  back once. One that may have been billed (unresolved) or completed keeps its debit, even if
  its key is reused: credits are keyed by the run's own debit for that provider and key.
- **Estimates, not bills.** The debit stays at the estimate when the charge settles; a cheaper
  actual never frees room. A dearer actual is recorded in the usage log as billed; the ceiling
  bounds new admission and claims nothing about provider pricing, refunds or cancellation.
- **Adopted work.** A job adopted after it already ran is debited even past the ceiling, never
  refused: the work happened, and the ledger should say so.
- **Unknown prices.** Jobs whose price is unknown are never debited as if free. They come only
  from steps the quote listed as unpriced, which `run.start` admits only with explicit
  `partial: true`, and any budget cap still refuses them at job time. Bounding them needs a
  price, so continuing past that is a new quote and acceptance.
- **Crash and reload.** The ceiling is in memory with the authorized run and isn't saved. A
  reloaded session has no authorized run; continuing needs a new quote and acceptance. Restart
  deduplication stays with the usage log's idempotency keys.
- **Bounds.** One ceiling tracks at most 100,000 open reservations (the run job limit); past
  that, new reservations are refused until some finish.

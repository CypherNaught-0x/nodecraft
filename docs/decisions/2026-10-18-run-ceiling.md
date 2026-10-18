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
  `JobPrice`; replacements clone the job and so share it. The field is private to the engine, so
  no caller can attach another run's ceiling or widen one, and host JSON or a copied `Quote`
  can't stand in for it. `JobPrice::new` builds a price for work outside an authorized run,
  which only the budgets bound: like the estimate itself, that is the trusted host's statement,
  the same boundary `ProviderRunner::register` already had.
- **Reservation.** Every new priced reservation of the run (first attempts, deferred 1→many
  expansion, concurrent jobs, retries and requeues under a new key, replacements on another
  endpoint) is debited at its estimate inside `ProviderRunner`'s reservation, under the usage
  log's lock, after the per-call/session/project checks. One that would take the run's reserved
  total past the accepted amount is refused with a budget refusal before the provider is
  invoked. A retry under the same key reuses its reservation and is not debited again.
- **Release.** A reservation released on evidence that nothing reached the provider (refused
  before invocation, cancelled before submission, a definite no-delivery failure) is credited
  back once, and only by the reservation that took the debit: a job that finds its key already
  reserved by another never credits it. One that may have been billed (unresolved) or completed
  keeps its debit, even if its key is reused.
- **Estimates, not bills.** The debit stays at the estimate when the charge settles; a cheaper
  actual never frees room. A dearer actual is recorded in the usage log as billed; the ceiling
  bounds new admission and claims nothing about provider pricing, refunds or cancellation.
- **Adopted work.** A job adopted after it already ran is debited even past the ceiling, never
  refused: the work happened, and the ledger should say so.
- **Unknown prices.** Jobs whose price is unknown are never debited as if free. They come only
  from steps the quote listed as unpriced, which `run.start` admits only with explicit
  `partial: true`, and any budget cap still refuses them at job time. Without caps, such a
  step's deferred fan-out is not bounded by the ceiling: the person consented to an unknown
  price, and bounding it needs a price, so a cap or a priced model is the way to bound it.
- **Crash and reload.** The ceiling is in memory with the authorized run and isn't saved. A
  reloaded session has no authorized run; continuing needs a new quote and acceptance. Restart
  deduplication stays with the usage log's idempotency keys.
- **Bounds.** One ceiling records at most 100,000 debits (a run has at most 100,000 jobs);
  settled debits stay recorded, so past that a run refuses new reservations and needs a new
  quote. Adopted work is still debited.

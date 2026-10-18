## 2026-10-17 · Budgets at job time (#207)

- **Reserve before submit.** `engine::jobs::ProviderRunner::with_budget` checks each priced job
  against the caps with `cost::check`. The current spend counts every reservation still open, so
  jobs running in parallel can't together overshoot a cap. The job is then recorded in the usage
  log as an `estimate`. A refusal is a non-transient failure starting with `budget: `
  (`is_budget_refusal`), so the scheduler never retries it, and it never reaches the provider.
- **Adopt what already ran.** A job resumed from a stored handle is recorded without a cap check:
  the spend already happened, and refusing would hide it.
- **Settle by what may have been billed.**
  - A finished job settles to the provider's reported cost in the estimate's currency, otherwise
    to the estimate.
  - A job the provider certainly never billed (refused at submit: a bad key, invalid input,
    cancelled before start) is released.
  - A job that got a handle, or whose submit was lost to a timeout, network or server error,
    keeps its reservation while a retry can come. When it is forgotten, settled, or requeued
    under a fresh key, it is charged its estimate as `unresolved`.
- **The host closes what the scheduler gave up on.** The driver doesn't report to the runner,
  so hosts call `ProviderRunner::settle` on terminal failures and cancellations, and `forget` when
  dropping finished jobs (see the method docs). A settle or forget that arrives while the job is
  running waits until its run ends. Estimates left open without a stored handle are settled as
  unresolved when a project is reopened.
- **A generation the provider reports as failed is assumed unbilled.** It keeps one reservation
  under its key, and a retry settles it once. Most providers don't bill failures; a provider that
  does would be undercounted by one estimate per failed attempt.
- **Free and local work isn't charged**: jobs without a `JobPrice` skip the budget. A job with an
  unknown price runs uncharged without caps and is refused under any cap.


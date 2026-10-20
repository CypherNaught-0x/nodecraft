## 2026-10-20 · A batch keeps the items that finished, and running again completes it (#480)

- **Partial results.** When some items of a fanned step fail or are cancelled and others finish,
  the step records one generation with every finished item and an empty list in each missing
  item's place, and marks it `incomplete` with the missing items and why (failed or cancelled).
  The step still reports a failure ("N of M items didn't finish: …", or Cancelled when every
  missing item was cancelled), so the card shows both. When no item finishes, the step fails as
  before and records nothing.
- **Never mistaken for a complete result.** A partial result is always stale, nothing downstream
  reads it (dependents in the run fail with "didn't produce a result for every item"; later runs
  plan its node again), the cache never reuses it, and format v1 refuses to write it. The
  `incomplete` field is omitted when absent, so complete results and existing files are
  unchanged.
- **Running again runs only the missing items.** If the node's active result is partial and it
  was made from the inputs the node has now (same input hash, same items, same output layout),
  the planner keeps the finished items and plans jobs only for the missing ones; the quote
  counts only those. The finished items are copied, with their job settings, into the completed
  result, which keeps every item's id. Anything that doesn't match exactly runs in full. Only
  steps whose jobs are known up front qualify: a step fed by this run can't know its inputs are
  unchanged until they are made.
- **Missing items get fresh idempotency keys.** Reusing an item's old keys looks like the way to
  avoid a double charge, but what a provider holds under them is the outcome we are retrying: a
  failed job answers with its failure and a provider-side cancel with `Cancelled`, so the retry
  could never succeed. Not resubmitting the finished items is what saves the money. An item whose
  job may have been billed also runs again under a new key, so it can be charged twice: one
  whose reply was lost (an uncertain failure after retries), and one the provider finished whose
  outputs then failed here (a download or validation error). Recovering those under their old
  keys is #517.
- **Inputs made in the same run run in full.** A step resumes only when its inputs are known at
  planning time. Re-running an upstream node together with the partial one runs every item, even
  when the upstream turns out unchanged; comparing hashes once the inputs are made is tracked as
  #518.
- **A re-run that is itself cut short replaces the active result** with its own partial, which
  still carries the items kept from before, so nothing already paid for is lost.
- **Order in the run host.** A step's finished result is recorded before its failure is reported,
  so the card ends showing the failure badge over the partial result rather than clearing it.

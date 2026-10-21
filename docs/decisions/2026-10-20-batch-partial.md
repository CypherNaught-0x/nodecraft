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
  result, which keeps every item's id. Anything that doesn't match exactly runs in full. A step
  fed by this run is decided once its inputs are made (see below).
- **A missing item runs under a fresh key unless it may have been billed** (#517, revised
  2026-10-21). What a provider holds under an item's old keys is the outcome being retried: a job
  it reported failed answers with that failure, a provider-side cancel with `Cancelled`. So those
  items, and cancelled ones, run under new keys; not resubmitting the finished items is what
  saves the money. At a provider whose policy allows sending a job again under its key (ComfyUI,
  the mock: those with `retry_ambiguous_submit`), two kinds of failure may have been billed
  without the provider saying the job failed: a reply lost on submit (an uncertain failure) and a
  job the provider accepted whose download or output check failed. Their missing item keeps each
  such job's exact sent key (`keys`, a list of `{job, key}`, omitted when empty), and running
  again sends that job under it, at most once per run, so the provider answers with the job it
  ran and the usage log keeps one charge. The provider's handle is kept for the session only (at
  most 256), so the same session polls it instead of submitting; after a restart the job is
  submitted under its key.
  - **Not for providers that ignore keys** (fal, Replicate, Meshy, Tripo, Seedream, OpenAI and
    Gemini images): sending the job under its old key would bill it again while the usage log
    settled the old charge, under-reporting spend and skipping the budget check. Their items run
    under fresh keys, with their own charges.
  - **A failure that repeats is followed once.** A job sent again under a kept key that fails the
    same way isn't kept again, so the next run sends it fresh (an output too large or gone).
  - **Deadlines and cancels run fresh:** the provider was asked to stop the job.
  - **Only provider-side failures.** A job that finished but whose outputs can't be converted or
    fit here is held as a paid result instead (#369), and technique nodes' inlined steps always
    run under fresh keys: the technique node has no jobs of its own to key them.
  - A recovered item's recorded settings and seed are the new run's. Older builds ignore the field
    and use fresh keys, so the format version is unchanged. Keys are validated on load: only a
    failed item's, in job order, each a visible-ASCII `nodecraft:` key of at most 64 bytes.
- **Inputs made in the same run are compared once they are made** (#518, revised 2026-10-21). A
  fanned step fed by this run is hashed when its upstream has finished, and when the hash is the
  one its partial result was made from, only the missing items run, as above. The comparison is of
  the whole input hash: one changed item runs every item. Its quote, made before, counts every
  item, so for these steps it is an upper bound; `run.quote` lists them as `resumable`, and the
  run dialog and the CLI preview say so. Technique nodes and their inlined steps keep #536's rule.
- **A re-run that is itself cut short replaces the active result** with its own partial, which
  still carries the items kept from before, so nothing already paid for is lost.
- **Order in the run host.** A step's finished result is recorded before its failure is reported,
  so the card ends showing the failure badge over the partial result rather than clearing it.

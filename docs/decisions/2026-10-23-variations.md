## 2026-10-23 · Variations are separate jobs, recorded as separate results (#81)

- **The count is per node**: `config["nodecraft.variations"]`, 1 to 20, set with
  `node.variations_set` (one, or `null`, stores nothing). Like the cache switch it lives in
  `config`, not the parameters, so it never reaches a provider and is left out of the input hash:
  changing it never makes a result stale. Only generation nodes not inlined from a Technique use
  it; every other kind runs once.
- **Each variation is its own jobs.** The planner repeats an eager step's jobs N times; a step
  whose inputs come from the same run repeats them once they are made. Each job keeps its own
  idempotency key and draws its own seeds, so variations differ (a fixed seed is sent unchanged
  to all of them) and a retry never charges for a sibling. The quote counts every variation.
- **Each variation is a result of its own.** The step records one generation per variation; the
  first keeps the id a single result would have, the rest are derived from it. The last becomes
  the active result, and what comes after the node runs once, on it. A variation fails alone
  (its sibling jobs are cancelled); the step reports "k of n variations didn't finish" and still
  counts as done unless every variation failed.
- **A fixed seed is sent as set.** A seed left random is drawn per variation; one the person
  pinned goes unchanged to every variation, so a model that is deterministic for a seed returns
  the same image N times, at N times the price the quote shows. That is what was asked for, so it
  isn't refused; warning about it on the card is follow-up work.
- **What each record takes.** The run host records each variation with only its own jobs'
  paid results, so a refused record holds just that variation's; a variation lost while
  assembling lets its results go to be held, never dropped silently. After a restart each
  single-job variation is recorded as it lands, so the active one is whichever landed last.
- **Not with a batch.** A node fanned over a list already makes one result per item; asking it
  for variations too blocks the run with the reason, rather than multiplying silently.
- **History first.** Variations go to the node's history. Placing them as new cards beside the
  node, the plan's other preference, builds on this and is tracked separately.

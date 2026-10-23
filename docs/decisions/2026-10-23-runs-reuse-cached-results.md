## 2026-10-23 · Runs reuse cached results before quoting or paying (#91)

Completes [result caching](2026-10-19-result-caching.md): `run.quote` and `run.start` apply cache
hits themselves, so a run never quotes, admits or pays for a node whose identical result is
already in the project.

- **Plan, look, plan again.** The request is planned as before; the planned steps on the real
  canvas with caching on are looked up with the same hits as `run.reuse`. When some reuse, the
  run is planned again on a copy of the project with those results in place, the reused nodes
  left out (`planner::plan_settled`). The quote lists them in `reused` and prices only what is
  left.
- **Only when nothing upstream runs.** A node reuses only when every step it comes after in this
  run reuses too. A node fed by a step that runs would read a different input once that step
  finished, so its cached result would already be stale.
- **One undo step, then the run.** `run.start` applies the reused results to the document as
  their own undo step, rebinds the prepared run to the document they made (the one it was
  planned on), and only then hands the run to the host; its result adds `reused`. A run whose
  every planned node reuses applies the results and goes to no host. An edit after that still
  makes the prepared run stale, as before. If the host refuses the run, the reused results stay
  applied: they are free and undo on their own.
- **Already current counts.** A node with caching on whose active result already matches its
  inputs (or does once the results upstream of it are reused) doesn't run either: asking again
  for an identical result with caching on reuses the one there. It is listed in `reused` with
  nothing to apply; with nothing else to do, `run.start` changes nothing.
- **Within one undo step.** The reused results together stay within the history's replay budget;
  past it, the latest hits in dependency order run instead, so a quote that allows a run never
  promises an edit the start can't make.
- **Never over a waiting run.** A run the host has accepted but not yet started is bound to the
  document as it is. A start that would apply reused results meanwhile is refused as busy
  (`Services::register_run_host_idle`), rather than leaving that run to be refused.
- **Cost.** A plan with candidates hashes their inputs once per quote and start. When some
  reuse, it is planned twice and the project is copied once (the look's own copy, unless the
  budget or the dependency rule left some hits out). A blocked first plan isn't retried with
  reuse.
- **Not here.** `run.estimate` and live recompute keep their own paths (live recompute already
  reuses first). Nodes inlined from a technique never reuse: they aren't on the document.
  Technique nodes don't reuse in a run either: they run their version's nodes, which may call a
  model, and live recompute reuses only those made of local work.

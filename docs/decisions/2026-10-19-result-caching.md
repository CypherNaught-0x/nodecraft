## 2026-10-19 · Results are reused by input hash, per node, off for models (#91)

- **The key is the input hash** (#75): kind, settings and input values. A node about to compute
  reuses a result with the same hash, first from its own history (a setting changed back
  selects its earlier result), else from another node of the same kind on the canvas (copied as
  a new result with a fresh id). Nothing outside the project is consulted, and nothing is ever
  paid for or run to fill the cache.
- **The switch is per node**: `config["cache"]`, set with `node.cache_set` (`null` restores the
  default). Without it, local kinds (actions) reuse and kinds that call a model don't, since the
  same prompt may be run again for another take. The switch is left out of the input hash, so
  flipping it never makes a result stale.
- **Where it applies.** Live recompute reuses before it computes (the reuse joins the edit's
  undo step, like a computed result). `run.reuse {canvas, nodes?}` applies hits as one undo step,
  for hosts to call before quoting a run; making `run.quote`/`run.start` skip cache hits
  themselves comes with the app's run host (#301).

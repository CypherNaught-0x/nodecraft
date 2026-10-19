## 2026-10-17 · 200-node performance (#65)

- **Undo keeps up to 4,096 steps** (was 256), subject to the 16 MiB history byte budget.
  The keyboard regression proves a 200-node graph built with 599 create/edit/move commands
  undoes to its original document; this does not guarantee arbitrary 1,000-node histories fit.
- **The view's schema cache accounts incrementally:** a running byte total and an index of
  shared descriptors, updated on insert and remove, avoid rescanning the cache to account each
  visible card. Schema validation still depends on descriptor size, and memory-pressure eviction
  scans entries for each removal. Tests recompute the total to catch drift.
- **The benchmark lives with the canvas tests** (`app/perf.rs`, ignored by default) and reports
  mean, p95 and worst frame; re-run it when cards or noodles get richer.

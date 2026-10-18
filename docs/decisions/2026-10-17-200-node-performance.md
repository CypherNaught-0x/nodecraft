## 2026-10-17 · 200-node performance (#65)

- **Undo keeps up to 4,096 steps** (was 256), so a 1,000-node graph built step by step (about
  3,000 commands) can be undone to empty; the 16 MiB history byte budget is what bounds memory.
- **The view's schema cache accounts incrementally:** a running byte total and an index of
  shared descriptors, updated on insert and remove, so the frame after an edit (which
  revalidates every visible card) stays linear. Tests recompute the total to catch drift.
- **The benchmark lives with the canvas tests** (`app/perf.rs`, ignored by default) and reports
  mean, p95 and worst frame; re-run it when cards or noodles get richer.


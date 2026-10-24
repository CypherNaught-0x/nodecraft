## 2026-10-20 · Random seeds are drawn by the run after admission (#162)

- **After admission, before submission.** Catalog admission keeps, per admitted submission, the
  seeds its contract defaults to `random_seed` with each one's `max`. When a step's jobs are
  handed out, each job that matches that submission gets one seed per such parameter, charged to
  the step's byte budget. Admission's exact-match rule is unchanged: the seed is never part of
  what the source submitted.
- **Derived, not sampled.** A seed is `blake3("nodecraft.run.seed" ‖ run id ‖ node ‖ job index
  within its step ‖ name)`, reduced into `0..=max`. Run ids are random per run
  (`RunOptions::id`), so seeds differ per run (a reused id repeats them), while a retry of one
  job sends the same seed and stays idempotent, and no entropy source is needed in `exec`
  (which must build for wasm).
- **Recorded, not identity.** The drawn seed is in the result's job settings (#162), so the
  result can be repeated with a fixed seed. Identity keeps only the `random_seed` marker:
  drawing again never makes a node stale, and an opted-in cache reuses whatever seed its
  result was made with. When settings are too large to record, inputs are shed first so the
  seed survives; settings that still do not fit are dropped whole.
- **Only where something draws.** `into_identity` (the catalog generation kind) admits a ready
  random seed; `into_resolved` (descriptor kinds, whose jobs no catalog witness seeds) still
  refuses it as unsupported. The generation kind no longer refuses random-seed models.
- **Supersedes** the random-seed bullet of `2026-10-19-contract-run-path.md`, which refused
  `random_seed` realization until a run could draw and record it.

## 2026-10-19 · Live recompute joins the edit's undo step (#88)

- **Results are derived, not edits.** When an edit leaves action nodes stale (or without a
  result), the app plans them with the upstream they need and runs the plan on a worker thread
  (`engine::live_actions`). Only local work runs live: a plan with a model call is left for an
  explicit, priced run.
- **One undo step.** `Session::record_derived` records the results as part of the undo entry of
  the edit that made them stale: the combined inverse first removes the results, then reverts the
  edit. One undo restores the old value *and* its old result, so undo never leaves a node stale
  and live recompute never fights it. Without an edit below (a fresh document), the results get
  their own entry.
- **Never out of date.** Results carry the revision they were computed from and are dropped if the
  document moved on, or while there is something to redo; the next round starts from the current
  document. At most one recompute runs at a time, so a dragging slider is followed as fast as the
  work allows without queueing.
- **Failures don't loop.** A node whose action failed is reported once (a notice) and not retried
  until the document changes.
- **Not yet:** reduced-resolution previews while a slider drags (every round is full
  resolution), and the browser build (no worker threads; live recompute is native-only).

## 2026-11-06 · The browser takes live rounds a few jobs per frame (#1072)

[`2026-10-22-drag-previews.md`](2026-10-22-drag-previews.md) ran a whole live round on the UI
thread in the browser, when it started, and said a worker would replace that later. This is the
replacement, chosen over a WebAssembly worker.

- **Steps, not a worker.** `engine::live_actions::LiveRound` is a round taken one job at a time;
  `compute`, which the desktop's worker thread runs, now loops it to the end, so both make the
  same results. In the browser the app keeps the round and takes jobs from it each frame for up
  to 8 ms (at least one), then draws. A wasm Worker would need its own copy of the session's
  assets and kinds across a message boundary, a second wasm instance and a build of its own; the
  rounds are already split into jobs, so stepping them gets most of the benefit for none of that.
- **The same rules.** The round's outcome goes into the same channel a worker's would, so the
  same checks apply: a round made out of date by an edit, a canvas switch or another document is
  cancelled, ends at its next step and records nothing; a round waits while a background save
  writes.
- **One job is never split.** An action on a large image runs whole in the step that takes it, so
  it still holds that one frame, bounded by the live work budget (`LIVE_WORK_UNITS`). Rounds of
  several jobs, such as a chain of actions or a technique, no longer hold the frame for all of
  them. Splitting a single action would need the actions themselves to yield.

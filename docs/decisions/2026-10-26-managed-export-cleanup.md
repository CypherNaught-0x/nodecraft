## 2026-10-26 · Account for export scratch and wait for cleanup before quitting (#587)

- Background result verification creates a managed sibling of the session's native asset
  workspace, under its configured storage parent. It does not allocate an unmanaged system
  temporary directory. Normal ownership removes scratch after its last reader; the existing
  bounded startup maintenance can recognize and reclaim scratch abandoned by a crash.
- Scratch uses the session's disk accounting, including lowered byte limits, backing counts,
  reservations and retained cleanup charges. Each quota has one additional export transfer
  gate, shared by every export scope. A live transfer and an export can proceed independently
  while their combined allocations obey the same limits. The existing process-wide export
  lease still admits only one worker. Neither the live asset operation gate nor its transfer
  gate is held across the export's first source read, verification or destination write.
- An export watch observes the exact retained receipt without granting cancellation or
  exposing a path. It survives replacement of its owning Session. A terminal receipt follows
  release of the verifier and destination writer, so the host can observe cleanup rather
  than guessing that a missing Session means completion.
- Native close decisions run in `eframe::App::logic`, before `craft-quit`, including when the
  window is minimized. An active export cancels the close and restores the window to offer
  **Keep exporting** or **Cancel export and quit**. The latter dispatches `export.cancel`,
  drops remaining numbered items and waits asynchronously for the retained receipt before
  requesting close again. Already completed files are kept. Atomic finishing cannot be
  cancelled; it must finish before closing. A blocked filesystem call keeps the window open
  and responsive, with **Keep app open**, instead of a timeout silently abandoning cleanup.
- This decision concerns background result exports. Timeline interchange and the headless
  `export.local_run` command retain their existing synchronous behavior.

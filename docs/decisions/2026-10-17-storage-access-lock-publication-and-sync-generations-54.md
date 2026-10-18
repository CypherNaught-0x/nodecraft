# 2026-10-17 · Storage access, lock publication and sync generations (#54)

- A staged Open returns explicit source access and retains the previous guard until decode and
  checkpoint publication succeed. Cancelling drops only the candidate. Save As changes guards
  only after its atomic file publication; a subsequent recovery failure still reports the new
  published destination. FileStatus/open results expose read-only sources, file.save refuses
  them, and local edits plus Save As remain available. Read-only is source capability, not an ACL.
- Project locks live in private per-user app data, keyed by canonical native path bytes. Stable
  lock files remain there rather than being unlinked and recreated across holders; user project
  directories receive no sidecars. Actual contention returns Busy, while inaccessible lock
  infrastructure/read-only directories permit explicit read-only loading. Cooperating same-user
  processes share this protocol; different OS users and hard-link aliases are outside its scope.
- Background/explicit syncs use a separate gate and cloned-handle tickets. The append-state mutex
  is released during sync. Captured generation/sequence bounds prevent a successful older sync
  clearing newer dirty data, and per-generation atomic failure tokens latch known failures before
  subsequent successful append results. Fresh checkpoints replace the token and generation so
  stale completion cannot poison the new file. Flush covers its captured boundary; close flushes
  before discard. Deterministic blocked-sync tests prove these relationships without latency-only
  claims. The250ms group-sync window plus scheduling/I/O latency remains unchanged.

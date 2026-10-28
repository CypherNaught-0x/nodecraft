## 2026-10-23 · Export result bytes without blocking app frames (#587)

- `history.export` remains synchronous by default for headless callers. The desktop opts into
  `background:true`, installs an executor with `Session::set_export_runner`, and receives a
  pending id. `export.status` retains the latest path-free terminal result; `export.cancel`
  addresses that exact id. File writes and cancellation remain human-only in MCP.
- Admission checks path syntax, retained descriptors and the requested history value without
  authenticating indexed bodies. A job clones the bounded AssetStore metadata and retains its
  immutable source leases. The worker captures a source under a short live-context gate,
  then releases that gate before hashing or probing. Already-verified leases are reused;
  indexed bytes are sealed in a separate private workspace, never the live session's staging
  pool or verified cache. One export can retain at most one 8 GiB scratch body plus its staged
  destination. Scratch and its directory are dropped on every outcome. The worker retries a
  busy source capture for at most 30 seconds, checking cancellation between 20 ms waits.
  Saves, imports and arriving run results can use the live context during export verification;
  accepted result bytes remain pinned while the document changes.
- Reuse the existing SaveRunner executor contract without lending project storage. Rejected,
  dropped and panicking jobs produce terminal failures; a rejected export never falls back to
  inline I/O. One process-wide export slot remains occupied until the job finishes or is
  dropped. Cancelling or replacing a Session cannot spawn replacement workers around a blocked
  filesystem call. OS calls themselves cannot be interrupted portably.
- Progress reports Checking, Writing and Finishing phases, rather than an invented percentage.
  Cancellation shares a short state lock with the transition to Finishing. Accepted cancellation
  aborts verification/copy and removes staged output while preserving an existing target.
  It also wins over a racing filesystem error before Finishing. The UI remembers an accepted
  batch cancellation so an occupied-name result cannot restart numbered exports.
  Finishing begins before the atomic writer syncs/publishes; cancellation is then refused and
  the UI says why. Atomic no-replacement and explicit save-dialog replacement consent remain.
- Export All starts one file at a time, advances numbered names without replacing existing
  files, and yields between attempts. Only an occupied-name failure retries another number;
  source failures do not repeatedly recopy the same large damaged asset. Cancelling retains
  already completed files and reports their count. Remaining queued history references are
  resolved when each file starts; a changed project stops remaining files.
- The small progress area owns its pointer gestures and Cancel action; the rest of the canvas
  remains editable. The browser retains its desktop-export guidance. Timeline interchange
  export still uses its existing synchronous command and is outside this result-export issue.

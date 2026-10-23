## 2026-10-23 · Export result bytes without blocking app frames (#587)

- `history.export` remains synchronous by default for headless callers. The desktop opts into
  `background:true`, installs an executor with `Session::set_export_runner`, and receives a
  pending id. `export.status` retains the latest path-free terminal result; `export.cancel`
  addresses that exact id. File writes and cancellation remain human-only in MCP.
- Admission checks path syntax, retained descriptors and the requested history value without
  authenticating indexed bodies. A job clones the bounded AssetStore metadata and retains its
  immutable source leases. Both the first indexed verification/staging and the streamed export
  happen on the executor. This state is independent of project saves: edits never wait for an
  export, and accepted result bytes remain pinned while the document changes.
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

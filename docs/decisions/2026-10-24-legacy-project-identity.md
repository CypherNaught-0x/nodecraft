## 2026-10-24 · Migrate the shared legacy starter identity before local adoption (#879)

- Only the exact old starter id (`02020202-0202-0202-0202-020202020202`) migrates.
  Located storage constructors, file opens (inline or background), and located recovery derive a
  stable id through an invertible `SetProjectId` changeset. The migration is one undo
  step; redo restores the same new id. Read-only decoding and pure `Session::new` construction
  preserve their input. Runs opened by the CLI and serve host supply the source origin and migrate too.
  A domain-separated BLAKE3 digest of the storage location determines the replacement id.
  Native storage and headless loaders canonicalize aliases first; opaque storage keys stay exact.
  The digest is truncated to 16 bytes, deterministically rejecting the reserved and all-zero ids.
  Paths and keys are never stored in the document. Unlocated legacy snapshots keep their original
  conservative pooled identity: a host must supply a stable origin to separate their budget safely.
- Prepare the id, changeset and undo bookkeeping before publishing the replacement recovery
  checkpoint. A failed preparation or checkpoint leaves the old live document and recovery
  intact. The checkpoint contains the migrated id before the document becomes visible, so a
  crash does not return to a pooled identity. Opening never overwrites the source file. The
  migrated document is dirty until saved, and `file.open` reports `identity_migrated: {from,to}`.
  Reopening that saved file keeps its new id. Discarding the migration and reopening the same
  old file also keeps the replacement id and spending total, including read-only opens. Moving
  an unsaved legacy source to a different location gives it a different scope; save the migrated
  identity before moving it to retain the association, just as when deliberately creating a copy.
- **Save As preserves identity.** It continues the same project at a new destination, retaining
  run-journal attribution, project totals and live-sharing associations. This does not introduce
  a project-fork command: two Save As copies deliberately share the same budget. A separately
  created project has its own id. This choice supersedes only the legacy-file limitation in
  [the project-budget decision](2026-10-23-project-budgets.md).
- Existing charges under the legacy id cannot be attributed to individual old files, so they
  stay under that id and still count toward the overall/session caps. Newly migrated projects
  get separate totals for future charges. A restored job under the legacy id is reported
  explicitly and remains parked under its original identity; no handle, charge or paid submit
  is silently transferred to an arbitrary migrated project. The existing parked-job expiry
  still settles an abandoned estimate as unresolved. An explicit undo restores the old identity
  and its shared budget/resume association; it does not rewrite the charge log.
- Joining or adopting a live shared document never migrates the room's identity. Save As also
  leaves that identity alone. If a person undoes an identity migration while hosting a room,
  the existing live-sharing project-id guard disconnects the changed document instead of
  publishing it into the old room; the sync mirror likewise refuses a different project id.
- Validation covers separate usage totals, exact graph inversion/serialization, saved-id
  stability, recovery/undo/redo, checkpoint failure, background adoption, live-join identity,
  Save As identity, and a real mock-provider job interrupted before migration and reported
  after restart without replay or reassignment. No live provider is called.

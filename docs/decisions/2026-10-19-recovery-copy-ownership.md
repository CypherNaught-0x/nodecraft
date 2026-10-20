## 2026-10-19 · Recovery copies belong to one explicit resolution (#450)

An app-global journal can contain unrelated projects or divergent snapshots of the same project.
UUIDs, paths, clean status and replay success prove no ancestry. Resolving an offer removes only
its selected journal and its location sidecar. Every other kept copy remains. Numeric slots are
reused; their order is deterministic, not a claim about chronology. Startup probes only the 99
canonical slots. Unknown names are untouched; exhausted or uncertain capacity refuses before
replacing the live journal. No automatic eviction or lock-sidecar garbage collection is implied.

The engine exposes an opaque `DocumentLineage` identified by pointer identity. Successful Open
or Recover and a new Session replace it; edits, undo/redo and saves keep it. Failed adoption does
not change it. A successful destination save records a witness for the current lineage, including
SaveAs and a published save whose subsequent recovery checkpoint fails (`recovery_ready:false`).
Failed saves do not grant this witness. These bounded host observations are independent of graph
UUID, revision, path, command events and existing execution/view identities.

The desktop captures the new unsaved lineage immediately after Recover. It removes that selected
copy only after an actual save of the same current lineage. An intervening replacement detaches
tracking and retains the copy, even if Open and Save both happen between UI frames. A pending
offer cannot overwrite a newly adopted unsaved document. The modal owns canvas input, including
derived navigation and its choice frame; it does not queue programmatic Session calls.

Start from last save opens the recorded file before removing the selected copy. Failure retains
it and names it in the notice. An untitled offer permits explicit selected-copy discard. Sidecar
paths are best-effort naming, never deletion authority: read a record capped at 4097 bytes (at most 4096 path bytes after trimming trailing newlines),
with one additional byte to detect concurrent growth. Native recovery
continues through bounded native readers, including streamed assets above the classic byte cap.

Incomplete and corrupt records share truthful wording: recovery stopped at that record, and only
preceding complete changes are included. The UI cannot infer how many later changes existed.
`file.recover` in `DocChanged` is a synthetic lifecycle event id, not a registered command or an
agent-callable recovery endpoint. Recovery remains an explicit host adoption.

Tradeoff: conservative retention may require the user to remove unwanted copies before the 99
slots fill. This is preferable to deleting unproven ancestors or overwriting the latest live work.
Metadata durability and multi-process retention policies are unchanged.

## 2026-10-28 · The `media.export` node is a command-driven sink with a human-only folder grant (#746)

This completes what [`2026-10-21-batch-folder-export.md`](2026-10-21-batch-folder-export.md) (#98)
left open: a node kind people place on the canvas, and the desktop app actually offering the
folder grant. It keeps every rule of that record and of #557/#966 (pinned descriptor-relative
writes, hash-verified copies, text refused, Unix-only secure folder writes, messages without
paths) and decides only what a *node* adds.

- **The node describes an export; it never runs one.** `media.export` declares media input ports
  (`image`, `video`, `audio`, `mesh`, `document`, each taking any number of wires) and a parameter
  schema for its filename `template`, `on_collision` policy, `format` (only `original`
  pass-through exists) and an optional `subfolder`. Its card, inspector and MCP schema come from
  that data. It has no outputs and no results. The run planner resolves it away exactly like a
  router or batch: `run.selected`, run-with-upstream, `run.all`/stale, live recompute, a Field
  run, replay, `run.quote` and every other planner entry never produce a job for it, so a
  recompute, undo, redo, autosave or reopened project cannot write a file. Planning it directly
  (`NodeKind::plan`) returns no jobs. Running "with upstream" from an export node still runs the
  nodes that feed it, which is ordinary generation, never an export.
- **Why not a side-effecting `JobSpec`.** A job's results are stored in the project and replayed;
  a job that writes outside the project would be replayed on undo/redo-driven reruns, retried by
  the scheduler, cached by input hash and reopened from autosave. The batch record already refused
  that shape. Instead the host performs the export through an **explicit command**:
  `export.node_plan` / `export.node_run` turn the node into exactly the `export.local_plan` /
  `export.local_run` request (same resolver, same validation, same `Effect::LocalExport`, same
  per-entry outcome) and nothing else. Sources are the active results wired into the node,
  resolved through routers with the planner's `routing::incoming`, in port then wire order; a
  list output contributes each of its items, bounded by the batch's 256-entry limit (one more
  is refused rather than silently cut).
- **The folder is a grant, never a parameter.** The destination is the folder a person picked in
  the desktop app's native folder dialog, held only as **view state** for that node in that
  window (an `ExportHost` with its pinned descriptor). It is not stored in the project, in
  settings, in autosave or in a command result, so a shared or reopened project carries no path
  and no authority, and no agent can plant one: `settings.*`, `node.param_set` and MCP have
  nothing to set. The node's `subfolder` parameter is only a relative name *inside* the grant
  (normal components only, at most 512 bytes; no `..`, no absolute path); it must already exist.
  `export.node_plan`/`export.node_run` with no injected host return Unsupported (and the browser
  build's own message). `local_export::dispatch_with_host` scopes the grant to the one command and
  restores the previous resource afterwards, as #966 does.
- **Agents may preview; only a person writes.** `export.node_plan` is agent-visible like
  `export.local_plan`: without a grant it explains that a person must choose a folder.
  `export.node_run` joins `export.local_run` in MCP's human-only list. Programmatic calls never
  open a dialog; only the card's *Choose folder…* button does.
- **Writing runs off the UI thread, cancellably.** `export.node_run` and `export.local_run` accept
  `background: true`, which hands the already-validated plan to the shared export worker
  (`export_job`): the same process-wide export slot, quit watch and retained receipt as
  `history.export` and #966. `export.status` reports per-file progress and finally the batch
  outcome (one entry per source); `export.cancel` stops it between files and keeps completed
  files. Headless callers keep the synchronous default.
- **Collisions are the node's choice, defaulting to keep.** `on_collision` keeps #98's meaning:
  `skip` (default) leaves existing files alone and reports them; `numbered` writes `name (2)`;
  `overwrite` stages and replaces atomically. Full-disk, permission and read-only failures fail
  that entry with the shared actionable message ("the disk is full; free some space…", "NodeCraft
  can't write there; choose another folder") and the rest continue.
- **Platforms.** Windows and other non-Unix desktops can preview but `export.node_run` returns
  Unsupported before writing, as `export.local_run` does; the browser cannot grant a folder at all
  and the card says so instead of offering the button.

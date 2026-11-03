## 2026-11-03 · Batch Generate batches: runs are snapshots, retry failed, limits, and what a sheet is not (#163)

- **A queued run is an immutable snapshot; edits need a new run.** A sheet run is `run.quote`
  then `run.start` with the rows' ids, like any Generate. The start carries the quote's `token`,
  so an edit between the quote and the start makes the start refuse ("quote again") and nothing
  is sent. Once started, the run plans every job from the document as it was: an edit to a
  queued or running row changes nothing that row's provider is sent, and its result is recorded
  on the row as out of date (the recipe hash no longer matches), never silently mixed with the
  edit. To run the edited row, run it again: that is a new run. An edit that reaches the host
  before it takes an accepted run makes the host refuse it as stale (#409).
- **Keys, retries and regenerates.** Every job of a run has one idempotency key
  (`nodecraft:<run>:<job>`), kept by automatic retries after a timeout or a lost reply and by the
  run journal across a restart (#508), so the provider runs and charges each row once. Running a
  row again on purpose (a regenerate, or Run on an up-to-date row) is a new run with new keys and
  a new price; its result is added to the row's history and made active, never overwriting the
  older one (#81).
- **Retry failed** (`sheet_cmds::batch::retry_failed`) runs, top to bottom, the rows whose latest
  run failed (as the run host reported it: a failure is host state, not document state, so it is
  forgotten on restart) and the rows whose active result is partial (#480, a re-run asks only for
  the missing items), skipping rows still queued or running. A row whose latest run succeeded is
  never retried, so successes are never paid for twice; a row that succeeded once and then failed
  is retried, because its latest run is the failure. The sheet's "Retry N failed" quotes exactly
  those rows. Headless callers do the same from the `Failed` notices or job views they hold and
  `run.quote`/`run.start`.
- **States.** A row is Not run (a draft), Queued or Running (with progress, from the job), Done,
  Out of date, Partial, Failed (with the reason) or Cancelled. Failed and Cancelled are what the
  run host saw this session; the status filter has each of them. "Cancel N" asks every queued
  and running job of the sheet's rows to stop: a queued one is dropped unsent, a running one is
  asked to cancel and may still finish and be recorded (its cost is left unresolved, #409).
- **Naming and reopening.** A batch is named by its sheet's title: `sheet.create` names it,
  `sheet.rename` (one undo step) renames it, and the Sheets panel and `sheet.list` reopen it by
  name. Sheets, their rows, settings, references, provenance and results are document content, so
  they autosave to the recovery journal with every edit and save into the `.ncraft` file.
- **Offline drafts.** Editing, expanding, wiring, renaming, saving and reopening a sheet need no
  provider: none of those commands looks at the provider routing or the run host. Only running
  does, and it refuses with an error that changes nothing.
- **Row settings come from the parameter contract (#162).** A row's Aspect cell is the parameter
  the contract tags `aspect_ratio` (or a choice of `W:H` values), as the card toolbar finds it,
  and says "Not offered" when the model has none. Every other setting is edited in the Row
  settings panel, the canvas's bulk parameter panel (#473) for the selected rows: Mixed marks a
  setting the rows don't share, and one row is edited with `node.param_set`, several with one
  `node.params_set_bulk`. Model… and a row's model cell open the model picker (#157), whose
  review lists every value the new model drops before it applies, in one undo step.
- **NodeCraft's own limits**, chosen for a responsive table and bounded runs, not any
  reference product's plan quotas: a sheet holds at most 1000 rows; one command adds at most 100
  rows (an expansion too); a template variable lists at most 100 values and a template at most
  16 KiB, a text value at most 4096 bytes, copies at most 100; the settings panel edits at most
  256 rows at once (`bulk_cmds::MAX_TARGETS`) and an export saves at most 256 rows; one placement
  puts at most 100 cards on the canvas; a row keeps up to 1024 results. A run is also bounded by
  the planner's and admission's own limits (jobs, input bytes, a provider lane's concurrency).
- **What a sheet is not.** The canvas Batch node (#92) applies one downstream workflow to the
  items of a collection with shared settings; its items have no settings of their own, and a
  sheet's per-row settings don't change it. `nodecraft-cli batch --csv` (#97) runs a whole
  project or Technique once per CSV row, headless, and writes the outputs to a folder; it doesn't
  store rows in the project. A sheet is a document of independent Generate rows, each with its
  own prompt, model, settings, references, history and status, edited and reviewed in the shell
  or through the `sheet.*` commands.

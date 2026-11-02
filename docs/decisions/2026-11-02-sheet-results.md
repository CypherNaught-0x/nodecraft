## 2026-11-02 · Batch Generate results: review grid, send to canvas, export and row history (#163)

- **A row's history is its card's history (#81).** Every run of a row records a new result and
  makes it active; the results before it stay in the row's history, oldest first. Nothing
  separate is stored for sheets, so the retention policy is the history model's: a row keeps up to
  1024 results (`MAX_HISTORY`) and stays within the node size limit, and when either would be
  passed the oldest *inactive* results are dropped first, so the active one is never lost to a
  re-run. `history.previous`, `history.next`, `history.select` and `history.delete` work on rows
  as on any card, one undo step each. `sheet.rows` reports each row's active result
  (`generation`), how many results its history holds (`results`) and the active one's place in it
  (`position`, 1 the oldest). A regenerate is a new result, never an overwrite.
- **Review grid.** The sheet view has a Table and a Grid mode. The grid shows the rows the prompt
  and status filters show that have an active result, as thumbnails (an image result's first
  image, the start of a text result) with the row's status badge (done, out of date, partial,
  running, failed) and number. Clicking a tile selects or unselects its row, the same selection
  the table's checkboxes and the toolbar use. A row with more than one result shows "2 of 3" and
  steps through them with ‹ and › (`history.previous`/`history.next`); the table's Result cell
  says "Done 2/3".
- **Send to canvas is `sheet.place`.** It puts the chosen rows' active results on a plain canvas
  as ordinary source cards, built exactly as `history.extract` builds one (a source of the
  output's modality whose parameter is the value), so they feed other cards at once, never run and
  are never stale. Every item of a list result is its own card. Cards are laid out four across in
  the sheet's row order, from `pos` or, without it, right of everything the canvas holds
  (480 points past the rightmost card, level with the topmost; the origin on an empty canvas).
  Each card records `{sheet, row, generation, port, item, asset}` (the asset's blake3 hash, null
  for text) as JSON text under node config `nodecraft.sheet_result`, so a placed result can always
  be traced to the row and run it came from even after the row is re-run or removed. Placement is
  one changeset and one undo step; ids derive from the caller's `seed`. Rows without a result are
  skipped and reported; placing nothing, a sheet or technique as the target, more than 100 cards,
  a grid that would leave the canvas, or a document the session can't edit is refused. The view
  sends to the project's first plain canvas and stays on the sheet.
- **Export reuses #98 and #966; there is no sheet export command.** Programmatic callers
  export rows' active results with `export.local_plan`/`export.local_run`, naming each row with
  the `output` `sheet.rows` reports (`{node: row, port: output}`): the folder authority, naming,
  collision policy and refusals are #98's, and the web build and platforms without the
  descriptor-relative writer refuse as they do there. In the desktop shell, "Export N" (the
  selected rows with a result) or "Export all" (every row with one) asks for a folder in the
  shell's own dialog and opens the shared workflow-result save (`export.results_plan`/
  `export.results_start`, #966) with those rows, starting on their active results only. That
  flow authorizes only the folder the person chose (the same `ExportHost` #98 defines), previews
  the names, writes on a background worker off the UI thread, can be cancelled, never replaces a
  file and records each file's provenance in `workflow-results.json`; the dialog can switch to
  the rows' whole history. `export.local_run` writes synchronously, so the shell does not call it
  for a batch the size of a sheet. Programmatic calls never open a dialog.
- **Template panel.** A variable is Text or Images. A text variable's "Empty value" box adds the
  empty value (it inserts nothing) after the listed ones; blank lines are still not values. An
  image variable lists the image sources on the sheet that aren't rows, in the sheet's order, to
  tick. Both map one to one onto `sheet.expand`'s `text` and `images`.

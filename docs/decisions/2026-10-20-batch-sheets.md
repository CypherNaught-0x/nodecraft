## 2026-10-20 · Batch Generate sheets are canvases whose Generate nodes are the rows (#163)

- **A sheet is a canvas of kind `sheet`.** A project can already hold several canvases, so a
  sheet is one more canvas, marked with `kind: sheet` (the default `canvas` is omitted, so
  existing files are unchanged; format v1 refuses the field). The desktop never draws it as a
  node graph: it opens as a table.
- **Rows are its Generate nodes.** Every Generate card on a sheet canvas is one row: its prompt,
  model and settings are the row's cells, its active result is the row's output and its run
  status is the row's status. Everything a card already has comes for free and means the same
  thing: the parameter contract (#162) and model swap, run quotes and budgets, per-job
  idempotency keys, partial batches (#480), history, staleness, and undo. Rows are added,
  duplicated and removed by changesets like any node, and bulk edits use
  `node.params_set_bulk`. Running a sheet, or selected rows, is `run.quote` and `run.start`
  with the rows' ids: no second execution path.
- **Row order is position.** Rows are listed top to bottom by their position on the sheet
  canvas (then left to right, then id), so the order is part of the document, undo restores it,
  and no separate row list can fall out of step with the nodes it names. Row commands place new
  rows in that column.
- **References are wired source nodes.** A reference image is a media source node on the sheet
  wired into a row's reference role, so it is validated against the row's model like any wire.
  One wired into every row of a sheet with two or more rows is a shared reference, and rows
  added later get it too; the others belong to the rows they reach. A one-row sheet has no shared
  references, so a reference wired into that row alone doesn't spread. `sheet.rows` lists a row's
  references by input, then wire order.
- **Limits are the row commands'.** The row commands refuse to grow a sheet past 1000 rows and
  add at most 100 at a time. Plain node commands (`node.add`, paste) can still put more Generate
  cards on a sheet canvas; a graph-level cap per canvas kind is left for the sheet view (#163).
- **Why not a separate table model.** A dedicated row store would need its own run planning,
  history, staleness, persistence and undo, and two ways to describe the same generation would
  drift. The cost of the canvas model is that a sheet is limited to what Generate nodes can
  express, which is what #163 asks for.

## 2026-10-18 · Generation history on the card (#81, part 1)

- **History is a node's own data, edited by commands:** `history.select`, `history.previous`,
  `history.next`, `history.delete`, `history.clear` and `history.extract`, each one undo step and
  all agent-visible. History is oldest first; "previous" is older. With nothing active, either
  direction starts at the newest. Stepping past an end is refused rather than wrapping, so
  pressing ← at the oldest result doesn't record an empty undo step.
- **Deleting the active result activates its newer neighbour, else its older one**, so a node
  with results keeps an active one until the last is gone.
- **Extracting a result makes a source node** (`source.text`/`image`/`video`/`audio` by the
  port's modality) whose parameter holds the value. Downstream nodes read a source's parameter,
  not a result, so the new node feeds them at once, never runs, and is never stale. It gets no
  history: editing its parameter can't leave a stale copy beside it. A list output needs an item
  index; a result recorded for a different port layout is refused (results are positional, as
  in the planner); mesh and document results wait for those source kinds. The caller supplies
  the new node id, as for `node.create`; the shell places it beside the source, stepping down
  past earlier extracts.
- **The strip reserves one card row** under the parameters and draws on the card's bottom edge:
  ‹ position › and Clear, then chips for as many results around the active one as fit the card's width (at most 12) (click to
  use, right-click to use, extract or delete). The cap keeps a node with hundreds of results as
  cheap per frame as one with a few; ‹ › reach the rest. ← and → step while the card or a strip
  control has focus (a clicked chip takes focus). Hover previews are built only for the hovered
  chip; thumbnails wait for asset decoding in the shell.
- **Not in this part:** the `+/−` variation counter and its history-or-canvas preference (needs
  the planner to emit several jobs per node and `run.record` to take several results per node),
  and copy/export of a result (host clipboard and save services).

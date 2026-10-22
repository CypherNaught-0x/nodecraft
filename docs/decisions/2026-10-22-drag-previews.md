## 2026-10-22 · Drag previews are view state, made small (#88)

- **A dragged value isn't an edit.** A slider keeps its value as a card draft while it drags and
  edits the document only on release (one undo step). While it drags, the app asks
  `engine::live_actions::preview` for the work the draft changes: the dragged node and the action
  and technique nodes downstream of it. The draft is checked as the edit it becomes would be, then
  applied to a *staged copy* of the document through the ordinary changeset, and the same planner
  runs on that copy. Nothing is recorded: `apply` refuses a preview's outcome, and its images stay
  in the outcome's own staged store, not the project's.
- **Small inputs.** A preview's actions scale input images down to at most
  `PREVIEW_EDGE` (512) pixels on the longer edge before working on them
  (`LocalActions::with_preview_edge`), so a drag stays responsive on large images. Parameters
  measured in pixels (a blur's strength) are not scaled with them, so such a preview looks
  somewhat stronger than the full render. Previews never use the result cache: a cached result is
  full size, and only a recorded round may place one.
- **Shown until settled.** The cards a preview covers show its images (read from its staged
  store) instead of their own results; previews are of images only, so a text action's card
  shows its recorded text until release. A preview is dropped when it lands for a document that
  changed meanwhile or after the drag ended; otherwise it stays until the rounds that follow the
  release have settled (the value recorded and everything it feeds recomputed), the drag ends
  with nothing to run, or another canvas is shown. A drag is a pointer held on a slider: a draft
  left behind by a slider released where it wasn't drawn previews nothing. One round runs at a
  time, previews included, and a held slider is not previewed twice at the same revision. While
  the next preview's images decode, a card keeps the last ones it showed.
- **Copy only what is needed.** The draft is staged on a copy holding only its canvas and the
  technique canvases it may inline (`Project::subset`), falling back to the whole project when
  that part doesn't stand alone.
- **The browser runs rounds in the frame.** wasm32 has no worker threads, so there `compute` runs
  on the UI thread when a round starts. A drag's previews are small; a full-size render holds the
  frame once, on release, as the work of a click would. A worker for the browser build would
  replace this without changing the engine side.
- Supersedes the "Not yet" line of
  [2026-10-19 · Live recompute joins the edit's undo step](2026-10-19-live-recompute.md).

## 2026-10-17 · Bounded graph-card editing (#57, #145)

- Full prompt data is retained; idle cards borrow values and lay out bounded previews. Full drafts
  are lazy, and schema/visibility caches follow document revisions. Dirty/invalid drafts survive
  unrelated undo/redo. Hidden dirty editors explicitly advertise unsaved data and offer discard.
- A drag previews position in shell view state and dispatches one node.move on release. Typed
  slider changes wait for Enter/blur; redundant numeric changes do not create history entries.
- Escape cancellation remembers ownership across egui's focus transition (#145). A failing-before
  event regression verifies the old has_focus-only condition committed; the fixed test preserves
  graph and undo state through the following frame.
- Overview labels remain readable, controls appear only at readable scale, and named keyboard
  Rename actions complement double-click. Settings store lifecycle and scroll preferences remain
  intact in the graph-backed shell. Selection expansion remains owned by #58/PR144.


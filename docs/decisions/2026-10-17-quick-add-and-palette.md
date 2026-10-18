## 2026-10-17 · Quick-add and palette (#61)

- **`node.create {canvas, id, kind, pos}`** creates a kind with its default parameters through
  the node registry, so quick-add keys, the palette and agents share one command; `node.add`
  stays the low-level "insert this exact node". Ids are supplied by the caller (`crate::ids` in
  the shell hashes and probes, as for edges).
- **Quick-add keys come from node kinds' `quick_add` metadata**, not from hard-coded handlers.
  A binding is one ASCII letter or digit, optionally `Shift+` (so `Shift+T` for the technique
  picker works later); anything else is not bound. They fire on a fresh press (not key repeat)
  with exactly those modifiers, outside text fields, menus and gestures, and never while the
  palette is open (the palette also takes canvas keys like Delete and Escape for itself).
- **The palette lists commands that are enabled with no parameters right now** (e.g. Undo when
  there is something to undo); commands that need a target are reached from the canvas or menus.


## 2026-10-23 · Older cards preview safe new inputs (#638)

- **Opening and painting do not edit the saved graph.** The canvas can show the inputs from a
  kind's checked `prepare_migration` proposal when it only appends inputs, preserves every saved
  input and every other node field, and has no warnings. The rule works for any registered kind.
  The canvas leaves changes to settings, outputs, existing input types/order or other data to
  the explicit migration command and its warnings.
- **The wire owns the migration.** Both `edge.connect` and `edge.rewire` use the same migration
  preparation for a missing destination input. `SetNode`, any replaced edges and the new edge
  form one changeset. Refusal leaves the graph intact; Undo restores the exact saved card and
  original wires, and Redo restores the upgrade. Planning continues to use the saved schema.
- **One port list drives every canvas interaction.** Card paint, input hit areas, acceptance
  rings and noodle endpoints use the same preview. A hint on each new input explains that
  connecting adds the inputs and Undo restores them.
- **View retention is bounded.** A separate input-preview cache holds only port lists, never
  node histories or assets. It validates on the first visible use per document revision, and
  invalidates when the session or kind-registry allocation changes. It keeps at most 4,096
  entries within 2 MiB of conservative accounting. It never evicts during a paint/wire pass;
  further cards use their saved inputs when the budget fills, so paint and hit testing agree.

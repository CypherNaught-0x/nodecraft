## 2026-10-22 · Fields are explicit zones, and a collapse hides members only in the view (#66)

- **Membership is explicit.** A node belongs to a field because `node.field` says so, set by
  `field.create`'s `nodes` or `node.field_set`. Dragging a card into or out of a zone changes
  nothing; inferring membership from overlap would make every move a hidden membership edit, and
  a moved card would silently leave its group.
- **A header drag is one `field.move`.** The zone and every member, on screen or not, move by one
  delta in one undo step. Resizing is a `field.update` of the rect and never moves members.
- **Collapse is document state; hiding is view state.** `collapsed` is saved and undoable, but the
  members keep their positions and edges. The shell skips their cards, drops them from the
  selection and ends their noodles at the header, so a collapsed field still shows what feeds it.
- **Fields don't nest.** A node has at most one field; making a field around nodes already in
  another moves them.
- **Not yet:** search and tag navigation don't reveal a collapsed field, and the minimap draws
  hidden members. Both are view-only follow-ups.

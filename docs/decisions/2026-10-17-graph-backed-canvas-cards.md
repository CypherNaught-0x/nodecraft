## 2026-10-17 · Graph-backed canvas cards (#57)

- **An empty stored title means automatic.** New descriptors leave titles empty. The shell derives
  a bounded label from the prompt/text or kind label; edits and undo retain the original graph data.
- **Schema edits are engine commands.** The session owns an injectable node-kind registry so the
  canvas and programmatic callers validate against the same descriptors. Null resets an override;
  missing values are malformed requests. Unknown kinds remain movable and renameable.
- **Widget drafts are view state.** Text commits on blur/submit, sliders on release, and Escape
  cancels text edits. Failed drafts survive changes to other fields. Card motion uses a unique
  engine gesture token, preserving one undo step across pointer events at every zoom level.


## 2026-10-18 · Input order badges (#64)

- **Only list inputs fed by more than one noodle show badges.** A single input, or a list input
  with one noodle, has no order to show. Each badge carries the noodle's 1-based position.
- **Badges sit 70% along each noodle**, towards its input. The noodles into one input meet at
  the port, so badges there would overlap. Badges are hidden below the zoom where card bodies
  hide (`BODY_MIN_ZOOM`), where their numbers couldn't be read.
- **Dragging a badge onto another's place moves it there** (`edge.reorder`): the dropped badge
  takes the position of the badge nearest the drop, and the others close up around it.
  Dropping it back on its own place changes nothing and records no undo step.
- **`edge.reorder {canvas, to, edges}` sets the whole order at once**, listing every connection
  into the input exactly once. Partial, duplicate or unchanged lists are refused, so an agent
  can't silently drop a connection. It's one undo step, using `SetEdge` changes, and an agent
  command.
- **The `H` prompt-overlay toggle from the issue waits** for media cards that show prompts
  (#83); it's tracked separately.

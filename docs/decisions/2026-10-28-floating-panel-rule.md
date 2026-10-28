## 2026-10-28 · One rule for floating panels: move, resize, clamp and stack (#957)

Every panel that floats over the canvas is built by one helper, `floating::panel` in
`crates/ui-egui/src/floating.rs`. Panels supply their title, id, home (top right, or centred for
viewers) and content. The helper alone sets position, layer, movement, resizing and clamping, so
two panels cannot drift apart in behaviour. The rule and an inventory of every overlay are in
[`ui-design.md`](../ui-design.md#overlays-and-stacking).

Panels move only by their header and resize from their right and bottom edges and corner. The
helper owns the header drag: egui's window move is turned off because its window-wide drag
sense also claims a press held on a button in the body, and that repaints every frame until
release. Drags in the body stay with the body's own controls. The shell publishes one
panel area per frame: the window below the application row. egui re-clamps each panel to it
every frame, so a smaller window pulls panels back in and a header never slides under the row.
That clamp is what keeps a panel off the control that opened it, rather than a per-panel offset.

Panels share `Order::Middle`, and egui brings the opened or pressed panel to the front, which
gives a deterministic order: drawing order on the opening frame, then pointer presses. A panel
reopens where it was left. If that, or its first home, would cover an open panel's header, it
steps down by the `panel_cascade` token, so every open panel's title and close button stay
visible. The fixed Shared parameters panel is counted as open for this. Home sits under Search
and to the left of an open Discussions panel, because pinned chrome is drawn above panels.

Menus, popups and question dialogs are `Foreground`, above panels; Rename field and the browser's
Unsaved changes prompt were raised to match. Notices stay `Foreground` in the bottom-left corner,
above panels and chrome. The issue asks for toasts above everything. This record keeps the
2026-10-18 staleness decision's exception: an open quick-add palette or Turn into menu, which owns
the keyboard, is raised over a notice. Moving notices to `Tooltip` would put a notice over the menu
being typed into.

Position, size and order are view state in egui memory. They are never changesets, undo steps or
project data. The Project files window is not converted yet, because open work (#1004) is
changing `file_dialogs.rs`. It should move onto the helper once that lands. Card toolbars (generation, Save
result) follow their card and are not clamped or cascaded.

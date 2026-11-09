## 2026-11-09 · Canvas navigation stays below floating tools (#959)

The minimap and Fit/zoom toolbar are canvas controls. They keep their existing position and
size, but draw in a separate Background area above the canvas root and below Middle floating
tools. The earlier pinned-chrome rule put navigation in Foreground, which painted it over real
Batch grid cells and the Shader editor's request field. A pointer in the overlapping part of a
Batch cell reached Fit all instead of selecting that item.

Keep the floating tools' existing geometry and scrolling room. Reserving a full-width bottom
band would unnecessarily shrink unrelated panels and clip forms with their own fixed content.
Application chrome, menus, questions, notices and tooltips retain their existing layer rules.
When navigation is uncovered its map and toolbar remain interactive above the board and cards.

Early minimap pan admission must agree with painting: remember the navigation area's actual
layer and admit a new map press only when that layer is under the pointer. An already-owned map
drag still finishes normally. This prevents a covered map from moving the canvas before the
floating tool receives its text or button input. The choice changes no document or saved view
format and adds no provider calls.

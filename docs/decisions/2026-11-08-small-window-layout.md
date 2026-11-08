## 2026-11-08 · Small windows: the row overflows by priority, the minimap scales (#1115)

This replaces the all-or-nothing overflow of [the unified top bar](2026-10-27-unified-top-bar.md).
At the desktop's smallest window (640×400) the old rule put every entry, Search and Comments
included, into one More menu, and the fixed 320-point navigation panel covered a large share of
the canvas.

The row now keeps as many entries as fit. Entries move into More from the right in a fixed order:
the navigation, activity and service entries first, then the entry of an open panel, then
Comments, then Search. More sits after the entries the row keeps and lists only what moved, in row
order. The choice is a pure function (`topbar::fit`) of the measured widths, so a test can pin it.
Icon-only entries were not added as an intermediate step: they would need a second measuring pass
and a second look for every entry, and the priority order already keeps Search and Comments
visible at 640 points.

The navigation panel takes a third of the canvas width between 240 and 320 points, and the map
keeps its default shape. Below a 720×480 canvas the map hides itself and its Hide map toggle goes,
leaving Fit and the zoom badge. The person's own map choice is kept for when the window grows.
Toolbar buttons share the width by their label widths, so 240 points still shows every label.

The agent sidebar is resizable between 280 points and half the window, so a 640-point window
keeps half its width for the canvas. Collapsing it to an icon below a breakpoint was left out.
Windows that are not floating panels get `floating::on_screen`, which caps their width at the
window and holds them inside it. Replacing those windows is #1116.

The generation toolbar of a card under the row now stays below the row. At 1000–1400 points the
row has entries where it used to show only More, so a toolbar clamped to the canvas top went
under it.

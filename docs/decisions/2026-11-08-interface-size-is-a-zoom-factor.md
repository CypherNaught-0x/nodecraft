## 2026-11-08 · Interface size is egui's zoom factor, and cards grow with it (#1107)

The interface size (80–200 %) is applied with `Context::set_zoom_factor`, not by scaling the
theme's tokens. Every size the shell uses is in points, so one factor grows text, spacing, panels,
menus, questions, the top bar and the minimap together, and no control needs a second, scaled
size. egui's own Ctrl +/- zoom keys stay off (`zoom_with_keyboard = false`): Ctrl +/- belongs to
canvas zoom, and the interface size has its own commands and Ctrl+Alt shortcuts.

The canvas is drawn in points too, so cards grow with the interface, as a page does under a
browser's zoom. Compensating for it (dividing canvas zoom by the factor so cards keep their pixel
size) was left out: it would touch every piece of canvas geometry, hit-testing and level-of-detail
threshold, and a person who wants larger chrome usually wants larger card text as well. The zoom
badge keeps reading canvas zoom alone. Image previews are requested at `pixels_per_point`, which
includes the factor, so they stay sharp.

The setting belongs to the installation (`[appearance] interface_size`, or the browser origin's
storage), never to a project, and the `view.ui_scale.*` commands record no undo step. A value that
isn't a number from 80 to 200 reads as 100 % with a notice rather than resetting `[appearance]`.

## 2026-10-20 · The Layer Editor stores layers on its node and composites locally (#101)

- **Layers live in the node's config.** `image.layers` keeps its layers as one JSON document
  under `config["nodecraft.layers"]`, changed only through `node.layers_set` and the `layers.*` commands, so
  every edit is one invertible changeset. Width, height, background and transparency are
  ordinary parameters (#162).
- **Wired images are layers by identity, not by position.** Each wired image is listed as an
  automatic image layer whose id comes from its upstream node and its index there, fitted to the
  frame, until something about it is edited; then it is stored like any other layer. Reordering
  inputs, moving or renaming the source card keeps the layer. Unwiring an image keeps its stored
  layer listed but not drawn until it is deleted or wired in again, so a slip doesn't lose its
  placement.
- **Frame pixels, centre and degrees.** A layer's `x`/`y` is its centre in frame pixels, with
  `width`/`height` before rotation and `rotation` in clockwise degrees about the centre; flips
  apply before rotation. Alignment uses the rotated bounding box. Every number is bounded on the
  way in (frame 100–4096 px, coordinates, text size, stroke width) and the composite's pixel
  count is checked against the media limits.
- **One renderer.** `nodecraft_actions::layers` draws the run output and the editor's text and
  shape previews (`render_layer`), so what the editor shows is what the run makes. Text is shaped
  with `harfrust` and drawn from `skrifa` outlines with the bundled Lato (OFL); any other font
  name falls back to Lato until custom-font import lands (#548).
- **A gesture is one step.** The full-screen editor previews a drag locally and commits one
  `node.layers_set` when the pointer lifts; a properties field commits one `layers.update` when it
  lets go. Snapping (frame and other layers' edges and centres) and Shift/Alt modifiers happen in
  the editor only; the commands take final values.

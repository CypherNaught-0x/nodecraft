## 2026-10-22 · Masks record the image they were painted over (#546)

- **The binding lives with the strokes.** A mask's stored strokes carry an optional `over`: the
  content hash and size of the image under it (under the Expand canvas, when there is one) when
  its first strokes were painted. Adding strokes keeps it; clearing them forgets it. Masks saved
  before this have none and are never reported stale, so older projects open and run unchanged.
- **Changed is a warning, reshaped is a stop.** A new image of the same shape (a re-run, a new
  seed) badges the mask card and the editor asks to check the strokes, but runs go ahead: the
  strokes still land on the same part of the frame. A new image whose aspect ratio differs by
  more than 1% would stretch the strokes, so `run.quote` lists the mask as blocked
  (`[mask_reshaped]`) and `run.start` refuses, like an oversized media input (#675), before
  anything is charged.
- **The user chooses, through one command.** `mask.refit` with `fit: false` keeps the strokes as
  they are and binds them to the new image; `fit: true` scales the old image to fit inside the new
  one, centred, and moves the strokes and brush sizes with it, Expand margins keeping their size in
  pixels. Either is one undo step. Only an explicit `mask.refit` moves the binding, so nothing
  stretches strokes silently.
- **Transparent areas count as marked.** With "include transparent areas" on, `mask.edit` accepts
  a mask with no strokes or margins when the source image has a transparent pixel (a cut-out), and
  refuses one that has none.

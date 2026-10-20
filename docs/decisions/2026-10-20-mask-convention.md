## 2026-10-20 · Masks are stored as strokes and rendered white and transparent (#102)

- **Strokes, not pixels.** A mask card stores its brush strokes (size and hardness relative to the
  image's longer side, points in 0..1 of the image) as bounded JSON, and the `image.mask` action
  renders them at the input's size. The mask follows its image through resizes and outpainting,
  stays small in the project file, and undoes one stroke at a time through `node.mask_set`.
- **One mask image for every provider.** The rendered mask is white and fully transparent where
  the image should change, black and opaque where it should stay. OpenAI Images reads alpha
  (transparent = edit) and Stable Diffusion inpainting on Replicate and ComfyUI reads luminance
  (white = repaint), so the same PNG works for both without a per-provider conversion step.
  Replicate's reading of it is checked against its public schema only; no paid call verified it.
- **Outpainting is expand, then mask.** `image.expand` pads the image with transparent margins and
  the mask's "include transparent areas" option marks them, so outpainting is inpainting the new
  edge. The editor creates both cards in one undo step (`mask.create`) and `mask.edit` wires the
  expanded image and the mask into a catalog model with a mask input.

## 2026-10-22 · Image tools menu (#103)

- **One submenu, every tool.** An image card's context menu has "Image tools": Inpaint, Expand,
  Remove background, Upscale, Split into grid, Split into layers, Color grade, Make Element and
  Open in 3D, always in that order. A tool that can't be used there is greyed, and its tooltip
  says why. The list comes from `image_tool_cmds::menu`, which the `image.tools` command also
  returns, so agents see the same menu with the same reasons.
- **Each tool reuses what exists.** Inpaint and Expand open the mask editor (`mask.create`),
  Make Element calls `element.create` on the card, and Split into grid keeps its own rows and
  columns submenu. The rest go through `image.tool`: Color grade adds an `image.grade` action,
  the model tools add a generation card (a 3D card for Open in 3D) with the image wired in. Split
  into layers also adds a Layer Editor fed by that card. Each is one undo step, and nothing runs:
  the new card is priced and run like any other. Model tools are marked "AI".
- **The catalog decides the model tools.** A model tool is offered when some catalog model has a
  mode with the tool's id (`upscale`, `remove-background`, `image-to-layers`, `image-to-mesh`)
  that needs nothing but the one image; an optional prompt is fine. The first such model is the
  default. The builtin catalog has no background removal or layer split yet, so those two show
  greyed until a provider adds one.
- **Single-image tools need a single image.** Inpaint, Expand, Color grade, Split into grid and
  Make Element are greyed on a card that makes several images; the model tools fan out over a
  list like any card fed by a batch.

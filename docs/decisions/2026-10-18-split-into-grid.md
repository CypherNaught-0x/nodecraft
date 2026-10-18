## 2026-10-18 · Split into grid (#86)

- **A split makes new Image nodes; it doesn't change the source.** `engine::split::split_grid`
  cuts a node's image into rows × columns tiles. The image is a Source image's asset, or the
  first image of a generator's active result. Each tile becomes an Image node holding its own
  PNG copy in the project's asset store, and the tiles are laid out in the same grid to the
  right of the source. One `canvas.paste` adds them, so one undo removes them all. The new tiles
  become the selection.
- **Tile order is row-major:** left to right, then top to bottom.
- **Uneven sizes:** tile edges are spread evenly (`length × i / count`), so tiles differ by at
  most one pixel and together cover the image exactly. Nothing is cropped away and nothing is
  padded.
- **Limits:** 1 to 16 rows and columns, at most 64 tiles, at least one pixel per tile. Decoding
  uses `media`'s limits. Every tile is cut and encoded, and the total checked against the asset
  store's room, before anything is stored. If adding the nodes fails anyway, the stored tiles
  are pruned.
- **Not a command yet:** like file drops (#197), a split stores asset bytes, which command
  handlers can't. An agent-callable form is tracked with the import command in #319.
- **Not yet:** picking N×M visually on the image, and wiring the tiles into a collection (#92).
  Cards don't render images yet; for now the card's context menu offers "Split into grid" with
  rows and columns fields.
- **Picking visually (#337):** while the card's "Split into grid" menu is open, the card draws the
  grid over its image in the accent colour, updating as rows and columns change. The lines use
  `split::edge` and the image's full size, so they fall exactly where the split will cut. They
  go away when the menu closes.

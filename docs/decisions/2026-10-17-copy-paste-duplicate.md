## 2026-10-17 · Copy, paste, duplicate (#63)

- **The clipboard carries versioned JSON** (`{"format":"nodecraft.nodes","version":1,…}`) of the
  selected nodes and the edges between them, so copies move between projects and windows through
  the OS clipboard. Paste remaps every id, drops edges to nodes that weren't copied, and lands
  the group centred on the pointer, as one `canvas.paste` changeset.
- **Copies leave generation results behind** (`node.add`'s rule): pasted nodes start without
  history. Copying results waits for the asset store (#54); pasted images and dropped files are
  #197.
- **Pasted nodes of installed kinds must match them** (ports as the instance declares, parameters
  the schema accepts); nodes of kinds not installed here are kept as data, as a loaded project
  keeps them. Tags and fields the target canvas lacks are dropped, so copies move between canvases
  and projects. A NodeCraft copy this version can't read shows a notice instead of becoming text.
- **Parameters travel through the OS clipboard as they are**, which is safe because kinds never
  keep secrets in parameters or config (keys live in the keychain).


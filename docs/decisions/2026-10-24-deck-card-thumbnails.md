## 2026-10-24 · Deck card previews and copied sources (#695)

- **The card previews the first three slides.** A Deck reserves the existing card preview
  area, including when empty, so arriving previews do not move its parameters. The preview
  uses the same checked frame, layer rendering and imported fonts as the slide editor.
  Missing sources draw placeholders; newer or unreadable decks are kept and show a notice.
- **Cache art, never document mutations.** Parsed display data is refreshed after document
  revisions. Text and shape textures are reused by slide/layer content, font availability
  and render scale, so moving a source does not redraw it. Images use the shared asynchronous
  preview cache. While a slider is dragged, a source reads the staged result and its staged
  asset store; release returns to the committed result. Neither path edits a deck.
  At most sixteen cards retain thumbnails, each sharing an eight-MiB vector-texture budget
  across its layers. Oversized art is a placeholder. Font payloads use the existing bounded
  reader and are dropped after rendering instead of being retained per card. The cache resets
  with the session identity; unreadable fonts are retried when a card returns after an overlay.
- **Copies follow the same copied selection.** After all fresh node IDs are chosen, the
  clipboard remaps readable `Content::Source` origins through that complete map. A source
  outside the copied selection keeps its original ID. Paste and duplicate still dispatch one
  `canvas.paste` changeset, so one undo removes the whole copied group. Deck and slide extra
  fields and unsupported layers are preserved; unreadable decks and newer versions travel
  untouched. If remapping would exceed the stored-deck limit, the whole paste is refused.
  This resolves the copy limitation recorded in [the original Deck decision](2026-10-22-decks.md).

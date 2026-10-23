## 2026-10-23 · Variations can arrive as cards; a fixed seed is flagged (#808)

- **The preference is per canvas**: `Canvas.variation_cards`, set with
  `canvas.set_variation_cards {canvas, cards}` (one undo step) and toggled from the variations
  counter on a card ("as cards" / "in history"). History stays the default, so a project saved
  before #808 reads the same. The flag is serialised only when set.
- **The last variation stays on the source; the others get cards.** The issue reads "each
  variation after the first", but what comes after the node already runs on the last variation
  (#81), so keeping the last on the source keeps the run's downstream result where it is. Every
  earlier variation of the same progress goes on a new card.
- **A card is a copy of the source.** Same kind, parameters and title; its incoming edges are
  copied so it can be rerun alone; its variation count is dropped (it is one variation) and its
  history holds only its own result. It is placed right of the source, stepping down past cards
  already there. Card and edge ids are derived from the generation id (blake3), so replaying a
  run's records makes the same cards and recording one twice is refused. The card and its record
  are one changeset: one undo removes both.
- **The record says where a result goes.** `run.record` takes an optional `card` (a new node id)
  and `later` (the ids of the variations after this one). The run host fills `card` when the
  canvas asks for cards. After a restart a variation can land after its later siblings; with
  `later` it is inserted before the first one already in the history and doesn't take over the
  active result, so the order and the active result are the same as an uninterrupted run. In card
  mode such a variation goes on a card instead.
- **A fixed seed is a warning, not a refusal.** A seed parameter that is set (or defaults to a
  fixed value) is sent unchanged to every variation, so a deterministic model returns the same
  result N times at N times the price. The card shows "same seed" once the count is above one,
  and `run.quote` lists such steps under `alike` (node, variations, seed names), which the
  confirm dialog and the CLI show before anything is spent. A schema-only model counts a seed as
  fixed when it would be sent; a semantic contract when its value is explicit or a fixed default.
- **Copying a history result to the OS clipboard** needs a clipboard host service that doesn't
  exist yet; it is #831.

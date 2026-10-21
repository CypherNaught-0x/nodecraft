## 2026-10-21 · Source video edits are declared per mode and priced per second of source (#179)

- **The edit is data on the mode, not a new node kind.** A catalog mode that edits a wired
  video declares `edit`: a provider-neutral operation (`video.motion_transfer`,
  `video.object_swap`, `video.restyle`), the source role, the shortest source, how much of a
  longer one the provider uses, the smallest frame and what happens to the sound (`source`,
  `silent` or `unknown`). Another provider's motion transfer declares the same operation under
  its own endpoint. The generation card, the planner and the picker read it; the catalog checks
  that the role is the mode's one required video input.
- **Checked before paying, from what the asset already knows.** A source shorter or smaller than
  the mode takes stops the run before anything is sent. The length and frame size come from the
  asset's metadata, which import now reads from MP4 and QuickTime headers without decoding. A
  value nobody read goes to the provider, which checks it.
- **Priced per started second of the used source.** `per_source_second` pricing names a choice
  setting (Genjutsu's resolution) and a price for each of its options. The quote counts the part
  of the source the provider uses (the first 30 s for Genjutsu), rounded up to whole seconds. The
  catalog carries the provider's list price, which is the most a run can cost: Higgsfield's plan
  credits can make it cheaper, and live estimates are #663.
- **An unknown length is unpriced, never free.** When the source is made earlier in the same run,
  or its length couldn't be read, the step is unpriced with its own reason ("priced per second of
  its source video, whose length isn't known yet"), and the run needs partial-cost consent like a
  usage-priced one.
- **An optional prompt is optional.** A mode whose prompt role has `min = 0` sends no prompt when
  the card's is empty, instead of refusing the run.

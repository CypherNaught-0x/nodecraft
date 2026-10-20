## 2026-10-20 · Media Generate fans out over a prompt batch; list ports flatten per-item lists (#519)

- **Every Generate card fans out over its prompt.** Image, video and audio Generate mark
  `prompt` `fans_out` like text Generate (#515), so a batch of prompts runs one job per prompt.
  Media roles (references, masks) still collect: a batch of references is one request.
- **A fanned list result is a list per item.** A media card makes a list per run, so its fanned
  result is `List[List[media]]`, one inner list per batch item, in item order with the item tags
  alongside. The per-item grouping is the provenance; nothing else records it.
- **A collecting list port flattens it.** A downstream list input that collects (a Generate
  image reference) receives every item's media in item order, whether the step runs in the same
  run or reads the stored result, and a Batch fed by it holds each media as one item. A single
  port, or a list port that fans out, still refuses the nested shape: it would need one run per
  media, which no one has asked for, and silently picking the first would lose work.
- **The fan/collect choice is part of the recipe, only where it matters.** The staleness recipe
  leaves `fans_out` out of the hashed contract, so declaring it changes no hash on its own.
  Instead a node gets a `fan` marker for each fanning input a batch (or a fanned node upstream)
  feeds. A media result made by joining a prompt batch therefore shows stale once its card fans
  out, and one with no batch upstream stays fresh. Hashing the declaration instead would have
  staled every media result once. The cost lands on text results recorded since #515, which
  hashed the declaration: they show stale once. #515 is unreleased, so these are only
  development builds' results, and running them again gives the same answers, so it is not
  worth a recipe version.
- **The card shows a row per item.** A card's preview draws the active result's images in rows,
  one per batch item (one row for an unbatched run), at most 4 rows of 4 with a "+N more" note.
  This also gives unbatched media Generate cards a preview, which looked only for a single image
  before. Split and the image viewer still use the first image.
- **Extract counts media, not items.** `history.extract`'s `item` indexes the values of a list
  output with each per-item list opened, so the Extract menu offers each image ("item 3 of 3")
  rather than an inner list it can't hold. On a plain list this is the same index as before.

## 2026-10-19 · Batches are resolved by the planner and fan out per item (#92)

- **A batch never runs.** Like a router, `structure.batch` is resolved at plan time into its
  items: the values wired into its `in` input (results of other nodes and sources such as
  uploads, lists spliced), then, for a text batch, its own `text` split by line (the default), a
  custom separator (1–64 bytes), the CSV first column, or not at all. Split pieces are trimmed
  and empty ones skipped; wired items are never re-split, so changing split settings keeps them
  and their order. A batch holds at most 1,000 items. One modality per batch, chosen in
  `config["modality"]` as for routers; all six modalities are supported, but only text batches
  have their own items.
- **CSV.** RFC 4180 quoting (`""` for a quote, quoted newlines allowed), comma separated, LF or
  CRLF rows, a leading BOM ignored, blank lines between records skipped, and an optional header
  row (on by default). Text is UTF-8, as every parameter is. An empty first cell, an unterminated
  quote or text after a closing quote is an error naming the row: no row is dropped silently.
- **Who fans out.** A *single* input fed by a batch, directly, through routers, or by a result
  that a batch fanned out, runs the node once per item. A *list* input fed by a batch collects
  every item into one run instead. A fanned result is stored as one generation whose outputs are
  lists with one value per item and whose `items` field tags each value with the batch items it
  came from (`ItemKey { batch, item }`, one per batch, ordered by batch id). Unfanned results
  write no `items` field, so existing files are unchanged.
- **Stable item ids.** An item's id is a hash of its batch, its value and which occurrence of
  that value it is, so reordering or adding items keeps every other item's id, and repeated
  values stay distinct.
- **Pairing.** Inputs that share a batch (a diamond from one batch) are joined on its items: one
  run per item, never a self-product. Inputs from different batches pair by **Cross** (every
  combination; the default) or **Zip** (item by item). Zip is used only when every batch involved
  says Zip and needs equal sizes. Unequal Zips and mixed settings are errors, never a silent
  change: `docs/plan.md` mentioned min(N, M), but quietly dropping rows (or turning a Zip into a
  larger paid Cartesian run) is worse than asking the user to fix the sizes.
- **Bounded before it is built.** The number of combinations is checked against 1,000 before any
  is built, combinations name at most 16 batches, each combination's copied inputs count against
  the plan's byte budget, and a fanned step's jobs against the 4,096-job step cap. Quotes count
  a fanned step's jobs per item, also when they are planned later in the run.
- **Staleness.** A batch contributes its settings and its feeds to the input hash of what it
  feeds, so editing its items makes the results downstream stale.
- **Not yet (later slices of #92).** A batch fed by a node in the same run, a batch feeding
  another batch, and a fanned list output into a list input are blocked with a reason. Per-item
  result navigation, the Cross matrix review and batches inside Techniques come with the UI and
  Technique work.

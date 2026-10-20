## 2026-10-20 · A batch wired into a list input can run once per item (#515)

- **The input says it fans out.** A list input collects every wire into one list, so a batch
  wired into Generate's `prompt` (a list, so several prompt pieces can be joined) ran once with
  every prompt joined. An input contract now carries `fans_out` (contract 1.2): a list input
  marked with it runs the node once per item of a batch wired into it, as a single input does.
  Text Generate marks `prompt`; media roles keep collecting, because a batch of reference
  images is one request with several references.
- **Text only for now.** Image, video and audio Generate make a list per run, so fanning them
  would give one list per item, which downstream ports, the card and Extract can't read yet. They
  keep joining a prompt batch until #519 flattens per-item lists (since done, see
  `2026-10-20-media-fanout.md`).
- **Earlier results don't turn stale.** The staleness recipe doesn't include the fan/collect
  choice, so a text result made by joining a prompt batch before this change still shows fresh.
  Running it again makes one answer per prompt. This one-time change wasn't worth a recipe
  version.
- **The other wires join each item in place.** The planner keeps the fanned wire's position in
  the input's wire order, so each run sees its item where the batch was wired, between the
  pieces before and after it. Wire order still decides prompt order.
- **Several fanned wires combine as single inputs do.** Each fanning wire is a relation to
  `fanout::combine`, like a fanned single input: wires from the same batch join on its items,
  and different batches pair by their Cross or Zip setting. Two batches into one prompt
  therefore run every combination (Cross) or item by item (Zip), never silently joined.
- **Optional and refused where it can't apply.** `fans_out` defaults to false and is omitted, so
  older contracts mean the same; registration refuses it on a single port, where it would mean
  nothing.

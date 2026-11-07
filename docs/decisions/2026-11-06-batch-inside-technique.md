## 2026-11-06 · A batch inside a technique fans its technique node out (#1073)

This lifts the last "not yet" of [`2026-10-28-nested-batches.md`](2026-10-28-nested-batches.md)
and of [`2026-10-20-technique-batch.md`](2026-10-20-technique-batch.md): a technique node whose
version holds a batch no longer blocks with "a batch inside its technique can't fan it out yet".

- **Same rules as on the canvas.** Inlining copies the version's batch onto the planning canvas
  like any other node, so the planner resolves it as it resolves a batch on the canvas: the same
  1,000-item limit over a chain, the same chain of at most 16 batches, the same refusal to collect
  a node in the run that isn't a source, and invalid settings that block with the batch's own
  reason. Its reason reaches the technique node as "inside its technique: …", as every inner
  blocker does.
- **What can feed it.** Every node of a version except its inputs runs with the technique, so a
  version's batch can collect only the technique's inputs (which take whatever feeds the
  technique node, such as a result already on the canvas) and source cards. One fed by another of
  the version's steps blocks the technique node with a reason that says so and what to do:
  publish the technique with what feeds the batch outside it, wired to an input.
- **The technique node fans as it does for a batch on the canvas (#533).** What it collects is
  fanned per item, so it becomes a fanned step; every item's jobs are planned, and so priced and
  quoted on the technique node, before anything is sent.
- **Items are tagged by the batch's inlined id.** That id is derived from the technique node and
  the version's batch (`technique::derive`), so it is the same at every planning and a partial
  result is completed item by item as for a batch on the canvas (#536). It isn't a node of the
  real canvas. Its own Cross or Zip setting pairs it, as any batch's does, with another batch
  fanning the same step; a batch on the canvas wired into the version's batch is collected by it
  (a batch collecting a batch flattens, #92).
- **Review reads the inlined canvas.** "Review items…" labels an item from a batch that isn't on
  the canvas by inlining the canvas's techniques first, so the version's batch titles and values
  name the items.

## 2026-10-20 · A batch into a technique node fans the technique out (#533)

- **The inlined nodes fan, and the technique node follows them.** Inlining wires a batch feeding
  a technique node's input straight to the version's inner nodes, so those fan out as any node
  fed by a batch does (#92, #515). The technique node then takes each output per item: it
  becomes a fanned step whose items are those of the inner nodes it collects, and each item's
  run gives back that item's outputs. Its own inputs only feed the inlined nodes, so they never
  fan it themselves: a version that takes every item in one run (a list input that collects the
  batch) runs once, and so does the technique node.
- **One result per item on the instance, tagged like any batch result.** The instance's history
  gets one result whose outputs hold a value per item and whose item tags name the batch's
  items, stored the way a Generate card stores its batch result (#515). A list output gives
  each item its own list.
- **Quotes multiply and name the instance.** The inner steps fan, so their jobs are counted per
  item, and they are priced on the technique node as before.
- **A batch inside a version still blocks it (#93).** When an output the technique node
  collects is fanned by a batch that is itself one of the version's nodes, the technique node is
  blocked with "a batch inside its technique can't fan it out yet"; only a batch on the canvas
  fans a technique node. A list output kept per item on an inner node that doesn't run again is
  refused too (inner nodes other than inputs always run again, and inputs never carry item
  tags, so this only guards hand-made files).

When some items of a fanned technique fail (#536), the technique node keeps a partial result,
as a fanned Generate card does (#480): an item that fails inside the technique is carried past
the inner steps that read it, the items that finished become the node's result, and the missing
ones are listed. A re-run with unchanged inputs runs only the missing items through the fanned
inner steps and keeps the rest. A free unfanned inner step (one that runs once, before the
fanned ones) runs again in full and makes what it made before. A resume is not attempted, and
the technique runs again in full, when an item reads other items' results or when a paid step
shared by every item feeds them: the missing items would be made from results this run doesn't
have, or from a new shared result that the kept items were not made from.

## 2026-10-20 · A batch into a technique node fans the technique out (#533)

- **The inlined nodes fan, and the technique node follows them.** Inlining wires a batch feeding
  a technique node's input straight to the version's inner nodes, so those fan out as any node
  fed by a batch does (#92, #515). The technique node then collects each output per item: it
  becomes a fanned step whose items are its inner outputs' items, joined on the batches they
  share with anything else feeding it, and each item's run gives back that item's outputs.
- **One result per item on the instance, tagged like any batch result.** The instance's history
  gets one result whose outputs hold a value per item and whose item tags name the batch's
  items, stored the way a Generate card stores its batch result (#515). A list output gives
  each item its own list.
- **Quotes multiply and name the instance.** The inner steps fan, so their jobs are counted per
  item, and they are priced on the technique node as before.
- **Unchanged:** a batch inside a version that is fed from outside is still refused at publish
  (#93). A list output kept per item on an inner node that doesn't run again can't be reused (an
  inner node always runs again, so this only guards hand-made files).

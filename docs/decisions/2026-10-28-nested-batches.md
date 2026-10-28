## 2026-10-28 · A batch collecting a batch flattens it, and fanned results are reviewed per item (#92)

This finishes two items that [`2026-10-19-batch-fanout.md`](2026-10-19-batch-fanout.md) left under
"Not yet": a batch feeding another batch, and per-item result navigation with the Cross matrix
review. A batch fed by a node in the same run still waits for that node's result (whole-canvas runs
list what is after it as waiting, #965), and a fanned list output into a list input is still
refused.

- **Flatten, not a nested product.** A batch's `in` is a list input, and a list input fed by a
  batch collects every item (#92). A batch wired into a batch does the same: its items join the
  collecting batch's at that wire's place, in its order, before the collecting batch's own text
  items, exactly as the canvas's text preview already showed them. A nested product would be a
  second way to write what two batches reaching one node already do with Cross, and it would turn
  a wire that looks like "add these items" into a multiplied, paid run.
- **The items are the collecting batch's.** Each flattened item takes a new id under the
  collecting batch (its value and occurrence, as for any item), so results downstream are tagged
  by the batch that fans them. A node reached by both a batch and a batch collecting it fans over
  two different batches and pairs them by their Cross or Zip settings, as any two batches. Wired
  twice, a batch's items come twice, each a distinct item.
- **Bounded before any copy.** The 1,000-item limit counts every item over the chain, and is
  checked as each wire's items are added, so an oversized chain is refused before it is built. The
  message says what to do: remove items or split them across batches. A chain holds at most 16
  batches (`planner::MAX_BATCH_CHAIN`; staleness follows 32), a loop of batches is refused with a
  reason (a valid canvas can't hold one), and each batch in a chain is resolved once however many
  wires reach it. Every inner batch is held to the outer one's rules: it collects existing results
  only, and its invalid settings block the run with its own reason.
- **Quoted per item.** Flattened items are known when the run is planned, so the quote counts and
  prices every item (and every Cross combination) before anything is sent, like any batch's.
  Staleness already hashed a batch's feeds recursively, so editing an inner batch makes what the
  outer one feeds stale.
- **Review is view state.** "Review items…" on the context menu of a card whose result was fanned
  out opens the Batch items window. It steps through the items with Previous and Next, naming each
  batch's item it came from and showing the item's text or images, or that it failed or was
  cancelled. While the card runs again, the window says the results shown are the earlier ones.
  Nothing it does changes the document, so it isn't a command and has no undo step. Agents read
  the same result through the `history.*` commands; `history.extract` takes one item out of it.
- **The Cross grid.** When a result was fanned out by exactly two batches and holds every
  combination of their items once (a Cross), the window shows them as a grid, the first batch's
  items (by node id, the order item tags use) as rows and the second's as columns, in each batch's
  current order. A cell shows its result's first image, a short text, or Failed or Cancelled;
  clicking it shows that item above. A Zip, or a result over three or more batches, has no grid,
  only the stepper: a grid of a Zip would be mostly empty cells that never ran.
- **Labels come from the batches as they are now.** Items are labelled with their batch's current
  values (`planner::batch_items`, the planner's own resolution). An item the batch no longer holds
  keeps its place and says "no longer in the batch", so editing a batch never hides results that
  were paid for.

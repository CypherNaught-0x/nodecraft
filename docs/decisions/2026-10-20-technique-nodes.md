## 2026-10-20 · A Technique node pins one version and is inlined for planning (#94)

- **A placed node pins one version's interface.** `technique.place` makes a
  `structure.technique` node whose config holds the version it runs (technique id, version
  number, version canvas) and the ports and parameters that version exposes, with their types
  and schemas. Its ports and parameters come only from that stored interface, so publishing a
  new version, or removing one, never changes a node already on a canvas.
- **Runs inline the version, for planning only.** Before planning, each technique node is
  replaced on a working copy of the canvas by the version's nodes, under ids derived from the
  instance and the inner id; the version's inputs are wired to whatever feeds the instance, and
  its exposed parameters take the instance's values. The instance itself stays in the plan as the
  last step and gives the inlined outputs back as its own results. The working copy is never
  stored: the run records only the instance's results, so a canvas never grows hidden nodes, and
  quotes, failures and provisional estimates name the instance, not the inner nodes.
- **An unrunnable version blocks only its instance.** A version that is missing, removed, or no
  longer matches the interface the node pinned blocks runs of that node (and what it feeds) with
  a message saying to upgrade it; the rest of the canvas still runs. Nesting is capped at 8
  levels, 2000 inlined nodes and 10 000 inlined wires per canvas (a fed input's wires
  multiply by what feeds it); a node past a cap, or whose inlined ids collide, is blocked alone.
- **Inner nodes get their own provider routes.** Routes are chosen per node, so a host routing a
  run also routes the inlined ids (`expand(...).inner`), which are deterministic for a given
  instance.
- **Upgrading keeps what still fits.** `technique.upgrade` moves a node to another version of the
  same technique, in either direction. A parameter is kept when the new version exposes one of
  that name whose schema accepts the value; otherwise it takes the new default. A wire stays when
  its port still exists with the same type; the others come off. Title, position, history and
  tags stay; the result in use stays only while the outputs are unchanged. The response lists what was dropped, so a caller can say so.
- **Detaching copies the pinned version back.** `technique.detach` replaces the node with copies
  of its version's nodes, laid out at the node's position, with fresh ids derived from a seed.
  An input node something feeds is replaced by what feeds it; an unfed input node stays, with its
  example. Exposed parameters carry the node's values. Wires out of the node are reconnected
  from the copies, keeping their ids. Only the version the node pins can be detached; a node
  whose version no longer matches must be upgraded first.
- **Each is one undo step**, and the card's context menu offers View workflow (the version,
  read-only), Edit technique (its source canvas, to publish again), Upgrade to the newest version
  and Detach.

Not yet: live updates of technique nodes (`live_actions` plans without inlining, so they wait
for a run). Batch fan-out through a technique node's inputs landed later; see
[2026-10-20-technique-batch.md](2026-10-20-technique-batch.md).

## 2026-10-18 · Selected-node auto-format geometry and ordering (#70)

`canvas.auto_format` is an atomic edit, not a host service mutation: one invertible MoveNode
changeset, no gesture coalescing, and no history/event entry for an unchanged result. A scoped
`LayoutContext` supplies selection and current logical geometry. Engine Context now exposes the
Session revision; project/canvas/revision and exact selection membership are checked before
layout. Custom hosts are trusted to compute actual-instance world metrics at reference zoom 1.0
and must refresh the capability after every document revision. Live `dispatch_layout`/`layout_enabled` build it afresh
and restore any prior capability on success or error. Raw headless sessions without this explicit
capability return Unsupported. No dialogs are opened by command dispatch.

Keep two logical size scalars per actual resolved node alongside the existing checked geometry
cache (at most 10,000 nodes, 80,000 bytes of scalar payload, plus bounded map overhead). Compute
size while borrowing the validated instance descriptor and actual node, including the history
strip after the parameter-height cap, never from large f32 Rect differences.
No new descriptor bodies or instance hooks are retained/performed for formatting. Shared card
metrics include the clipped title/UNSAVED notice, handles and half-stroke at reference zoom 1.0.

Grid groups positive vertical-overlap components, then orders each component by left edge and
NodeId. Tall cards can bridge overlap chains; touching intervals start separate groups. Layered
retains nearest-8 outer y/x keys (ties away from zero), then NodeId. Align output anchor and cell
spans to 8 world points. Positive left/top decoration insets are added to cell origins. On grid
reapplication, recover a lattice row origin only when adding its inset and converting to f32
exactly reproduces the stored body y. The legal coordinate domain has f32 ULP at most 1, smaller
than the 8-point lattice, so this recovers emitted rows even when tiny positive heights are below
one ULP. Other input rows retain their actual interval starts. Camera zoom has no effect on
persistent arrangement. Reject host metrics with an outer span above 1,000,000 points and reject all output when any body
position exceeds ±10,000,000; never clamp individual results. Reapplication assumes unchanged
logical geometry/topology, rather than pretending a position-dependent custom kind cannot
change its own size. Whole-DAG longest-path ranking uses a stable ordered ready set in
O((V+E) log V), avoiding repeated transitive searches. Only selected ranks are placed; obstacles
and edge-crossing optimization are outside this contract.

### Layout availability with persistent admission

Layout Preview checks editability, parameters, fresh geometry and exact selection, then returns a non-document effect. It deliberately does not solve or stage the document: final output bounds, history budgets and frozen-format replay admission remain dispatch checks. Dispatch returns one ordinary Edit through the unchanged private Prepared/journal boundary. A latched recovery journal disables layout before either phase. Automation classifies this reversible command, while raw headless callers still need explicit trusted geometry.

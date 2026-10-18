## 2026-10-17 · Router node (#72)

- **`structure.router` is a node kind with no work of its own:** one list input and one single
  output, in a modality chosen per instance (`config["modality"]`, `text` by default), so it can
  feed any input of that modality. Quick-add `R`. Renaming uses the node title.
- **Routers are resolved away in one place, `exec::routing::incoming`:** every consumer's feeds
  through routers (and chains of routers) become the routers' own sources, in order. The planner
  and staleness both read inputs through it, so a router adds no step, no cost and no hash, and
  editing what feeds a router makes its consumers stale.
- **What a router can't hide is blocked with a reason:** a single input fed several values, or a
  list, through a router; and an input wired only to routers with nothing behind them (it isn't
  treated as unconnected, which could silently pick another mode). Routers in a run request are
  dropped (selecting only routers is "nothing to run").
- **Resolution is linear and bounded:** each router's sources are resolved once, routers before
  the routers they feed (Kahn's order), each list capped one past the list limit (so the planner
  still reports an oversize fan-in), and at most 4M entries in total; past that, resolution is
  refused. No recursion, so long router chains can't overflow the stack.


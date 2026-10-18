# 2026-10-17 · Router validation (#216)

- **Resolution uses the installed registry.** Each router instance is validated once against
  its configuration and stored ports before it can act as a passthrough. A bounded, fixed-size
  reason and the first invalid router identity travel through router chains to each affected
  consumer port. Valid feeds on that port cannot hide an invalid branch; unrelated selected
  nodes can still plan. Original wired ports are indexed once alongside incoming edges, so an
  empty router remains distinct from an unconnected input without rescanning edges per port.
- **Unknown is not fresh.** An invalid-router-dependent node cannot produce an input hash and
  remains unknown under the existing staleness contract. The stale set lists known changed
  results, not proof of freshness; callers needing that distinction use `input_hash`. Global
  resolution budget failures return errors from both `staleness::stale` and `canvas.stale`,
  without changing the document or undo history. This does not change hash bytes or versions.

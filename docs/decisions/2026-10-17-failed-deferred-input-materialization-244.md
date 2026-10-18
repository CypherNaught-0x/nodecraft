# 2026-10-17 · Failed deferred input materialization (#244)

- **Preflight the whole deferred step before copying its inputs.** Borrowed source slices check
  availability, cardinality, list counts and checked byte totals first. A late invalid input or
  limit refusal therefore copies no accepted prefix and consumes no cumulative copy allowance.
  Splicing does not allocate a temporary list of references. Runtime logical-size accounting
  matches the planner, using a depth-bounded iterator stack and document depth/item limits
  before recursive clones. Sources cannot change between the preflight and copy passes in
  the synchronous executor.
- **Admitted copies remain charged when planning fails.** Once the complete input copy is
  admitted, its bytes count toward `RunLimits::run_bytes` even if the node kind subsequently
  refuses to plan. Independent steps may use only the remaining allowance; existing per-step
  and per-list limits, ordering and failure propagation remain in force.

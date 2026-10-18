# 2026-10-17 · Text action schema and split cardinality (#87, #162)

- **The normative Text inventory lives in `text-actions.md` until #162 supplies the shared
  descriptor contract.** It specifies stable parameter identities, controls, active branches,
  wire defaults and input/output cardinality without introducing a parallel schema runtime.
  Future generated adapters must preserve that contract and its wire fixtures.
- **Split requests persist `max_parts`, defaulting to 16384 for legacy requests.** Explicit
  values are positive and cannot exceed the hard ceiling. Execution intersects this setting
  with the validated host item budget, including a zero host budget; exceeding either returns
  an error without partial output. The setting is an output ceiling, not `splitn` truncation.

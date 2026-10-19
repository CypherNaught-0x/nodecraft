## 2026-10-19 · Every kind plans and runs through its contract's resolution (#162)

- **One resolution path.** The default `NodeKind::validate_instance` resolves the instance
  contract (`contract(descriptor)`, validated) instead of `ParamSchema::resolve`, and returns
  `Resolution::into_resolved(ready)`. The planner, actions, routing, paste, migration and
  staleness already go through `validate_instance`, so each sees the same values; jobs receive
  exactly `submission()`. `generation.image` uses the same conversion, so a model parameter
  declared `identity: false` is now still submitted (before, it was silently dropped).
- **Edits are lenient, runs are strict.** Invalid and unknown values refuse at once, as
  `ParamSchema::resolve` did. Missing required values and failed checks refuse only when `ready`,
  so `param.set` can pass through an intermediate state (one bound raised before the other).
- **Random seeds are refused, not dropped.** `submission()` sends nothing for a `random_seed`
  parameter; running that silently would be an unrecorded, irreproducible paid run. Until seeds
  are recorded with their generation, a ready resolution with an enabled random seed is an
  `unsupported` error; a fixed seed runs.
- **Same admission as before.** `Contract::resolve` counted every schema default in its text
  budget even when a stored value replaced it, refusing a stored 600 KB prompt over a 600 KB
  default that `ParamSchema::resolve` accepted. Both contract resolvers now count only the
  defaults that apply.
- **Staleness hashes the submission.** Identity is still the visible-value hash rather than
  `Resolution::identity()`, so `identity: false` parameters remain hashed (stale too often, never
  too rarely) until the stale hasher reads identity from the contract.

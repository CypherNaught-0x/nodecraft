## 2026-10-18 · Run commands: quote, start within budget, record (#201, part 1)

- **`run.quote` prices a request without changing anything.** It plans the request and prices it
  against the catalog. The response has:
  - the first 500 steps, with each step's price or why it has none;
  - totals per currency, which always cover the whole run;
  - the budget blocks;
  - the provisional and estimated steps (job counts known only at run time);
  - the planner's reasons when the request can't run yet;
  - a quote `token`.
- **The v2 token binds consent to the actual live Session (#294).** The original revision/price
  digest was insufficient for copied projects. Each Session now mints a private fallible32-byte
  nonce and checked source generation. Every document/asset admission invalidation advances that
  generation, preflighted before append/storage effects; import and conservative save/GC invalidate
  even without a document revision change. Reconstructing identical ProjectIds/content gets fresh
  authority. The immutable `RunCatalog::new` snapshot has its own nonce; Clone preserves it,
  replacing the snapshot invalidates even equal-price/provider mappings. These nonces are never
  serialized, user identities, or caller-supplied values.
- **Canonical bounded request binding** uses explicit version/mode tags and counted sorted unique
  UUIDs (only fields used by that mode), with a checked256KiB binding-input cap. No whole-project
  serialization/hash, Debug representation, opaque implementation inventory or quote cache is
  added. This authority relies on immutable Session-owned definitions and trusted pure NodeKind
  callbacks; hidden interior mutation behind a live definition is unsupported. Definition updates
  need a fresh supported Session/source identity.
- **Host contract for the `run.start` service.** Enqueue the request during the service callback;
  before reservation/execution, call authoritative `run_cmds::price(&actual_session, request)` and
  verify token, live allowed/partial state and every accepted currency ceiling. The former API
  taking arbitrary Project/revision/registry/catalog no longer mints launch authority. Reserve
  atomically against the live ledger, then freeze the verified document/plan/asset references and
  retain the SAME kind/catalog definition owner before releasing the Session borrow/scheduling.
  Do not later re-fetch a different Session or registry for deferred jobs. Deferred costs remain
  partial and subject to accepted ceilings/job-time budgets. This is a trusted host boundary,
  not durable execution/idempotency or a proof that a confirmation UI was displayed. Live budget
  changes recheck without changing consent identity. The desktop async host remains future work.
- **`run.record` is host-only.** It records a run's results (generations with asset references,
  up to 1,000 per call) as one undo step. MCP lists it as human-only, so agents can't fabricate
  results.
- **`usage.export_csv` pages** (up to 2,000 rows per call), so a long usage log stays exportable
  within the command result limits.
- **Pricing inputs are host resources:** the catalog (`RunCatalog`), the caps with what has been
  spent (`BudgetState`), and the usage log (`UsageSnapshot`). Without them the commands are
  unsupported. Staleness is computed only for the modes that use it.
- **Not yet done:** the desktop host. It needs an async runtime, a provider registry, the run
  service, budget settings and the cost-confirmation UI on generation nodes (#83, #84).

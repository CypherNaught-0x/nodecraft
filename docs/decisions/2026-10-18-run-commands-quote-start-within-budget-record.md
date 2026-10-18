## 2026-10-18 · Run commands: quote, start within budget, record (#201, part 1)

- **`run.quote` prices a request without changing anything.** It plans the request and prices it
  against the catalog. The response has:
  - the first 500 steps, with each step's price or why it has none;
  - totals per currency, which always cover the whole run;
  - the budget blocks;
  - the provisional and estimated steps (job counts known only at run time);
  - the planner's reasons when the request can't run yet;
  - a quote `token`.
- **The token binds a run to the quote that was accepted.** It is a digest of the document
  revision, the request, the plan's steps and their prices. `run.start` requires the token of the
  quote the caller accepted, and amounts that cover every currency's total. It also requires
  `partial: true` when part of the price isn't known: unpriced steps, or job counts known only at
  run time, which could cost more than quoted. Any edit since the quote changes the token, so the
  caller has to quote again. No caller (UI, CLI, agent) can start a paid run without having been
  shown its current price.
- **Host contract for the `run.start` service.** The request carries the run parameters, the
  revision, the token and what was accepted. Before running, the host prices the request again
  with `run_cmds::price` on the document as it is then, and refuses if the token differs. It
  reserves the estimate in the budget ledger when the run starts (job-time budgets, #222), so two
  quick starts can't both pass the same headroom, and holds estimated steps to the accepted
  amounts.
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

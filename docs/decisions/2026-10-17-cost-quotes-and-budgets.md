## 2026-10-17 · Cost quotes and budgets (#76)

- **Pricing lives in `cost::quote`.** A node is paid when its kind has a model parameter
  (`ParamControl::Model`); its price is the sum of the catalog models its model parameters name
  (each priced from its own parameters via `model_params`, which provider requests must use too)
  times the jobs the step plans. A paid step whose jobs are planned later is quoted as one job and
  flagged as an estimate (`Quote::estimated`, so the total is not "complete"); the driver checks
  budgets again before each job (#207). Kinds without a model parameter
  (sources, local actions) are free, except generation kinds: one with no model to price is
  unpriced, never free (#219). Category is a stand-in signal: local generation kinds (a ComfyUI
  workflow) will need to declare that they are free, or capped runs with them will be blocked. Totals are kept per currency and never converted.
- **Caps are conservative.** A capped run is blocked when any step's price is unknown (no model,
  model not in the catalog, usage-based pricing without a count) or is in a different currency
  from the cap; spend in other currencies doesn't count towards a cap. The per-call cap applies
  to each job; session and project caps to what was already spent plus the run. Amounts equal to
  a cap are allowed, and a run that costs nothing is never blocked.
- **The usage log counts a charge once per provider and idempotency key** (empty keys are refused), keeps the
  newest 100,000 entries while folding evicted spend into running totals so caps never loosen,
  tracks sessions by id, and exports CSV that spreadsheets can't read as formulas.
- **Totals are kept as charges arrive and saved with the log** (#219). `spent()` can't fail, a
  charge over a million units is refused, and nothing changes on an error. A saved log keeps its
  totals (`UsageLogFile`), so reopening a project never forgets evicted spend. Loading checks
  that the totals cover every listed charge.
- **Charges have a status** (`Charge`: actual, estimate, unresolved). `settle` moves an estimate
  to the actual cost, or keeps it as unresolved when a job may have been billed. Totals move by
  the difference; an actual charge is final. Recording the same key again never replaces a
  charge, so a retry can't change what was billed. `release` removes an unsettled charge the provider certainly
  never billed and frees its key. Settling can't change a charge's currency. A saved log's
  project total must equal the sum of its sessions and stay far below overflow.


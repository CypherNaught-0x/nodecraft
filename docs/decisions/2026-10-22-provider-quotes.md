## 2026-10-22 · Providers quote by-usage jobs before a run (#663)

- **Quotes are a separate, free provider call.** `Provider::quote` takes the job the run would
  send, with an empty idempotency key, and is "unsupported" unless an adapter overrides it. An
  estimate never uses up a key the run will send and never registers a charge.
- **Quotes are keyed by the planned job, not the sent one.** A run's id is random and seeds are
  drawn from it when jobs are handed out, so the body a run sends can't be known before it starts.
  A quote is kept under a hash of the planner's `JobSpec` (model, parameters, inputs, endpoint)
  and is found again for the same document state. The provider is asked about the handed-out work
  of a throwaway run, so what it prices matches what will be sent apart from the seed.
- **A quote prices a step only when all its jobs have one.** The step costs the dearest of them per
  job; anything else stays "unknown before execution", with the existing partial-price consent.
- **Quotes belong to the provider routing they were asked under** and are dropped when it changes,
  and after ten minutes on the host's clock, since a plan's credit price may change. Every new or
  dropped quote changes the quote token, so an earlier token is refused.
- **Each request has an id.** `run.estimate` answers `{accepted, request}` and the host's
  `Estimated`/`NotEstimated` notices carry it, so a confirmation acts only on its own answer.
- **Asking sends the run's inputs.** Quoting a job sends what the run would send, so an image input
  is uploaded to the provider when the price is asked, before anyone accepts the run. Nothing is
  charged for it; the desktop confirmation says the inputs are sent while it asks.
- **Upload reuse is the adapter's job.** Higgsfield keeps the URL of an image an estimate uploaded,
  by content hash, for the next submit that sends the same bytes (at most 32 kept, the oldest
  dropped first). The adapter has no clock, so a kept URL has no age limit yet.
- **Only a host that pumps between runs offers `run.estimate`** (`RunHost::register_estimates`):
  the desktop app does. The CLI and MCP server don't register it yet, so the command is refused
  there and their runs keep the unknown price.

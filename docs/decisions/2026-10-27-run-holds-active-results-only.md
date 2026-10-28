## 2026-10-27 · A run carries only active results and holds a bounded amount of output (#254)

A `Run` keeps its own copy of the canvas it was planned on. That copy now keeps only each node's
active generation (`Canvas::active_only`, or `into_active_only` for an owned copy;
`nodecraft_exec::run::snapshot` picks one). The engine's prepared and estimated runs are admitted
on that cut-down copy too, so a canvas with long histories is no longer copied whole, up to the
project size limit, for every run. Everything a run reads is the active result: upstream values
for input hashes, the partial result a fanned step completes, and its node's ports. Its hashes
therefore match `stale()` on the live canvas. Anything that needs an inactive generation must read
the live document, as `record_changes` already does with the live node. It must not read the run's
copy.

The job outputs a run holds are bounded by `RunLimits::held_bytes` (default
`MAX_HELD_OUTPUT_BYTES`, 256 MiB of JSON). That counts outputs waiting for their step to assemble
them and finished steps' outputs kept for later steps. Media are asset references, so the count is
mostly text and data. A result that would go over fails its step with a reason that says to run
fewer nodes together or make the results smaller. It is not stored. The host already holds a
failed step's paid results (#369, #611), so nothing paid is lost and nothing is sent again. Local
jobs' outputs waiting for the host (`run::Outputs`) are held to the same total. Over it, the local
job fails instead of finishing. Local jobs cost nothing.

Durable identity for resuming a paid job was the third follow-up. The run journal (#508, #664)
already covers it. It saves each submitted job's idempotency key and provider handle. On resume
the job is polled from that handle, or a staged request is sent again under the saved key, so it
never needs the original run id or first job id.

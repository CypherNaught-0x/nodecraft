## 2026-10-27 · A run carries only active results and keeps a bounded amount of output (#254)

A `Run` keeps its own copy of the canvas it was planned on. That copy now keeps only each node's
active generation (`Canvas::active_only`, or `into_active_only` for an owned copy;
`nodecraft_exec::run::snapshot` picks one). The engine's prepared and estimated runs are admitted
on that cut-down copy too, so a run of a plain canvas never copies the inactive history. A run of
a canvas holding technique nodes still copies the canvas whole for a moment: `Canvas::inlined`
clones it to add the inlined nodes. That copy is then cut down in place, and the run keeps the
cut-down copy.

Everything a run reads is the active result: upstream values for input hashes, the partial result
a fanned step completes, and its node's ports. Its hashes therefore match `stale()` on the live
canvas. Anything that needs an inactive generation must read the live document, as
`record_changes` already does with the live node. It must not read the run's copy.

The job outputs a run keeps are bounded by `RunLimits::held_bytes`. The default is
`MAX_HELD_OUTPUT_BYTES`, 256 MiB of JSON, and admission caps a host's value at that. The count
covers outputs waiting for their step to assemble them. It also covers every finished step's
outputs, which stay until the run ends because later steps and their input hashes read them. So in
effect the limit bounds what a whole run produces, not only what it holds at one moment. Outputs
that are used or let go (a failed step, item or variation) leave the count. Media are asset
references, so the count is mostly text and data.

A result that would go over fails its step with a reason that says to run fewer nodes together or
make the results smaller. It is not stored. The host already holds a failed step's paid results
(#369, #611), so nothing paid is lost and nothing is sent again. A host can set other limits with
`RunHost::with_run_limits`.

Local jobs' outputs waiting for the host (`run::Outputs`) are held to the same total. Over it, the
local job fails instead of finishing. Local jobs cost nothing. The host takes a local job's
outputs before it imports what the job staged, so a failed import leaves nothing counted.

Durable identity for resuming a paid job was the third follow-up. The run journal (#508, #664)
already covers it. It saves each submitted job's idempotency key and provider handle. On resume
the job is polled from that handle, or a staged request is sent again under the saved key, so it
never needs the original run id or first job id.

Surfacing the input-hash diagnostics is only partly done. `canvas.stale` returns the first
unknown result's node, cause and reason (#328), which the CLI, MCP and agents see. The desktop
stale badges read only the stale set, and a run records a result whose inputs can't be hashed
without a hash ("unknown, never stale"). Showing that reason on the card is left to a UI change.

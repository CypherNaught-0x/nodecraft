## 2026-10-17 · Run planner (#73)

- **The planner plans nodes, not only jobs.** `NodeKind::plan` needs concrete input values, but a
  node fed by another step of the same run has none until that step finishes. A `RunPlan` is
  therefore a list of `Step`s in dependency order, each with its inputs (an existing value, or a
  named output of an earlier step) and its `JobSpec`s when every input is already known; the
  driver plans the rest as their upstream steps finish. The scheduler (#74) receives job
  dependencies derived from the steps' `after` lists.
- **Source nodes are resolved inline.** An upstream source outside the run contributes its content
  directly (its `plan` is free and deterministic), so *Run selected* never blocks on an unrun
  text or image source; a source only becomes a step when it is requested itself.
- **Staleness is an input.** The planner takes the stale set from its caller; computing it from
  input hashes is #75. *Run all stale* plans the stale nodes plus the empty or stale ancestors
  they need, like *Run with upstream*.
- **List inputs are planned as one `Value::List`** per port, in noodle order; single inputs pass
  their one value. A list *output* wired into a list input is **spliced** (its items join the
  list) rather than nested; `InputSource::Step { list: true }` tells the driver to do the same.
- **Staleness is transitive by contract.** *Run with upstream* stops at an ancestor with a fresh
  result, so the stale set handed in must already contain everything downstream of a change.
- **Who mints job ids.** The driver turns steps into scheduler jobs (`nodecraft-exec`'s own
  `scheduler::JobSpec`, distinct from the node-level `nodecraft_nodes::JobSpec`): it mints job ids
  and `nodecraft:<run>:<job>` idempotency keys, and gives every job of a step a dependency on
  every job of the steps in its `after` list. Steps planned later are added as placeholders so a
  failed upstream can skip them and the queue panel can show them.
- **Pricing is not the planner's job.** A `RunPlan` carries no cost; `cost::quote` (#76) prices
  its steps from their nodes' models and parameters, which don't depend on upstream values, so
  multi-step runs can be quoted in full before they start.
- **Planning is bounded** for hostile projects: upstream values are resolved once per output,
  one list input holds at most `MAX_VALUE_ITEMS` items, one step's known inputs at most 64 MiB
  and the whole plan's at most 256 MiB, and at most 256 blockers are reported; past a limit the
  run is blocked with a reason rather than planned.


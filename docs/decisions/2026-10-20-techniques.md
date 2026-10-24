## 2026-10-20 · A published Technique is a frozen canvas of kind `technique` (#93)

- **A published version is a canvas of kind `technique`.** `technique.publish` copies the part
  of a plain canvas that feeds the chosen outputs into a new canvas of kind `technique`, with
  the Technique's metadata (id, version, name, description, category, tags, input and output
  ports, exposed parameters) stored on that canvas. The project file carries it like any other
  canvas; format v1 refuses non-plain canvas kinds, so older readers never misread one.
- **Published versions are immutable.** Every content change goes through the graph's
  `canvas_mut`, which refuses a technique canvas, so no command, paste or plugin can edit a
  published version: publishing again adds version N+1 on a new canvas. A version can only be
  removed whole (`technique.remove`), which leaves a retired stub on the same canvas id: the
  same id, technique and version number with no ports, parameters, nodes or edges. The stub
  keeps the number taken, so a later publish gets the next number and never reuses one an
  instance may have pinned; `technique.list` skips stubs, and undo puts the version back. Runs refuse technique canvases too
  (`run.quote`, `run.start`): a run records results on the canvas it ran, and a frozen canvas
  can't take them. The Technique node (#94) runs a version by instancing it.
- **The source stays a detached, editable graph.** Publishing copies nodes with fresh ids
  derived from a caller seed, keeps only each node's active result as an example (dropping one
  a batch fanned out, since its item tags name the source canvas's batches), and drops tags
  and fields,
  so the working canvas and the published version never share ids or later edits. `source`
  records which canvas a version came from.
- **The subgraph is traced, not picked.** The builder names output (and optional input) ports;
  the version holds every node upstream of an output. An input must have no incoming wires and
  must feed some output. Outputs may keep outgoing wires on the source canvas, so part of a
  larger canvas can be published; only the traced part is copied.
- **Validation before publishing.** `technique.check` (read-only, also used by the builder
  wizard) and `technique.publish` run the same build, including the final validation of the
  frozen canvas, so a selection check accepts is one publish accepts. Unknown nodes and ports
  are reported as problems like everything else; only a missing or non-plain source canvas
  and more than 64 ports are errors. They refuse a selection with no outputs, unknown or mistyped
  ports, duplicate names or endpoints, more than 500 nodes, more than 64 ports, a plan the
  planner blocks (cycles, unconnected required inputs, model/capability mismatches), or more
  than 1000 jobs, counted as each step's planned jobs (a Batch upstream multiplies them). A
  Batch inside the trace that is fed by another node is blocked by the planner like on any
  canvas. `enabled` runs the full build, which is bounded by the node, port and job limits.
  The graph validates the stored metadata again on load: input and output names share one
  namespace, a node is never both an input and an output, every node feeds an output, and
  duplicate `(id, version)` pairs are refused.
- **Ports keep their types.** A port's type is the type of the node port it names, including
  `many`, so a collection output from a Batch is a collection-valued port. How App mode, the
  CLI and MCP feed collection inputs is #94's.
- **Exposed parameters are named, not overridden yet.** A Technique lists the node parameters a
  user may change (they must exist in the node kind's schema). Applying overrides, re-quoting
  and the override allowlist land with the Technique node (#94), which must re-run the same
  job-count and cost checks on the effective settings.
- **Not yet decided:** nested Techniques (a Technique node inside a published version) and
  behaviour when a copied node's kind or model is unavailable are left to #94; today both
  surface as planner blockers at check time.

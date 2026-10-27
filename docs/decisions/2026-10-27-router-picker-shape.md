## 2026-10-27 · Preserve Router source shape in connection and model choices (#949)

A Router stores one scalar output so a later graph edit can connect it before its input is
complete. That declaration does not prove that its resolved sources carry one value. Choosing
from it previously offered scalar-only actions for an upstream list and counted several model
input wires as one. Direct output metadata is unchanged.

Share the planner's existing bounded Kahn resolution with an ephemeral output-metadata resolver.
A list source remains a collection; several scalar sources form a collection. Preserve source
order and duplicate feeds. Validate the installed Router kind and each terminal source's stored
ports. Cache successful terminal validation by node id within that immutable capture, so duplicate
feeds do not repeatedly reconstruct a dynamic workflow descriptor. Keep the duplicate entries
and their order. A missing source, empty Router branch, invalid branch or cycle refuses a concrete output
choice with a reason. A known list stays a list even when empty or not run yet. Do not infer a
single value from unavailable provenance or publish a truncated feed count as exact.

The canvas resolves the dragged output when the gesture starts, and resolves it again when a
connected-node choice is applied. The same engine predicate checks `node.create_connected`, so
CLI and MCP cannot add the scalar-only consumer that the picker excludes. Reverse wiring also
uses the shared shape. Existing graph serialization, Router ports and undo changesets remain.
Model selection captures resolved terminal wires once per target and reuses that capture for all
candidate contracts; card mode counts read the corresponding terminal result metadata. Direct
field option contracts, conditional option scopes and fetched lists are unchanged.

Connection shapes and model wire counts are compatibility metadata, not evidence of paid input
counts. The run planner and quote admission still count actual reference values, including lists,
and keep an unresolved list's paid fee unknown. Offline tests run five Grok edit references
through a Router chain, both as separate sources and as a Folder list: quote and ledger reserve
the full reference fee, and the mocked provider receives all five images only after consent.

See the [Router semantics](2026-10-17-router-node.md),
[Router validation](2026-10-17-router-validation.md) and [UI design](../ui-design.md).

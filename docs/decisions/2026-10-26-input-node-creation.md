## 2026-10-26 · Create compatible upstream nodes from free inputs (#950)

Dragging an unconnected input starts a wire toward a source output. Dropping it on empty canvas
opens the existing keyboard/pointer node chooser, restricted to kinds with a compatible output.
Connected inputs retain their established meaning: pick up the existing wire to reroute it, or
disconnect it by dropping on empty canvas. Escape cancels either gesture without editing.

The shared `node.create_connected` command accepts exactly one existing endpoint. Its original
`from` form creates a downstream consumer; the new `to` form creates an upstream provider. The
command checks the live endpoint and instantiates the selected kind before publishing one
invertible changeset. A single input occupied while the chooser is open is refused, preserving
the intervening connection. List inputs append with checked ordering. Legacy input migration,
when required, belongs to that same changeset.

The engine supplies both candidate filtering and default node instances. Router and Batch
instances carry the connection's modality, and UI wiring delegates to the same compatibility
predicate. Candidate kinds are resolved once when the chooser opens. The shell sizes the exact
default instance so the connected side meets the drop point at any zoom; programmatic callers
continue to supply an explicit node position. No creation command opens a dialog or runs a model.

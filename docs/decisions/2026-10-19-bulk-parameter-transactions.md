## 2026-10-19 · Shared parameter edits are one admitted transaction (#71)

The bulk panel is a generated view of selected instances of one node kind. Parameter names
and labels alone do not establish compatibility: the owning contracts must agree on control
category, units and declared meaning. Model-owned parameters retain their model namespaces;
the node kind namespace is never substituted for them. Untagged fields from different model
owners remain separate even when their labels and schemas happen to match.

Generated controls use the intersection of permitted values. Choice options intersect by
stored value. Numeric bounds intersect; signed and unsigned integer step lattices intersect
with full-width integer arithmetic, retaining singleton domains without floating-point
rounding. Slider steps remain presentation hints. Conditional visibility and enabling are
resolved independently for every target. Missing stored overrides, explicit defaults and Mixed
remain distinct; rendering or focusing a control never writes a default into the document.

`node.params_set_bulk` prepares all selected targets before producing one changeset. A failed
target, stale revision, incompatible field, unresolved contract issue or kind-owned constraint
refuses the entire edit. Hidden or disabled values marked for reset are removed in that same
changeset, so one undo restores both the controlling value and its dependent overrides.
Borrowed target and caller payloads are bounded before cloning or deserialization.

The shell caches bounded view state by Session identity, document revision and exact selected
IDs. Catalog-backed instances additionally require immutable catalog/definition authority;
a resource replacement must invalidate their drafts and admission. The bulk panel consumes
the common model picker and model-swap transaction rather than looping over single-node edits.
Changing a model requires an explicit migration preview and acknowledgement of dropped or
reset settings; an ordinary parameter edit cannot bypass that path.

These boundaries preserve UI/headless command parity and document/history/recovery atomicity.
They deliberately permit fewer fields when model metadata cannot prove compatibility, rather
than guessing semantics from provider labels. See [the descriptor contract](../descriptor-contract.md).

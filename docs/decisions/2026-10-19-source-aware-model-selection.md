## 2026-10-19 · Source-aware model selection (#157)

Single-card and bulk model choices share `node.model_preview` and `node.set_model`. The former
is read-only; the latter consumes its current source/catalog-bound token and applies one complete
changeset. Model editing never acts as paid-run consent. This keeps UI and MCP behavior on the
same mutation path and gives both one Undo/Redo boundary.

Migration uses the actual source and destination model-owned contracts supplied by the immutable
generation snapshot. Node-owned controls stay separate. Names alone do not prove shared meaning:
missing semantics cause explicit drops rather than invented tags. Complete name/reason/reset/default
notices precede Apply, while dropped values stay out of public reports. An unavailable saved
source uses an explicit corrective preview, not a fabricated source contract or automatic model.

The picker keeps bounded IDs/reasons/indices and reuses one immutable snapshot. Actual kind-owned
role/collection checks supersede coarse modality counts where ordered text wires are composed
into one prompt. It does not copy nodes/history for every catalog option, silently remove wires,
or equate an unknown/usage-based price with free. Dirty drafts and closing-frame input ownership
remain with their original UI owner. See [the model selection contract](../model-selection.md).

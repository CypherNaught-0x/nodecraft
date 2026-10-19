# Model selection

The production `generation.image` card opens a provider-grouped model picker. Search matches
catalog IDs, labels and providers. Disabled options remain visible, with a reason and the
model's mode/input-role details. Quality and typical latency are catalog hints. Free, usage-based
and per-call/unit rates remain distinct; these are model rates, not a computed run quote.
Choosing or applying a model never launches a provider or grants paid-run consent.

The controller retains an immutable generation/catalog snapshot. It builds compatibility once
when opened or explicitly refreshed, reuses query results, and virtualizes rows. Closed pickers
do not scan the catalog. Compatibility uses the kind's actual connected roles and collections:
several ordered text wires may form one provider prompt. It does not silently delete references
to make a model fit. Required wired roles must fit; missing executable parameter settings remain
subject to the shared contract and the existing run-admission boundary.

Only the registered production image kind currently owns this transaction. Other catalog
modalities remain visible as unavailable for an image target; this does not implement new video,
audio or document generation kinds. The reusable presentation can serve later kinds only when
they supply their actual model ownership and wiring contract.

## Preview and apply

`node.model_preview` takes `{canvas, nodes, model}`. `nodes` is an exact, nonempty set of unique
IDs in one canvas. Its read-only result contains `binding`, `revision`, `model` and a complete
`targets` list: previous known model (or `source_unavailable`), carried parameter names, dropped names/reasons, conditional
resets, destination defaults and whether each node changes. No values are included in notices.

`node.set_model` takes the same selection plus `expected_revision` and `preview_binding` from
that preview. It checks the live Session execution identity, revision, canvas, target set,
destination and immutable catalog/installed-definition identity before preparing replacements.
A stale preview refuses; it does not silently replan. Both commands are available through the
ordinary MCP `command_run` surface. Raw `node.param_set` selector replacement/removal is refused
for the production generation kind, while custom/fixture Model controls retain their behavior.

All targets use the same source-aware preparation and publish one changeset, one Undo and one
Redo. The current image definitions all have the same image-collection output; every target's
incoming-role validation therefore sees unchanged opposite outputs during a bulk swap. A future
kind with changing output signatures needs complete candidate endpoint-overlay validation
before using this transaction. Validation, report/admission or persistence-append refusal does
not publish a partial node prefix. An uncertain append latches the existing checkpoint-required
state, rather than claiming external storage rollback.

An unavailable saved model is never substituted automatically. Its card offers explicit model
correction. The preview reports every opaque model override as source-unavailable and preserves
node-owned values; only confirmed Apply replaces it, and Undo restores the saved state. Unknown
future configuration versions still refuse instead of being erased during correction.
Saved Model text uses its existing bounded value contract, even when a legacy spelling does not
match today's catalog-ID syntax. Destination choices still require a valid installed catalog ID;
unknown saved selector text is omitted from public preview notices.

Unsaved card edits remain view drafts when opening/canceling the picker. Apply is disabled until
affected drafts are explicitly finished or canceled. A model-opening click cannot submit a
lost-focus parameter intent in the same pass. The picker owns canvas mutation/navigation input
through its closing pass. The bulk host must include its own affected shared drafts in this
same dirty-target predicate; no bulk draft state is implicitly imported into another model.

## Bounds and qualifications

Before migration copies, a borrowed pass checks every selected source payload and the combined
original/replacement upper estimate against the existing 16 MiB serialized replay envelope.
Destination contracts conservatively include all defaults; declared roles conservatively include
new port metadata. The complete report must fit the existing 1 MiB/16,384-item command result
limits. Oversize selections refuse with a smaller-selection explanation; required notices are
not truncated. Graph's existing 10,000-change limit also applies. These are serialized admission
bounds, not measurements of allocator capacity, native callback peaks or process RSS.

Migration's temporary source partition, shared migration result, detached constructor/resolution
and final replacements overlap. Validated catalog definitions have their own generation snapshot
budget, and one targeted definition check may temporarily clone bounded contract metadata. Source
history is preserved as references/data, without reading media bodies in the picker. The UI
retains only bounded candidate IDs/reasons and row/filter indices, borrowing catalog metadata;
temporary per-target port-count captures use the existing 16 MiB view metadata limit and are
dropped before painting. No whole-project clone or provider work is needed for picker filtering.
These independent owners are not represented as a single global memory quota.

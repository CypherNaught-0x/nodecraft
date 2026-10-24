## 2026-10-24 · Last-known server evidence prices older workflow cards (#872)

A workflow card imported before partner names were stored can still contain paid ComfyUI
classes. A successful node-list refresh now retains a bounded set of hashes of partner class
names per provider instance. Quotes and run admission compare the stored workflow against the
last successful list for the actual selected server. Another server's list cannot classify it.
The unnamed compatibility route uses the default server's list.

This is runtime evidence, not a document migration: opening, quoting and refreshing never
rewrite a mapping or add an undo entry. Stored partner names remain conservative evidence.
Failed refreshes keep the previous evidence; editing/removing a server forgets its evidence.
A refresh or forget changes the quote token, including between preparation and authorization.
Unknown-price consent and budget caps therefore apply to older cards just as to new ones.
Without known evidence, a legacy card keeps its previous behavior until its server is refreshed.

Each server retains at most 4,096 fixed-size hashes. A larger partner list conservatively makes
all its workflow costs unknown. At most the configured server limit plus one compatibility
route is retained. The complete node catalogs are not copied into this evidence cache.

An invalid partner class name is omitted from import diagnostics and replaced in the mapping
by the display-only marker `Unidentified partner node`. The import warns, while the credit-cost
evidence survives even when every partner name is malformed.

The instance descriptor cache's 4 MiB budget now counts mapping text, parsed mappings,
descriptors, string/vector capacities and Arc headers. It excludes allocator overhead and
caller-owned clones. An entry above the budget is resolved without retention. The generic
model-rate formatter calls workflow cost dependent on its partner nodes; `comfyui.workflow`
remains excluded from the generic model picker (`cards = false`).

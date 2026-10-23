## 2026-10-23 · A workflow card with ComfyUI partner nodes is quoted as unpriced (#833)

ComfyUI's paid API ("partner") nodes, which `/object_info` flags with `api_node: true`, spend
the server's comfy.org credits. NodeCraft can't price those credits, so a workflow card that
uses partner nodes must not be quoted as free just because its catalog model,
`comfyui.workflow`, is.

- At import, `nodecraft_comfy::discover` lists the workflow's partner nodes. The card's
  `Mapping` stores their distinct class names in `partners`, at most 64. The field is omitted
  when empty, so cards without partner nodes are stored exactly as before.
- `NodeKind::unpriced(node)` lets a kind say that a node spends money no catalog price covers.
  The default answer is "no". Admission adds the answer as an `Unpriced::Invalid` selection, so
  the step's quote is unpriced and the run's total is partial. This is the same path a model
  without a price takes, so budgets and consent treat the card the same way.
- The import warns, and so does the card's Model parameter description in the inspector.

A literal input whose spec can't be a parameter (a FLOAT with min > max, say) no longer
refuses the whole import. It is left unexposed with a warning, and the card keeps the
workflow's value for it.

The instance descriptor is cached by mapping text: a hash finds the entry, and an exact text
comparison confirms it. The cache holds at most 32 entries and 4 MiB of mapping text, so a
UI frame no longer parses up to 512 KiB of JSON per card. If its lock is poisoned, the cache
is skipped.

A NodeCraft build from before this change refuses a mapping that has `partners`, calling it
malformed, because `Mapping` refuses unknown fields. That is the safe failure: an older build
would quote the card as free.

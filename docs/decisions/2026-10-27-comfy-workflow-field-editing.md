## 2026-10-27 · Workflow field edits preserve current values and keep results grouped by media (#947)

`comfy.workflow.mapping` reads a bounded choice list from the card's checked server catalog;
`comfy.workflow.mapping_set` edits an existing card through one changeset. The UI, MCP and
control surfaces share these commands. The mapping binding covers project, canvas, card,
workflow, mapping and server, while current parameter values are merged at Apply. An opaque
session lineage separately prevents a UI draft from applying after opening another document
with reused ids. Unrelated document changes do not discard the draft.

Before a field is hidden, its current value is baked into the embedded workflow. Existing
parameter ids stay stable when labels change. Renamed file and result ports reconnect edges
by workflow target and media type; disappearing wired ports require explicit disconnection.
Previously recorded partner-node evidence is retained when remapping, even if a later catalog
omits that evidence. Applying to a queued or running card is refused so a result cannot arrive
against a different output contract.

The one-list-output-per-media architecture from [the original card](2026-10-23-comfy-workflow-card.md)
remains. Changing list order permutes compatible history; changing its set of media types
requires explicit permission to clear history. Undo restores mapping, values, wires and
history together. A changed mapping marks the active result stale. A no-op preserves it.
Selected saver order is stored in new local and cloud provider handles so polling and resume
can honor it. Legacy handles keep their former ordering. Unidentified cloud results are
retained after identified results, since discarding a paid result would be worse than a
server ordering limitation.

Default imports expose prompts, seeds, steps, CFG and dimensions as well as upload inputs.
Single-line strings named as prompts count too. Compatible core PreviewImage nodes are
converted to SaveImage in the imported copy, making PBR previews retained image results.
Conversion requires both descriptions to have their expected inputs and neither to be an API
partner node. Custom previews remain unchanged. No external workflow fixture is copied.

Optional `output_names` changes only the media list port names. Empty names keep config
version 1; a custom list port writes version 2 so older readers refuse rather than reconstruct
incorrect ports. Storage keys retain their v1 names. Catalog memory stays bounded: the default
catalog plus the last successfully checked named server, whose mapping editor may ask for a
fresh check after another server was checked. Mapping commands never fetch over the network.

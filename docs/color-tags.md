# Colour tags and the tag filter

A node has one optional tag. Select one or more nodes, open a selected card’s context menu, and choose **Tag selected nodes**. Standard colors create a named tag when first used; existing tags and Clear tag are also available. The menu marks a tag applied to the whole selection with a check, or a partially applied tag as mixed. One command creates the definition and tags the entire selection in one undo step. Clearing a tag does not delete its definition.

Each tagged card carries a color marker and a bounded title tooltip. Colors come from theme tokens, with distinct dark/light palettes. The marker and filter outline do not replace selection, error or running indicators. Stored titles are preserved even when their displayed labels are abbreviated.

## Filtering and jumping

Open **Tag filter** on the canvas. Select exact tag IDs, or the Untagged bucket. Multiple choices use OR matching. Matches receive an accent outline; other cards retain readable content and remain editable. Filtering highlights rather than hides document data. Clear tag filter restores ordinary decoration. The panel reports progress while scanning, then its match count. Close tags retains the filter and highlights.

Previous tag and Next tag wrap in stable NodeId order, selecting and framing one node using its actual instance geometry. Enter and Shift+Enter do the same only while the filter owns focus. Navigation requested during a scan waits for that generation to finish. Closing cancels pending navigation; edits, undo/redo and canvas changes invalidate stale scans. No matches disables stepping. Escape closes the owned panel without clearing the current selection. Active editors, popups, drags and marquee selection retain input priority.

Text search remains independent: it still matches linked tag titles as well as other text. Tag-filter highlights do not change search results. Neither changing a filter nor opening it changes selection or frames a card; explicit stepping does. Both panels reserve viewport space when framing. On a tiny viewport or oversized card, a bounded notice explains unavailable complete framing.

## Document and command contract

Persisted Tag is `{id,title,color?}`. Color is an optional string of at most 64 UTF-8 bytes. Known identifiers are red, orange, yellow, green, teal, blue, purple and pink. Missing or unknown colors use a neutral marker. Legacy and custom tag IDs/titles and bounded future identifiers are preserved. A color never determines an ID, and duplicate titles/colors are valid. Older applications which do not preserve unknown fields may discard color metadata when rewriting a file; backward reading does not promise lossless old-version rewriting. Existing same-canvas tag-reference validation remains unchanged.

All command objects and nested definition DTOs reject unknown fields. Persisted Tag tolerates additive fields. Command metadata limits are byte limits even where JSON Schema expresses conservative character limits; runtime validation is authoritative. No separate schema framework is introduced.

| Command | Parameters and effect |
| --- | --- |
| `tag.put` | `{canvas,tag:{id,title,color?}}`; create/update a bounded definition; unchanged metadata is a no-op. |
| `node.tag_set` | `{canvas,nodes:[NodeId],tag:TagId|null,create?:{id,title,color?}}`; 1–10,000 raw IDs, stable dedup, one undo step. `tag` is required. Optional create must match tag and target an absent definition; omit create instead of null. |
| `tag.remove` | `{canvas,tag}`; atomically clear all references and remove the definition; one undo restores both. |
| `canvas.tag_filter_open` | `{}`; open the live panel. |
| `canvas.tag_filter_close` | `{}`; close, retaining filter and selection, cancelling pending jump. |
| `canvas.tag_filter_set` | `{tags:[TagId],untagged:bool}`; at most 10,000 raw IDs, stable dedup, OR matching; empty plus false clears. |
| `canvas.tag_filter_next` / `canvas.tag_filter_previous` | `{}`; step after the current generation completes; no match is unavailable. |

Persistent commands work headlessly with an explicit canvas and nodes. View commands require a scoped live host and fresh viewport/focus context; raw headless Session returns Unsupported and never opens a dialog. View commands do not change the document, revision or undo history. Results contain compact counts/state rather than copied nodes. Invalid requests preserve document/history and do not echo arbitrary titles or JSON.

## Bounds and implementation

`SetNodeTags` changes only IDs. Its inverse stores each old tag ID without cloning node parameters, configuration or generation history. A changeset has an independent aggregate ceiling of 10,000 tag assignments in addition to the outer 10,000 operation limit. The same aggregate validator checks the generated inverse. A full 10,000-node assignment plus first-use PutTag therefore fits the operation-count limit, subject to existing changeset/replay byte budgets.

Command preflight checks raw lists and metadata before additional command-owned copies. Existing authoritative Session preview/staging validation and its bounded document clone remain in place; this does not redesign transactions or promise clone-free enabled preview. Picker rows do not stage transactions to determine eligibility.

In addition to its separate 256-entry presentation-index budget, the tag filter visits at most 256 nodes per rendered frame using BTreeMap range after the last ID. It stores at most the graph’s 10,000 matching node IDs and one pending jump. Presentation order uses the bounded single-line display title followed by exact TagId. On invalidation, an ordered index admits at most 256 labels per frame and drains at most 256 IDs per frame before publication; no full-title sort occurs during dragging. The old safe presentation remains available until publication; the picker/filter lists virtualize visible rows and abbreviate display labels to 128 Unicode characters plus ellipsis. Control characters become spaces for display only. A unique ordinal prefix disambiguates duplicate displayed titles; matching and mutation still use exact persisted IDs. Document values remain intact. These bounds cover this feature’s own state, not arbitrary caller-owned Session data or other subsystems.

Live frontends replace a document through `NodeCraftApp::replace_session`, which reconstructs scoped navigation/search/tag services and invalidates selection, editor, geometry and query caches while preserving theme/settings. Direct assignment to its public Session field is not a supported live-host replacement path: stale tag/search requests are invalidated by the retained identity token, but registration of view services is not implied.

Persistence uses the reviewed v3 bridge (#193): archive version3 and NCRJ0003 include Tag.color and SetNodeTags. Frozen v1/v2 readers reject the newer roles explicitly; both older formats migrate before new appends. The graph/model and codecs are retained from that bridge, including Comments' certified replay and pin budgets. `replace_session` accepts a fresh Session which is not already registered to another canvas host; conflicting named services are rejected without replacing the existing app.

The standard-color ID allocator examines at most the project’s aggregate 10,000 tag IDs on an explicit assignment click. TagId uniqueness is global across canvases, so an active-canvas-only collision check would be insufficient. Palette metadata is never copied from node payloads.

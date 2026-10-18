# Local comments and Note nodes

Comments are local document data. Use **Comments** to pin a discussion on the canvas or on the
single selected node, or right-click a node and choose **Comment on this node**. **C** starts a
canvas pin at the pointer (or canvas center), yielding to text editors, popups and gestures.
**Add Note** creates a normal `structure.note` node with a schema-generated plain-text body.
Notes have no ports and plan no jobs; they do not send text to a provider.

A discussion has a root message and ordered, flat replies. Open a numbered pin or its list entry
to read/edit, reply, resolve/reopen, attach/detach or delete it. Resolved discussions remain stored;
**Show resolved** only changes the view. The active resolved pin remains visible with a checkmark;
open pins show a message count. Bubble tails and a second outline distinguish the active pin
without relying on colour. A pin covered by the panel is displayed beside it with a leader to
its stored anchor. Lists page 16 threads/messages, and long messages have
4096-byte UTF-8-safe reading chunks. Pins stay readable at low zoom; the view paints at most 128
visible pins and the paged list remains the route to every stored discussion. Coincident pins
receive deterministic small screen offsets. Comments do not join node selection or auto-format.
Node search continues to search node fields, including Note text; discussion search is not claimed.

## Editing and retention

Message editing is explicit: **Save message** or Ctrl/Command+Enter makes one engine edit;
**Cancel draft** or Escape in the editor discards that unsaved edit. Leaving focus never saves a
message. A second Escape closes the discussion panel before canvas selection handles Escape.
Save is undoable through normal document history. The discussion editor clears egui's full-text
local undo snapshots after rendering; use Cancel for an unsaved message and document Undo after
Save. A pin drag previews locally, commits once on primary-button release, and Escape cancels it.
Dragging a bubble displayed beside the panel follows the pointer from that visible position.
Its stored anchor moves by the same world-space delta, retaining node attachment. The temporary
display offset remains after release until the panel closes, another thread opens, or the anchor
changes; clicking alone never moves an anchor. Undo/redo restore exact stored coordinates.
Secondary dragging never moves a pin. Node removal detaches its discussions onto the canvas;
undo restores the node and original anchors without copying message bodies into pin metadata.

The shell retains at most 8 drafts with 2 MiB aggregate String **capacity**, including allocations
retained after deleting text. Refusing another draft never evicts a dirty one. Insertion grows
through an exact, checked replacement allocation and rejects oversized pastes rather than
truncating accepted content. Full buffers are copied lazily when editing begins; inactive drafts
render 128-character previews. A focused egui TextEdit still makes transient full-text/layout
copies: clearing its undoer bounds retained history, not its temporary frame allocations. Text
replacement may temporarily hold the old/new draft plus removed selection (each at most1 MiB
plus up to four bytes beyond the content limit for a valid-UTF-8, invalid-length draft); egui adds its bounded active-editor copies. This is not a
zero-copy active editor. The2 MiB retained-draft bound does not include document/history storage,
widget/layout caches, or bounded per-frame editor work.

Drafts capture a held Session identity, project/canvas IDs and document revision. Any different
revision requires explicit reload/review or **Save as new discussion**, even if the final entity
looks unchanged after remove/recreate. Session replacement with identical IDs/revision cannot
reuse the old draft or pin geometry. A missing target leaves an orphan draft that can be resumed,
saved as new or explicitly discarded. Failed reload preserves it. Hidden dirty node/title drafts
also retain their existing behavior; discussion controls yield to unrelated editors and popups.

## Data and command contract

`Comment {id,text,pos,node,replies,resolved}` keeps the legacy root spelling. Missing new fields
mean an unresolved canvas pin with no replies. Replies are `{id,text}`; all root/reply IDs are
unique project-wide. With no node, `pos` is a canvas position; with a node it is an offset from
that node's stored body origin. Both offset and computed world anchor must be finite and within
±10,000,000 points. Node moves that violate this reject atomically. Initial UI attachment uses
actual instance geometry and current zoom-aware decorations; later movement follows stored data.

Root plus replies total at most1 MiB UTF-8 per thread; a reply is at most64 KiB; at most256 replies
per thread. Roots+replies together use the existing10,000 project comment-entity cap. Existing
1 MiB legacy roots and Note bodies remain fully editable. The dispatcher allows1 MiB content plus
1KiB for input keys/IDs; per-message/Note validation still rejects one-byte content overflow.
Result JSON keeps its existing1 MiB limit. Graph256 MiB, changes64 MiB, history16 MiB/4096 steps and
archive/journal budgets remain additional limits, including escaped JSON growth.

| Command | Parameters (IDs are UUID strings) |
| --- | --- |
| `comment.create` | `canvas,id,text,pos,node?` |
| `comment.edit` | `canvas,id,text,reply?` (omit reply for root) |
| `comment.reply` | `canvas,id,reply,text` (reopen resolved threads first) |
| `comment.reply_remove` | `canvas,id,reply` |
| `comment.move` | `canvas,id,pos,node?` (absent/null node means canvas position) |
| `comment.resolve` | `canvas,id,resolved` |
| `comment.remove` | `canvas,id` (whole discussion) |

Each mutation optionally accepts `expected_revision`; the UI always supplies its captured
revision. Trusted immediate headless callers may omit it deliberately. Exact no-op changes
preserve redo/events. Command-local position objects reject unknown keys. Unknown actor/role
fields reject; commands never echo message contents in errors. The shell allocates UUIDv4 IDs with
fallible platform randomness (`getrandom`, Web Crypto on wasm). Comment allocation checks live
roots/replies and retained drafts; Note allocation checks NodeIds independently. Both retry at most
16 collisions and report an actionable error without changing the document/history or discarding
drafts. IDs carry no timestamp or sharing identity. Tags can reuse this shell pattern while keeping
its typed namespace separate; pure graph/types crates continue to accept caller-supplied IDs.

`comment.compose {pos,node?}`, `comment.open {id}` and `canvas.comments_toggle {}` are view-only
commands. `NodeCraftApp::comments_enabled`/`dispatch_comments` install a fresh scoped host
capability and restore previous resources on every result. Raw Session callers without a live
host receive Unsupported; document CRUD is available headlessly. Programmatic calls never open
file dialogs. The shell owns only view/draft state; every accepted mutation uses invertible graph
changes. This is a trusted local-owner contract, without authenticated authors, ACLs or networking.
[Sharing #121](https://github.com/CypherNaught-0x/nodecraft/issues/121) owns future central access
control and identity; a comment-only UI restriction would not secure normal node edits/undo.

## Canonical replay and format versions

Structural node batches touching original or incoming pin relationships use a certified exact replay path. Stage F:A→B
to obtain inverse I. One extra clone of B probes I→A' and obtains actual redo R; require A'==A.
Reuse that probe for R→B and require the same final B and inverse I2==I. Every generated payload
and aggregate pin metadata is bounded incrementally; large inverse bodies are preflighted by
borrowed serialization before copying. F, I and actual R all pass graph, history and journal
record/frame/asset budgets before publication. This preserves valid-final batches such as pinning
a thread to a node before adding that node. It avoids a pessimistic10,000-pin reserve per add.
A per-stage anchor index avoids scanning every comment for every removed node. Move-only paths
retain one stage/no probe clone, as do structural edits with only unrelated comments. Matching
uses both canvas and node IDs and includes AddCanvas, PutComment, SetCommentPin and restored pin
metadata, retaining valid-final batches even when a pin precedes its target. A deterministic10,000-node
test proves unrelated structural removals/replacements use one stage and no extra clone.
The deterministic1000-node/1000-pin test checks stages 1/3,
extra probe clones 0/1 and anchor visits 1000/3000; timing is diagnostic, not a performance guarantee.

Comments owns archive **v2** and journal **NCRJ0002** on the combined canvas/persistence foundation.
This version includes Node.config, scalar Values, threads/anchors/Note and restored pin metadata.
Exact v1 fixtures come from reviewed commit 08586e3319f5577f1ac6f618103bfa4702e39043. V1 archive
and journal roles reject new fields/variants, including when old default fields such as pos are
absent. Harmless unrelated extension objects retain v1 behavior. Legacy recovery checks record
identities against the original archive bytes, then a new Session/checkpoint atomically establishes
v2 before any new append. Failed conversion leaves the old live session and recovery untouched.
A recovered legacy snapshot explicitly reports `legacy`; hosts must checkpoint it before writing.

Tags/Fields are a later, coordinated **v3** integration. Both v1/v2 readers reject Tag.color and
Field.rect/color/collapsed even when null/default, and reject future bulk operations. Archive and
journal writers, including replay admission, apply the same guard to their exact serialized JSON
before emitting bytes; unrelated extension objects keep their existing compatibility. They must
never silently accept/drop a competing expanded v2 model. No active peer Fields/Tags source is
imported here. The later v3 writer must provide lossless v1/v2 migrations and explicit defaults.


## Current-main integration evidence

The v2 writer uses landed persistence admission and recovery, not the original feature-base host.
Frozen v1 guards remain strict (including input_hash); current v2 can durably retain stamped run
results, config and scalar Values. Main's scoped navigation geometry and seeded node/edge IDs stay
available; Comments/Note use distinct fallible UUID allocation. Main clipboard/quick-add behavior
remains, yielding during held discussion drags. Landed Search is retained; Layout from the original feature
foundation is not bundled with Comments. Current review evidence uses a public egui_kittest/wgpu scene with a discussion, reply,
Note, resolved pin and undo in dark and light themes. Captures are attached to PR #194;
generated screenshots are never committed. Pointer tests cover retained pin and input-order
ownership, file hovering, and refusal of file drops and image splitting during a pin drag.


MCP classifies all seven local discussion mutation commands as agent-accessible, using the same
Session validation, undo and journal admission. The three discussion view commands are temporarily
human-only until the live control bridge supplies their scoped host capability. MCP neither lists
nor runs those view commands; custom engine hosts without the capability return Unsupported.
Keys and file commands retain their human-only policy. This does not
add a full-thread inspection endpoint or claim that canvas_inspect returns discussion bodies.

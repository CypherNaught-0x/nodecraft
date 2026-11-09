## 2026-10-27 · Official ComfyUI mappings are pinned data over the ordinary importer (#948)

The official workflow_templates repository is MIT licensed. The audit pins commit
`3c2199b9362457b12b9c60c6cc303501526ebd62`, including its index usage counts, and surveys all
727 workflow files. Curated field choices belong in versioned data rather than node-class
special cases in engine command handlers. Matching must ignore prompt/model values and layout
while rejecting changed graph contracts. A match changes field visibility, names and order;
it never substitutes provider inputs, model choices or prices. Explicit import parameters and
the saved field editor remain authoritative. Unknown or changed workflows keep generic discovery.

Some selector nodes save widget values without declaring their parameter names in the UI file.
For root nodes with no input sockets, a mapping may use the pinned widget ordinal and kind,
but only while discovery reports the exact pinned widget count. Keep the server's label in
this case instead of guessing its parameter name or units. Linked fields, changed counts or
changed types prevent curation.

277 files use embedded UI subgraphs, including the most-used templates. Their public JSON
interfaces are expanded into the same API workflow the existing server-catalog converter reads.
Node ids are namespaced per instance, input/output boundaries are aliases, and saved widget
values retain wire precedence. Seed modes remain metadata: NodeCraft keeps imported seeds fixed
as documented for the original workflow card. Muted/bypassed instances do not activate internal
savers or paid nodes. Ambiguous proxy serialization is refused with an unpack/export instruction.
Expansion is bounded by depth, node count, link hops and the shared JSON allocation budget.

A shared scalar interface must remain one editable control. Compatible free server Primitive
nodes preserve that fanout; missing or paid substitutes are refused. Different unprovided
defaults remain distinct. DynamicCombo descriptors compile bounded option trees, preserving
nested child order from the descriptor. The selected literal option inserts its active widgets
into conversion at that position. Child request names use dotted paths, as demonstrated by the
[public API reproduction](https://github.com/Comfy-Org/ComfyUI_frontend/issues/20068). Active
children retain their own types, choices and bounds; inactive or unknown paths are refused.
Positional mismatches never shift a model option into the next seed or other input. Unsupported
schemas stay unsupported rather than being partially compiled.

Some pinned UI exports contain one obsolete trailing widget and an explicit named-widget map.
That suffix may be omitted only when the named map matches every active typed widget and value
exactly, with exactly one undeclared scalar matching that trailing positional value. It cannot
replace active values, links or seed modes. Stale/conflicting names, additional widgets or an
unproven positional mapping keep the API-export refusal. This does not invent a server input.

Explicit card output selection retains the complete upstream dependency closure before request
validation. Disconnected helpers and unselected custom/paid output branches are not scheduled;
every reachable required input still must validate. Traversal uses bounded parsed links, including
dotted DynamicCombo/Autogrow references, and never guesses references inside literal arrays.
The imported stored workflow remains intact for later mapping edits; generic workflow checks
without explicit result selection remain strict.
The card may still carry parameters/files for the discarded branches. The provider ignores
only declared, valid scalar targets and declared upload roles on node IDs actually removed
from the admitted original workflow, before
applying overrides or inspecting/uploading media. Unknown original targets, retained targets and
required ancestors still undergo the normal checks. This preserves saved mappings without
letting unused uploads block or leak into a selected request.

Connection-only V3 Autogrow groups retain one bounded template in the catalog; validation
checks the provided dotted ports without allocating one descriptor per possible socket.
MatchType ports are polymorphic. Frontend primitive values feeding connection-only ports use
one compatible free typed Primitive node per immutable source id, including fanout and saved
seed modes; missing, paid or incompatible substitutes are refused instead of dropping values.

Only mappings ship in the product. Survey metadata and test fixtures derived from the pinned
MIT files retain attribution and the MIT notice. No reference prompts, images, videos or ComfyUI
implementation code are copied into product assets. Fixtures replace example prompts with
original synthetic text; the source Git blob ids and deterministic structural audit preserve
provenance. Tests exercise the same conversion and import commands with recorded descriptor
facts and clearly separated original mock catalogs. Import success alone does not establish
that a template can be submitted; mapped templates also need request-validation coverage.

Offline evidence is layered: 42 observed descriptor fact classes, 13 separately synthetic core
classes, and 20 current-documentation projections. The pinned resize selector subset is separate
because current documentation spelling differs from the saved MIT graph. All ten profiles pass
conversion, selection, bounded selected-request checks and command import/save-reopen under this
explicit projection; that is not an exact installed-server contract or live acceptance proof.
Live-instance validation remains the user's hard merge blocker.

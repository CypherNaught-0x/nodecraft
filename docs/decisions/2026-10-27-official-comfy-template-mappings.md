## 2026-10-27 · Official ComfyUI mappings are pinned data over the ordinary importer (#948)

The official workflow_templates repository is MIT licensed. The audit pins commit
`3c2199b9362457b12b9c60c6cc303501526ebd62`, including its index usage counts, and surveys all
727 workflow files. Curated field choices belong in versioned data rather than node-class
special cases in engine command handlers. Matching must ignore prompt/model values and layout
while rejecting changed graph contracts. A match changes field visibility, names and order;
it never substitutes provider inputs, model choices or prices. Explicit import parameters and
the saved field editor remain authoritative. Unknown or changed workflows keep generic discovery.

277 files use embedded UI subgraphs, including the most-used templates. Their public JSON
interfaces are expanded into the same API workflow the existing server-catalog converter reads.
Node ids are namespaced per instance, input/output boundaries are aliases, and saved widget
values retain wire precedence. Seed modes remain metadata: NodeCraft keeps imported seeds fixed
as documented for the original workflow card. Muted/bypassed instances do not activate internal
savers or paid nodes. Ambiguous proxy serialization is refused with an unpack/export instruction.
Expansion is bounded by depth, node count, link hops and the shared JSON allocation budget.

A shared scalar interface must remain one editable control. Compatible free server Primitive
nodes preserve that fanout; missing or paid substitutes are refused. Different unprovided
defaults remain distinct. New V3 dynamic widget groups need protocol-aware conversion and
submission checks before they can be marked supported; positional mismatches must never shift
a model option into the next seed or other input.

Only mappings ship in the product. Survey metadata and test fixtures derived from the pinned
MIT files retain attribution and the MIT notice. No reference prompts, images, videos or ComfyUI
implementation code are copied into product assets. Fixtures replace example prompts with
original synthetic text; the source Git blob ids and deterministic structural audit preserve
provenance. Tests exercise the same conversion and import commands with recorded descriptor
facts and clearly separated original mock catalogs. Import success alone does not establish
that a template can be submitted; mapped templates also need request-validation coverage.

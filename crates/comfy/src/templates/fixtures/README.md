# Pinned ComfyUI template fixtures (#948)

Source: [Comfy-Org/workflow_templates](https://github.com/Comfy-Org/workflow_templates/tree/3c2199b9362457b12b9c60c6cc303501526ebd62/templates),
commit `3c2199b9362457b12b9c60c6cc303501526ebd62`, MIT; the unchanged notice is in `LICENSE`.
The matching profile in `assets/comfy/template-mappings.v1.json` records each original Git blob
id. Original blobs were checked against their Git SHA-1 (`blob <length>\0<bytes>`) and tree size.

The ten workflow JSON files here are **redacted structural derivatives**, not executable
examples for a real account. Node classes/ids, modes, port order, links, interface names and
saved-widget counts are retained. Titles, positions, groups and help notes are omitted; note
widgets are blank. Prompt widgets use original NodeCraft test text. Model and media filenames
use `fixture-model.safetensors` and `fixture.png`; saver prefixes use `NodeCraft`. No example
images, videos or reference prompts are included. Layout and widget values are intentionally
excluded from structural matching, so the same tests can match the original pinned contracts
without bundling creative text or model assets.

`catalog-facts.json` contains minimal public input/output descriptor facts observed through
read-only `GET /object_info/{node_class}` on 2026-10-27. It omits help text, string defaults,
hidden authentication fields, server/device data, and private installed model/media options.
These are protocol facts, not ComfyUI implementation code. An absent class is not proof that a
server does not implement it: the remaining descriptor requests timed out.

`mock-core-nodes.json` is an original **synthetic** test catalog for missing core descriptors.
It is kept separate from recorded facts. Tests use this union to verify conversion, mapping,
preserved values and connections; they do not prove a particular live installation can run all
templates. The six additional candidate profiles, including the popular LTX, Qwen and Krea templates,
still require complete descriptor and request-contract verification;
unsupported dynamic variants are refused. No test submits a generation request or spends provider credits.

Live-instance validation is an explicit merge blocker requested by the user. These offline
fixtures and successful CI runs cannot satisfy that requirement on their own.

The four image/audio/API fixtures also pass the ordinary workflow check used before provider
submission and bounded prompt-request serialization. Connection-only V3 Autogrow descriptors
validate their named or numbered dotted ports using one stored template. MatchType ports remain
polymorphic. Original synthetic cases additionally cover typed frontend primitive fanout,
preserved seed modes, invalid names/types, broken links and refusal of paid substitutes.
Original synthetic DynamicCombo tests exercise nested selected branches, dotted API inputs,
linked siblings, following seed values, inactive/missing children and malformed metadata.
Recorded BlockSparseAttention metadata verifies the selected child range. The provider mock
checks the actual generation POST and proves invalid child values never reach it. The public
API reproduction [ComfyUI_frontend#20068](https://github.com/Comfy-Org/ComfyUI_frontend/issues/20068)
provides independent evidence for dotted request paths; no ComfyUI implementation code was read.
All ten profiles now have client conversion, field selection, dependency-closure validation and
bounded request serialization checks under the explicitly combined projections below. Engine
tests also import each through the shared command and save/reopen its card. Complete observed
video contracts remain unavailable; none of these checks establish acceptance by a live server.

The production manifest stores structural contracts and field/result selectors only. A changed
contract or incompatible discovered field prevents curation and leaves generic discovery intact.
Explicit import `params` bypasses curation. A saved card uses its own serialized mapping, so
updating the bundled data cannot rewrite an existing project's field choices.

`documented-nodes.json` is an original, documentation-backed **projected** test catalog for
20 otherwise missing classes. It records public interface facts (names, types, selector values
and documented bounds), with source URLs, document hashes and available source fingerprints in
`documented-provenance.json`. No implementation, documentation prose, reference prompts or
system instructions were copied. Widget order and available saved selector values are checked
against the pinned MIT workflow structure. This projection is neither recorded `/object_info`
nor proof that a particular installed version has the same descriptor: current documentation
and the pinned templates can differ. Remaining mismatches must stay explicit. These fixtures
cannot clear the live-instance merge blocker.

`pinned-selector-facts.json` is a separate, **partial** two-branch projection for the resize
node in the pinned LTX graph. Current docs name these selectors `SCALE_DIMENSIONS` and
`SCALE_LONGER_DIMENSION`, while the verified MIT graph saves `scale dimensions` and
`scale longer dimension`. This file combines only those observed saved spellings with the
documented child names/types/bounds; its provenance records the pin and exact source blob.
It is not recorded server metadata and does not claim any other resize option or installed
version. A negative test retains the incompatible current-documentation spelling. The original
four-fixture catalog remains recorded facts plus the separate synthetic core catalog; the
ten-candidate projection test adds the documentation and explicitly pinned subset.

Card request checks select the mapped outputs and their complete upstream dependencies first,
just as the provider does. Disconnected sizing helpers and unselected custom comparison savers
are not scheduled. Missing or invalid required ancestors still fail before uploads or prompt
submission. The stored imported workflow remains intact for subsequent mapping edits.
An engine-generated job can still carry saved parameters and wired files for a removed branch.
The provider skips only valid declared targets on nodes actually pruned from that original card;
unknown targets and all
retained values/uploads still validate. An import/plan/provider regression covers an unused
prompt and image loader, verifies zero unused uploads, and refuses unknown or invalid required
targets before prompt submission.

The same pinned LTX source explicitly names a trailing legacy SaveVideo `codec` widget
alongside `format.codec`. It is not projected as a current server input. Compatibility omits
exactly that one scalar only when the saved named map matches every active typed widget/value
and its sole undeclared entry matches the positional suffix. Stale names, different values,
missing active fields and multiple extras still refuse conversion. Synthetic tests also retain
seed modes; no named value can overwrite a current value or link.

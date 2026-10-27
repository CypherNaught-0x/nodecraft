# Pinned ComfyUI template fixtures (#948)

Source: [Comfy-Org/workflow_templates](https://github.com/Comfy-Org/workflow_templates/tree/3c2199b9362457b12b9c60c6cc303501526ebd62/templates),
commit `3c2199b9362457b12b9c60c6cc303501526ebd62`, MIT; the unchanged notice is in `LICENSE`.
The matching profile in `assets/comfy/template-mappings.v1.json` records each original Git blob
id. Original blobs were checked against their Git SHA-1 (`blob <length>\0<bytes>`) and tree size.

The seven workflow JSON files here are **redacted structural derivatives**, not executable
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
templates. Dynamic widget groups and the three video profiles still require further protocol
and schema verification. No test submits a generation request or spends provider credits.

Live-instance validation is an explicit merge blocker requested by the user. These offline
fixtures and successful CI runs cannot satisfy that requirement on their own.

The production manifest stores structural contracts and field/result selectors only. A changed
contract or incompatible discovered field prevents curation and leaves generic discovery intact.
Explicit import `params` bypasses curation. A saved card uses its own serialized mapping, so
updating the bundled data cannot rewrite an existing project's field choices.

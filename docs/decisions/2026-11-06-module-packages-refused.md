## 2026-11-06 · Module packages stay refused (#1071)

The [plugin RFC](../plugins-rfc.md) left executable (`rust_wasm`) packages to a later phase, gated
on a feasibility spike (§11, phase C) and a security review before any engine was chosen. That
phase came back with a bounded expression interpreter (#165,
[`2026-10-18-custom-text-expressions.md`](2026-10-18-custom-text-expressions.md)), which
`text_actions` packages now use, and provider work went to declarative templates (#174,
`provider_operations`). No Wasm engine was evaluated against the RFC's gates (enforced memory and
work limits, a safe host API with no `unsafe`, native conformance on three OSes, browser
parity or an explicit refusal), and none is in the workspace.

- **The refusal is final for this contract.** A package whose `runtime.kind` is `rust_wasm`
  never installs, on desktop or in the browser, signed or not. The store refuses it with an
  `unsupported` error, `plugin_store::MODULE_REFUSED`, which says NodeCraft runs no code a package
  ships and names the kinds to use instead. `plugin lint` refuses it the same way, so an author
  learns before publishing.
- **The kind stays in the wire format.** `rust_wasm` still parses, so a module package gets this
  precise refusal rather than "unknown runtime kind", and the manifest's shape is unchanged for
  anything built against plugin wire version 1.
- **What it supersedes.** The line in `2026-10-18-custom-text-expressions.md` that heavier logic
  "can still come later through the plugin work (#154/#173)" no longer holds for Wasm modules:
  #173's executable phase D stays unbuilt until the new decision below.
- **Reopening takes a new decision.** Accepting module packages needs a new runtime-selection
  issue that runs the RFC's §11 spike with at most two safe-API engines, passes its gates and a
  separate security review, and replaces this record. Until then, logic belongs in
  `text_actions` packages or in first-party Rust in the workspace.

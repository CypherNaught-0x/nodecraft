## 2026-10-27 · Starter workflows are verified project data, added atomically (#952)

Starter workflows ship as small ordinary `.ncraft` archives in `assets/templates`, with a versioned
index, a BLAKE3 digest, short description and feature tags. Each archive is directly openable and
contains the assets it references. An original Rust data author lives in the engine tests; runtime
loads graph data rather than reconstructing a workflow in product code. These files remain in this
repository; moving the collection to another repository is separate work (#953).

`template.list` describes the shipped dataset. `template.add` adds its canvas and any published
Technique canvases through one ordinary graph changeset and the existing opaque PreparedImports
boundary. Verified assets and document publish together; undo removes the added workflow while
retaining the authority needed by redo. Current work is never replaced. IDs are namespaced per
starter and version. Re-adding that version is refused explicitly; duplicate the added nodes using
the existing copy path when another copy is wanted.

The Techniques panel shows these descriptions and adds a selected starter as a new canvas. It also
shows the descriptions already stored by published/project and library Technique versions. This
is separate from extending the personal Technique library to media or nested Techniques: its
existing persistence contract remains unchanged.

Tests open, validate, execute and round-trip every starter. Provider jobs pass through the existing
MockProvider; its signature-only media are replaced with authored codec-valid fixtures. Native
GPU/FFmpeg actions have an explicit mock-resource profile in portable tests, and a separate offline
native profile verifies their actual operation. Neither profile contacts a server or spends money.
The mapped ComfyUI workflow is authored from core nodes; no official/vendor JSON is copied, and
no live ComfyUI validation is claimed. Live runs require normal user configuration and consent.

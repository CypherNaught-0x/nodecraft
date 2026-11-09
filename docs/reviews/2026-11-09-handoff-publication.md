# Local handoff review and publication · 9 November 2026

The owner supplied `nodecraft-push-handoff.zip` after the cloud session could not write to
GitHub. All six archive checksums passed. The imported branch preserves the 163 commits ending
at `96dc109016396662f0e4475cf0107213d0b89cae`, based on main
`caecbb32c8fdc9b6f9034bc4507f5c1f35f865b6`. Work continues in an isolated worktree; unrelated
primary-checkout edits are outside this integration.

## Review and correction

Existing topic PR discussions and recorded review rounds were read first. The independent
integration review of `caecbb32..96dc1090` found one actionable P2: persistent Add node guidance
used Foreground while floating tools use Middle, obscuring their bottom-left content. Earlier
navigation regressions constructed hosts with guidance disabled, unlike the desktop/browser.

Add node now uses the same below-tools Background policy as canvas navigation. A new real-pointer
regression enables guidance, overlaps the actual Batch items panel, checks tool ownership and
unchanged document/view/item state, then opens quick-add through the exposed part of the button.
It runs in both themes. The pre-fix regression observed the wrong Foreground owner; the final
focused group passed nine tests, including existing guidance, Batch and Shader regressions.
Before/after native wgpu captures are attached to the integration PR rather than committed.

The inspected provider registration/features and browser-worker packaging/lifecycle yielded no
additional concrete integration defect. Existing topic review cycles were not restarted. The
sole verification of this correction and the added live test, now committed as
`96dc1090..50d7687a`, was clean. The integration's two-round review cycle is closed.

## Available live ComfyUI checks

The owner supplied a LAN instance. On 9 November 2026 it reported ComfyUI **0.41.0**, Linux,
Python 3.12.3 and an AMD Radeon 8060S device; its queue was empty before testing. NodeCraft parsed
all **1,551** node descriptors with **zero** skipped descriptors. The pre-upload metadata snapshot
has SHA-256 `71bd25fec072660ce1ebe36a75c86ad865f678d2567a703c8518c0f340831c97`.
The raw response is retained locally because it contains the server's private input inventory;
neither it, the LAN address nor existing user media is published.

The existing opt-in catalog smoke passed. The new ignored
`live_comfyui_model_free_workflow_card` test also passed against that instance in **1.21 s**:

- Encode an original solid 16×16 RGBA fixture and upload it through the workflow-card media role.
- Apply the filename override and select the SaveImage output through ordinary card text inputs.
- Queue stock LoadImage → ImageInvert → SaveImage, then poll and download through the adapter.
- Decode one PNG, check its dimensions and verify every inverted channel within one quantization step.

No model was loaded, no paid API node was queued and no existing user input was opened. The smoke
leaves one small public-safe input/output on the server. Its explicit opt-in and two-minute
deadline keep it outside ordinary unit tests and CI.

### Pinned templates against the observed catalog

A local Rust probe used the committed fixture files and the actual observed catalog through
`ApiWorkflow::convert`, discovery, promoted-field annotation, curation, selected-output closure
and workflow checking. This is metadata compatibility evidence, not request acceptance or generation:

| Profile | Live conversion/curation | Request check with original fixture values |
| --- | --- | --- |
| LTX 2.3 image-to-video | 8 fields, 1 output | Refused; 9 input problems |
| Qwen image edit | 11 fields, 1 output | Refused; 5 input problems |
| Krea 2 | 5 fields, 1 output | Refused; 6 input problems |
| Z Image Turbo | 5 fields, 1 output | Refused; 3 input problems |
| Nano Banana Pro API | 6 fields, 1 output | Admitted structurally; **not queued** |
| SDXL | 7 fields, 1 output | Refused; 1 input problem |
| Stable Audio | 6 fields, 1 output | Refused; 2 input problems |
| MiniMax H3 image/reference-to-video; Wan 2.2 image-to-video | Conversion refused | Saved dynamic selectors did not match the observed contract; API-format export requested |

No fixture values, installed models or paid-node policies were changed to force those checks
green. The original full-template live acceptance requirement of PR #987 remains open.

The Hunyuan core nodes are present, and SaveGLB advertises its MESH/file union. However,
ImageOnlyCheckpointLoader lists neither `hunyuan3d-dit-v2.safetensors` nor
`hunyuan_3d_v2.1.safetensors`. No Hunyuan generation was queued, no weights were downloaded and no
checkpoint hash/viewer result is claimed. PR #1001's live-generation merge blocker remains open.

## Validation and publication limits

The clean-main macOS baseline passed `CARGO_INCREMENTAL=0 cargo xtask ci`: all five gates,
**6,281 native tests**, doctests and wasm checks. Local Rust is **1.99.0**, distinct from the
cloud's recorded 1.97.1. The integrated source at `50d7687a` also passed all five gates:
**6,507 native tests**, 267 configured skips, doctests, layering, 405 decision records and wasm.
Both ignored engine adversarial corpora passed in 129.99 seconds. The PR records real-browser
and web-package checks separately; the historical cloud gate is not substituted for local validation.

All **306** PNGs indexed by the archived 153 file pairs passed their recorded hash and dimension
checks. One pair remains explicitly marked as a different-size semantic comparison. Those
historical pictures are not relabelled as final captures of the later navigation/guidance repairs.

The GitHub connector still rejects writes with HTTP 403; the authenticated owner GitHub CLI
successfully claimed #1152, #959, #1120, #377 and #382. Existing claims and topic PRs are retained;
#1125 is a non-claimable tracker. The integration references partial issues without closing them.
It stays draft while the explicit live ComfyUI blockers apply. Materials/textures (#569) and dense
transformed in-card targets (#1114) retain their separately documented unfinished scope. The
cloud's Linux two-GiB harness measurement is historical evidence, not a repeated macOS measurement.

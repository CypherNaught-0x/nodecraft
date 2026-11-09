# Local medium-task sweep — 9 November 2026

The working result is on `codex/medium-tasks-local` in the primary checkout. The sweep started
from `caecbb32c8fdc9b6f9034bc4507f5c1f35f865b6`, inspected every open medium issue and existing
PR, and continued existing topic histories instead of creating competing implementations.
The separate `nodecraft-templates` repository had no open issues.

GitHub issue/comment writes returned “Resource not accessible by integration”. The owner then
requested local work without issue tracking. No remote issue state, PR state or branch was
changed by this sweep. “Implemented” below describes local code, not a merged GitHub issue.

## Work by issue

| Issue | Local result and remaining limits |
|---|---|
| #1151 Vertex AI chat | Added native Gemini whole/streamed/tool chat through `LlmClient`, shared service-account signing, secret-header auth, signed native turn replay, typed error handling, a current Gemini 3.8 Flash catalog entry and opt-in live smoke. Prices remain unknown; live requests/CORS were not tested. |
| #1152 Additional chat providers | Added Together, Moonshot, Z.AI, MiniMax text, Perplexity, Hyperbolic, Mira and native Cohere. Corrected the decommissioned Hyperbolic route, verified current model references, and retained unknown up-front quotes. Mira uses `mira_model` to avoid the card's reserved `model` control; a full registry/session regression guards this. No paid live calls. |
| #1110 Typed notices | Continued both existing notice histories into one local implementation: levels, dismissal, announcements, idle timing, ownership and correct Escape handling. Updated newer callers to the typed API. |
| #1112 Control states | Continued visible resting/pressed/hover/focus states, including held noodle feedback, with token-based rendering and dark/light evidence. |
| #1119 First-run/empty guidance | Continued empty-canvas hints, visible Add node and provider readiness guidance; fixed stale guidance leaking into view-only sessions. |
| #958 Tag filter | Continued the redesign on the current shared panel API. Query/count publication is atomic during revision changes; title/footer, current top-bar overflow, disabled reasons, OR matching and navigation are covered. |
| #1011 PLY | Integrated the bounded ASCII PLY profile, conversion/import/preview tests and current-main updates. This is not a general binary PLY implementation. |
| #1019 USDZ | Integrated the bounded static USDA-in-USDZ profile, actionable binary-USDC refusals, parser/CRC-bypassing hostile fixtures, import/preview and range fixes. General animated/PBR USD scenes remain outside the documented profile. |
| #1013 Browser mesh worker | Integrated bounded Rust decode/preparation workers and corrected completed-reply/deadline ordering, delayed retries and protocol error preservation. Added real-browser regressions and package inventory checks. |
| #948 ComfyUI mappings | Fixed custom multiline prompt fallback, stable file-port suggestions and generic import when mappings fail. Verified the bytes of ten original pinned MIT workflow blobs. Live server/template validation and the historical unrecorded descriptor-hash recipe remain unresolved acceptance limits. |
| #24 ComfyUI 3D | Corrected image-to-mesh discovery and excluded poster files before local/cloud mesh downloads, with regressions. Real ComfyUI generation and TRELLIS compatibility still need a configured server and separately authorized paid work where applicable. |
| #1120 Persistence stress proof | Completed the resource-authorized 2 GiB native telemetry-harness run: open, save, Save As, forced-quit recovery, startup keep-aside and cancellation. Every sampled write peak was below its inventory bound; painting continued. This was Linux, one measured workload, not a stock-executable/p95 or cross-platform claim. |
| #959 Menu/panel review | Completed the inventory and representative actual opening/control/closing regressions, shared overlap checks at 1000×700 and 1600×900, browser question button flows and folder-import Cancel verification. Fixed all eight review findings and frontmost floating-tool Escape arbitration. The matched capture matrix records exact per-surface evidence and provenance; optional live-service/OS speech/all-data combinations are separate. |
| #1125 UX tracker | Reconciled the tracker locally: 12 of its 17 listed child issues were already closed at the snapshot, while five still appeared open. The local continuation incorporates those remaining UI branches; remote checkboxes were not edited. |
| #569 3D parent | Saved views, geometry formats and browser preparation are represented by the integrated child work. Retained UV/material/texture rendering remains an architectural prerequisite: current media, worker protocol, renderer and resource budgets deliberately bake/drop this information. It is not completed by retaining original input bytes. |

Related easy work includes #1108 focus rings and the existing #1114 enlarged wire/badge/avatar
targets. #1114 remains partial for dense transformed in-card controls; the design choices and
collision/zoom constraints are documented in `ui-design.md`. The Agent name/label/disabled-hover
follow-up from the audit is implemented locally.

## UX findings and repairs

| Finding from the review | Local repair |
|---|---|
| Mask controls clipped at narrow widths/200% | Viewport-bounded width and scrolling preserve normalized painting coordinates. |
| Direct modals lacked named Dialog/modal semantics | A shared helper names all 18 direct modal sites; sampled AccessKit ancestry regressions preserve existing action/key paths. |
| Browser provider settings could not reach Save | The panel body scrolls while its header remains visible; the Save path is unchanged. |
| Long Layer Editor title pushed actions outside the viewport | Actions reserve space; the visible title truncates while the full accessible title/hover remains available. |
| Eight long blocked-run reasons pushed Cancel offscreen | Details scroll inside a bounded viewport; Cancel stays in a separate action row and Enter does not start a paid run. |
| Native Agent names/disabled explanations were incomplete (source finding) | Named mention menu, Endpoint association and disabled Clear/Settings explanations, with native-host regressions. |
| Long global notices hid their actions and Dismiss | Bounded stack and scrollable details keep full text, action and Dismiss reachable; wheel input is owned before canvas panning, and unseen notice timeouts pause. |
| Canvas navigation covered Batch result controls and the Shader prompt | Navigation paints above the canvas root and below floating tools. A minimap press must own its actual layer, preserving tool input and panel geometry. Real pointer/type regressions preserve the document and canvas view. |

The original diagnostic issue packets remain useful provenance; they describe the baseline,
not the repaired source. See [the audit](2026-11-09-ui-ux-audit.md) and the separately recorded
layout repair acceptance/evidence. Public-safe screenshots and full issue packets are local at
`/workspace/ux-review/README.md` and `/workspace/ux-review/issue-drafts.json`.

## Validation

- Full integrated gate on `7b4e753a0a5f3379c6172d8b80cd032ee8e6f939`: **all five steps passed**.
  `TMPDIR=/var/tmp NEXTEST_PROFILE=slow-tests cargo xtask ci` passed formatting, Clippy with
  warnings denied, **6,506 enabled native tests**, doctests, dependency layering, 405 decision
  records and all library/testkit/web WebAssembly checks. The existing profile retained the
  default filter and 266 configured skips while collecting failures without stopping early.
  Catalog file validation is part of the test suite. Rust 1.97.1 was activated through
  `/workspace/.nodecraft-tools/activate.sh`. The exact revision is recorded in the
  validation manifest; the full command log is retained.
- Real headless Chromium: **five browser tests passed** (engine two, mesh three). A freshly
  emitted Rust/Wasm Worker additionally decoded a 9,000,084-byte STL at both root and nested
  URLs, matched the native geometry fingerprint and accepted a real pointer edit during
  reported decode. Missing Worker files produced the expected refusal while editing remained
  usable. This dev-profile conformance proof does not certify release latency.
- The two ignored engine panic hunts passed in 318.67 seconds. Production engine/command
  code did not change afterward; later UI dismissal and documentation work does not alter
  that tested command path.
- The measured **2 GiB persistence proof** covers open, save, Save As, cancellation, forced-quit
  recovery and startup keep-aside in a native production-UI telemetry harness. The decision
  record preserves exact timings, residency/inventory bounds and its Linux/harness limits.
- UI evidence includes both themes, agreed 1000×700 and 1600×900 opening checks, small windows,
  pointer/key lifecycle tests and exact physical-size assertions at 200%. Historical diagnostic
  files mislabelled as 1280×720 at 200% are explicitly corrected in the audit; valid narrow
  before images and newly verified repair images remain available.

Logs and SHA256/source provenance are local at
`/workspace/scratch/medium-sweep/validation/manifest.json`. Browser Worker evidence is at
`/workspace/scratch/mesh-worker-e2e/REPORT.md`; the UI inventory matrix is at
`/workspace/ux-review/acceptance959-matrix.md`.

The completed inventory snapshot contains **153 actual before/after file pairs**: 152 have
the same dimensions, and one legacy Tag undo pair is explicitly marked as a semantic-state
comparison with different viewport sizes. All 306 PNG hashes were checked. The fresh baseline
uses the original product source plus a recorded capture-only patch; native gallery captures
also retain their executable hashes and the limit that the preinstalled baseline has no
embedded source revision. One old File-workflow capture hook is explicitly partial on both
versions; actual native File/Open recent captures and enabled interaction tests cover that
surface. Source, image index and limitations are at
`/workspace/ux-review/capture-pairs/index153.json`.

During capture follow-through, two test-only topics corrected retained-notice assertions and selected the actual
Light theme in browser-question fixtures. Their 15 fixture hooks and the ignored recovery
capture passed, with inspected corrected screenshots. Earlier mislabelled theme images are
kept as historical diagnostics and are not Light-theme proof. The eighth finding was then
proved independently with a three-column Batch interior and a Shader prompt; the original
six-pixel Batch edge probe was discarded as ambiguous scrollbar territory. Its separate
before/repair proof is at `/workspace/ux-review/navigation-occlusion/`.

The navigation correction had one initial independent review of `96823163..aca30342`. It
identified a stale quick-add layer recognizer, which was corrected by sharing the actual
navigation layer identity. Sole verification of `aca30342..7b4e753a` closed cleanly: the three
new regressions and 37 minimap tests passed, with two configured ignores. The correction's
focused groups totalled 151 passing test executions across 13 filters, including repeats.
The final full gate above includes the fix, both browser-theme fixture corrections and all
related source changes. Only documentation recording followed that gate.

Independent review covered the new provider implementation and its sole verification, the
new Vertex path and its correction verification, the new modal/accessibility and layout
repair topics, frontmost tool Escape, and the separate navigation-occlusion correction. Existing PR review histories and their two-round limits were preserved. No
known correctness finding was left unresolved. Focused native evidence is not presented as a
substitute for the final Cargo gate or real browser checks.

## Remaining prerequisites

A real ComfyUI instance/live workflow evidence is still required for #948/#24. No paid run was
made. The retained-scene design for #569 requires a versioned mesh/worker payload, bounded image
residency, UV/texture GPU bindings, an embedded writer and native/browser capture fidelity
proofs. The detailed local follow-up packet is
`/workspace/scratch/medium-sweep/materials-569-followup.md`.

The all-menu review separates its finite inventory acceptance from optional broader state/host
combinations; see [the acceptance follow-through](2026-11-09-ui-959-acceptance.md) and the local
`/workspace/ux-review/acceptance959-matrix.md`. #1114 retains the in-card target design work.
No remote issue labels or checkboxes were changed.

The related easy M6 media-editor (#377) and Future-provider (#382) documentation close-outs
are completed locally after verifying that their feature prerequisites are finished. README,
ROADMAP, the plan and focused guides now describe actual host wiring, limits and fresh
completed-only tracker counts. GitHub still has those documentation issues open. The M8
documentation (#379) received factual worker/retry/fidelity corrections but remains partial.

The other easy close-outs explicitly wait for unfinished features: #374 depends on live model
catalogs/template mappings, #376 on the signed release, #379 on retained 3D materials, and #380
on paid-automation consent. #728 already contains the bounded badge-layout wait requested in
its issue; this Linux environment cannot supply its remaining repeated Windows-run evidence.

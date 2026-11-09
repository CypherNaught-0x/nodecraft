# Future providers batch close-out · 9 November 2026 (#382)

The feature batch is complete within each adapter's documented scope. GitHub milestone 12
contains 31 issues closed as completed, Sora #28 closed as not planned, and only documentation
issue #382 open. This is a local documentation refresh: it does not close an issue or milestone,
publish a release, or establish that a remote paid service works today.

## Tracker snapshot

Read on 2026-11-09 from the repository's GitHub milestones and each milestone's issue list.
Counts exclude pull requests and count `state_reason=completed`, not every closed issue.
The denominator includes open and declined issues. A finished feature checklist does not imply
that its GitHub milestone is closed.

| Milestone | Completed / total | GitHub milestone state | Open issue numbers |
| --- | --- | --- | --- |
| M0 Skeleton | 10/10 | closed | — |
| M1 Canvas core | 36/37 | closed | —; #135 declined |
| M2 Organise | 14/14 | closed | — |
| M3 Generation | 48/51 | open | #374, #948, #1075 |
| M4 Actions | 14/14 | closed | — |
| M5 Batch & Techniques | 34/36 | open | #100, #376 |
| M6 Media editors | 13/14 | open | #377 |
| M7 Video & audio | 34/34 | open | — |
| M8 3D & documents | 10/16 | open | #24, #379, #569, #1011, #1013, #1019 |
| M9 Agent & automation | 9/11 | open | #380, #1079 |
| M10 Share & collaborate | 20/20 | open | — |
| Future providers | 31/33 | open | #382; #28 declined |

Source: [GitHub milestones](https://github.com/CypherNaught-0x/nodecraft/milestones) and
[Future providers milestone 12](https://github.com/CypherNaught-0x/nodecraft/milestone/12).
The read used `GET /repos/CypherNaught-0x/nodecraft/milestones?state=all` and each milestone's
`GET /repos/CypherNaught-0x/nodecraft/issues?milestone=N&state=all`; all returned issue lists
fit within the 100-item page. Issue #382's old claim lapsed, and no matching open pull request
was found. No claim, issue, label, pull request or milestone was changed by this audit.

## Implemented scope and host evidence

The standard engine enables provider features in `crates/engine/Cargo.toml` and constructs
adapters through `crates/engine/src/provider_setup.rs`. The app, CLI and browser use that engine;
sharing construction does not remove the transport and storage restrictions below. Catalog
models are embedded by `crates/catalog/src/lib.rs` from `assets/catalog/`. The provider guide
retains the exact modes and bounds; this table is the batch inventory.

| Issues | Current implementation | Limits that remain material |
| --- | --- | --- |
| #25, #33, #36, #38–#40, #177 | Direct Runway, BFL, Stability, Magnific, OpenAI Audio, Deepgram and Higgsfield adapters, bound by the host factory | Keys, per-model inputs and native storage/transport requirements still apply. Remote cancellation and retry guarantees differ by adapter. |
| #26–#27, #29–#32, #34–#35, #37, #41–#43 | Explicit fal catalog routes for Luma, MiniMax media, Pika, LTX, Grok Imagine, Alibaba, Ideogram, Recraft, Topaz, Rodin, Hunyuan3D and HeyGen | These are scoped model routes, not each vendor's complete API. Several refuse browser or in-memory hosts before payment because results need streamed project storage. |
| #44–#48 | Gemini API text/video reading, Azure deployment cards, Bedrock OpenAI-route cards, Google Cloud Gemini image models and Hugging Face text | Credentials and model availability are service-specific. Azure, Bedrock and Hugging Face entries do not invent a universal monetary quote. |
| #864–#865 | Named ComfyUI servers, default and per-server concurrency; Comfy Cloud profile and adapter | Comfy Cloud partner/API nodes are refused. WebSocket progress has polling fallbacks; arbitrary external workflow plugins are not bundled. |
| #489 | Maintained LLM client decision, followed by rig-core over NodeCraft's own transport | The chosen client does not own NodeCraft prices, credentials, budgets or persisted types. See the [decision](../decisions/2026-11-06-llm-layer-rig.md). |
| #1060, #1062 | Direct BFL FLUX.2 text generation and Kontext editing; scoped output-megapixel/fixed-image quotes | FLUX.2 editing that requires public input URLs is not offered. HeyGen duration rounding and Magnific credit conversion do not support a verified monetary bound, so those quotes remain unknown. |
| #1061 | Pika 2.5 first-frame and keyframe video through fal | fal removed the text-to-video route; its absence is recorded rather than represented as working. |
| #1070 | Speech preview requests on desktop, browser playback and CLI partial-progress reporting | This establishes the supported speech path, not arbitrary provider streaming or browser parity. |
| #28 | Sora intentionally excluded | Closed as not planned after retirement, never counted as an implemented adapter. |

Recent merged history supporting the last batch follow-ups:

- [PR #1102](https://github.com/CypherNaught-0x/nodecraft/pull/1102), BFL model/editing expansion,
  merged 2026-11-07.
- [PR #1101](https://github.com/CypherNaught-0x/nodecraft/pull/1101), BFL megapixel quotes and
  documented unknown HeyGen/Magnific prices, merged 2026-11-07.
- [PR #1092](https://github.com/CypherNaught-0x/nodecraft/pull/1092), Pika keyframes and removed
  text-to-video route, merged 2026-11-07.
- [PR #1095](https://github.com/CypherNaught-0x/nodecraft/pull/1095), browser/CLI speech previews,
  merged 2026-11-07.

## Local continuations and verification limits

This source tree also includes Vertex Gemini chat (#1151), eight additional key-only text
endpoints (#1152), and Bedrock's native Converse/SigV4 paths (#1150, #1163). These are separate
follow-ups, not evidence that their original batch issue promised every later capability.
In particular, #1151 and #1152 remain open on GitHub at this snapshot; their local implementation
and fixture checks must not be reported as remote issue closure or released availability.

No paid request was made for this close-out. Provider contract fixtures, host wiring and wasm
compilation are useful evidence, but do not prove current account permissions, browser CORS,
remote model availability, cancellation refunds or invoice accuracy. Unknown-price admission,
no automatic replay after an uncertain paid submit, and per-adapter native/browser refusals
remain product behavior, not unfinished documentation. See [provider details](../providers.md).

The latest published release checked here is **v0.6.0**, published 2026-10-28. Downloads retain
their tagged source; no new release was published. The README's generic roadmap badge and test
floor remain appropriate. Module headers in providers, catalog and provider setup were checked
for obsolete claims that nothing constructs an adapter; no such header required a code edit.
No new convention or architecture decision was introduced, so no decision record was added.

Validation for this documentation-only topic: `git diff --check` and local relative-link checks.
Cargo gates are omitted under the repository's documentation-only exemption. Product changes
and their broader validation belong to their own topic records and the integrated gate.

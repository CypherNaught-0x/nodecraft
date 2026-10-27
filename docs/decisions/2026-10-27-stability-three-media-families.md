## 2026-10-27 · One first-party Stability adapter for image, audio and 3D (#36)

Stability's direct Image Core, Audio 3 Large and Fast 3D models share a bearer key and the same
API origin. They run on the existing Image, Audio and 3D generation cards through the shared
HTTP transport, delivery accounting, task runner and RunHost. There is no separate execution
engine or SDK. Model descriptions and prices remain catalog data.

Aggregator coverage was checked first. fal publishes SD3.5 Large and Audio 3 Small, which are
different models from Core and Audio 3 Large; its tested Fast 3D alias has no published queue
schema. Replicate's official Stability publisher lists image models and Audio 2.5, without an
official current Fast 3D or Audio 3 Large entry found in that inventory. A mixed route would need
two credentials and distinct model contracts. The direct API covers the chosen current models
in all three issue families with one account. This is not a claim that every Stability model,
editor or aggregator alternative is implemented.

The current reference moved from `platform.stability.ai/docs/api-reference` to
`docs.stability.ai/api-reference`. Its pricing page, Audio 3 operation and OpenAPI agree on
8 credits per Audio 3 generation; the older platform pricing page still says 26. The catalog
uses the migrated reference's 8 credits ($0.08), alongside Core's 3 and Fast 3D's 10. Sources and
the dated discrepancy are recorded in [providers](../providers.md#stability-ai-direct).

Core and Fast 3D finish synchronously. Audio returns a 64-character id; only a versioned bounded
handle and requested duration persist, never a key, remote-supplied URL or binary body. GET polls
stay on the fixed API origin and resume that id. The API offers no documented cancellation or
submission deduplication: ambiguous paid creates are terminal, and local cancellation cannot
claim a refund. Only parsed documented refusal envelopes clear paid-send evidence and permit
the shared retry policy to reconsider a create.

Host quotas are explicit: a 4 MiB image input with 64–2048-pixel sides for 3D; output caps of
16 MiB image, 128 MiB WAV and 64 MiB GLB. Audio uses WAV so its structure and duration can be
checked before publication, including bounded RIFF chunks, PCM/IEEE-float sample formats,
frame alignment and byte rates. Images and self-contained meshes pass the existing bounded decoders.
All authenticated requests use no-redirect transport. The built-in browser transport cannot
provide that guarantee, so host setup refuses this adapter there before key retrieval or billing.

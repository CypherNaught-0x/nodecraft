## 2026-10-28 · Supplied-audio HeyGen avatars and lip sync reuse fal (#43)

Use the existing fal key, canonical application-root queue, saved handles and uncertain-create
policy. Photo Avatar4 takes a portrait plus supplied PCM WAV, with stable movement, 720p,
16:9, captions disabled and safety enabled. Speed and Precision lip-sync take MP4 plus WAV,
with dynamic duration and captions disabled. Expose only the documented music-removal and
speech-enhancement boolean controls. Do not send text, voices, partial timing or unsupported
safety fields. No stock identities, native HeyGen account or TTS dependency is needed.

Published rates are USD 0.10/0.05/0.10 per output second, but no strict output duration or
billing-rounding contract establishes a maximum currency amount. All three catalog entries
remain unknown/by-usage USD and require explicit acceptance of an unpriced job. Do not turn
measured input duration into a fabricated invoice. Omit optional AudioUse/Edit metadata:
the current enums would assert unsupported duration or unchanged-sound semantics.

Client admission requires a static upright PNG/JPEG/WebP up to 1 MiB/1024axes, measured PCM
WAV up to 1 MiB/10s, or source MP4 up to16 MiB/1280×720/10 s. Reject missing or inconsistent
container clocks (over 100 ms difference), not unknown-length estimates. Bound the complete
escaped multi-data-URI JSON before codec work or owned base64 allocations. A HeyGen-only
16 MiB input and 32 MiB request envelope leaves every legacy fal model's input/request caps
unchanged. Require full default response/output budgets before new paid work; input/request/
text/codec limits remain lowerable. Saved handles intersect current output limits.

Reuse Pika's bounded static-raster and MP4 metadata walks, keeping its original 11 s policy.
HeyGen output is one MP4 within 256 MiB individual/aggregate, 4096 axes/16,777,216pixels,
11 s positive clocks,256 KiB movie metadata,4096 boxes and16 levels. These are client bounds,
not full decode, face detection, content quality or vendor output guarantees. Only existing
credential-free fal.media origins are accepted; caption sidecars are ignored and never fetched.

Resolve final executed quote model ids to provider and remote alias before allowing native
storage only. Cover renamed catalog entries and deferred steps without scanning unused cards.
Use the existing worker hash/seal, UI receipt adoption, held retry and indexed save/reopen
paths. An ambiguous create cannot be replayed, and cancellation cannot promise a refund.

Primary schemas/rates checked 2026-10-28: [Photo Avatar4](https://fal.ai/models/fal-ai/heygen/avatar4/image-to-video/api),
[Speed](https://fal.ai/models/fal-ai/heygen/v3/lipsync/speed/api),
[Precision](https://fal.ai/models/fal-ai/heygen/v3/lipsync/precision/api).
Offline fixtures and native UI evidence are authored locally; paid live generation is opt-in
and has not been performed.

## 2026-10-27 · Hunyuan3D V3 through the existing fal queue (#42)

Use fal's current Hunyuan3D V3 text, photo and sketch endpoints with the existing fal key,
saved queue, canonical routes and no-replay policy. No Tencent credential implementation,
SDK, provider feature or new renderer is needed. These are live service aliases, not pinned
weights. See the [dated contracts and prices](../providers.md#hunyuan3d-through-fal).

Mesh presets bind generation type, PBR and low-poly topology into one priced choice.
Sketch supports only its documented textured/PBR choices and omits the other schemas' type
and topology fields. Enforce its 128×128 minimum image size before paid HTTP, within the host's
1024-pixel maximum. Retain the standard 500K face target by omitting `face_count`; custom
counts and multiple views add fees, so neither is offered in this integration. Unknown fields
and extra images refuse before sending, and safety remains enabled.

Add the `call` pricing unit so choice-priced jobs with `default_count = 1` are described as
calls rather than images. Preset rates include the PBR surcharge; quotes and submitted flags
come from the same bounded choice. Existing pricing units and models retain their behavior.

Hunyuan3D examples include GLBs over 60 MiB, beyond fal's old 16 MiB mesh cap. Only these
three aliases receive a 256 MiB per-file/aggregate result allowance, including saved handles.
Other fal model quotas stay unchanged. Consume only the canonical `model_glb`, with no key
on the storage request or redirect following; sidecar and thumbnail URLs are not routing
authority. Existing packed-GLB and mesh decoder bounds remain in force.

The catalog and adapter still build for WebAssembly. The builtin browser transport refuses
the no-redirect operation before fetch; execution needs native HTTP or an audited host
transport. Paid live validation remains opt-in. Offline fixtures prove queue, pricing,
budget ledger, typed output and decoder behavior without spending API credit.

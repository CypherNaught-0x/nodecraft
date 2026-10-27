## 2026-10-27 · Alibaba Qwen-Image 3 and Wan 3.0 through fal (#32)

Reuse the fal provider, credential, saved async queue, canonical routes and no-replay policy
for Alibaba's current Qwen-Image 3 text/edit and Wan 3.0 text/frame aliases. A direct DashScope
SDK or a second task implementation would duplicate capabilities already covered by this
aggregator. These are live aliases, not immutable model weights. The dated
[contracts and rates](../providers.md#alibaba-through-fal) describe the supported subset.

Qwen exposes exact square 1024×1024 and 2048×2048 presets, with one to six outputs.
The published per-image tiers are $0.04 at 1K and $0.075 at 2K. Explicit dimensions avoid
unpriced automatic sizes or assumptions about arbitrary aspect-ratio tier boundaries.
Edit references are ordered, limited to three, and validated against their actual bytes:
each axis 384–1024, at most 1 MiB, and no alpha channel for PNG. Host bounds are deliberately
narrower than the remote 2048-pixel/10-MiB allowance.

Wan exposes explicit 2–30-second durations and 480p/720p/1080p, so the quote is determined
before payment. Native audio, prompt expansion and thinking are explicit switches. Frame
mode keeps its optional prompt and optional last frame. Smart duration and the separate
reference-to-video endpoint remain unsupported. Fixed numeric seeds are optional; no
random-seed semantic contract is claimed.

Only these aliases admit up to 96 KiB of text metadata (20,000 UTF-8 prompt characters),
Qwen editing admits an 8 MiB encoded request, Qwen output batches admit six bounded images,
and Wan clips admit 256 MiB including resumed handles. Legacy aliases retain their quotas.
Escaped JSON and every base64 extent are admitted before codecs or owned encodings.
Safety remains enabled. No redirect following, key forwarding to storage or paid-create
replay is introduced. Native HTTP or an audited host transport is required for execution;
WebAssembly compilation does not imply builtin browser transport support.

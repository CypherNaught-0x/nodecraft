## 2026-10-24 · Luma Ray and Photon reuse fal's queue (#26)

The issue permits an aggregator adapter where it already covers the provider. fal exposes
documented Ray 3.2 text/frame video and Photon image endpoints, so these are three explicit
catalog entries and a separate request module inside the existing fal adapter. Its fixed
origins, bounded responses, secret headers, accepted-task resume and refusal of ambiguous
submission replay remain shared. No new key account or provider feature is needed.

Ray text admits 5/10 seconds; first/last-frame animation admits 5 only. The 10-second frame
route requires multi-keyframe controls not offered here. Duration is sent explicitly and
standard SDR MP4 is requested with HDR and EXR disabled. Optional looping requires five seconds
and no end frame. Rate by resolution is 0.10/0.20/0.40 USD per second at 540p/720p/1080p.
Photon charges 0.019 USD/megapixel but offers no fixed output geometry, so it keeps an unknown
upfront quote. fal's optional billable units are retained without converting them to Money.

The model-specific module counts escaped JSON and encoded input sizes before decoding frames.
Existing 1 MiB/1024-pixel input and 16 MiB image output limits stay intact. Ray text/frame
clips use a separate 256 MiB per-file/aggregate ceiling, selected again from the admitted model
when resuming. Lower application budgets still apply. Other fal models retain their existing
output caps. MP4 checks inspect the container envelope without codec allocations; the image
codec allocation bound stays unchanged. Photon uses the generic
file schema, whose dimensions are optional; its actual PNG/JPEG/WebP is probed and decoded.
Other image models keep their PNG and required-dimension contracts. Ray uses the existing
MP4 envelope validator, which does not decode the video stream. Extra EXR output is refused.

Create requests use the full endpoint, while Ray's queue root is `luma/agent` and Photon's is
`fal-ai/luma-photon`. Primary sources checked 2026-10-24:
[queue paths](https://fal.ai/docs/inference-api/queue/submit-path),
[Ray text](https://fal.ai/models/luma/agent/ray/v3.2/text-to-video/api),
[Ray frames](https://fal.ai/models/luma/agent/ray/v3.2/image-to-video/api), and
[Photon](https://fal.ai/models/fal-ai/luma-photon/api).
No paid live validation has been performed. Account availability, real receipts and billing
remain subject to the explicitly opted-in live tests.

## 2026-10-27 · Native Magnific Precision V2 with unknown credit pricing (#38)

Use a dedicated `magnific` provider and key account. No official Magnific endpoint was found
on the existing aggregators; another vendor's creative upscaler is not Magnific. Current
contracts use `api.magnific.com` and `x-magnific-api-key`. Keep the header secret through the
shared HTTP boundary, and never validate a key by paying for generation. Browser setup and
submission return an explicit unsupported reason.

Offer one Precision V2 photo-flavor model, one upright static PNG/JPEG/WebP input <=1 MiB
and 1024 pixels per axis. Scale is an explicit choice of 2 or 4; normalize its schema text
to an integer in the wire request. Sharpen, smart grain and ultra detail are integers 0–100,
with defaults 7/7/30. Fix NSFW filtering on. Do not send prompt/seed/count/output-format fields
absent from this API. Keep expected geometry and lower-only payload/codec limits in saved
handles; the single output must be a fully verified original raster within 4096²/256 MiB.

Current pricing consumes organizational credits without a verified per-job currency rate.
Use unknown/by-usage USD pricing and the shared unpriced-job consent/budget path, never an
old EUR example or a subscription-bundle average as a fixed invoice. Variations are separate
unpriced jobs. The API does not report an authoritative monetary cost in this task response.

Reuse shared async task/run machinery around canonical UUID polling. There is no documented
idempotency or remote cancellation contract: an ambiguous paid POST cannot replay, and local
cancellation cannot promise the remote task stopped. Only complete documented rejection
evidence proves nonacceptance. Results are delivered from the documented exact
`ai-statics.freepik.com` host, without credentials or redirects.

Output URLs expire 30 minutes after generation and cannot be recovered. Download immediately;
durable task identity does not promise long-offline recovery of an expired file. Preserve
any successfully imported original through history and project save/reopen rather than keep
only its temporary URL. Require native streamed storage before a paid job; use the existing
shared result-sealing worker and lightweight adoption path for large originals. Do not add
a second synchronous large-file import path on the UI thread.

Primary sources checked 2026-10-27: [request schema](https://docs.magnific.com/api-reference/image-upscaler-precision-v2/post-image-upscaler-precision-v2),
[task status](https://docs.magnific.com/api-reference/image-upscaler-precision-v2/get-%7Btask-id%7D-by-id),
[pricing](https://docs.magnific.com/pricing), [authentication](https://docs.magnific.com/authentication)
and [expiry](https://docs.magnific.com/quickstart). Validation uses authored offline media;
paid live generation is explicitly opt-in and has not been performed for this change.

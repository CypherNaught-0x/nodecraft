## 2026-10-24 · Runway video tasks (#25)

- **Use the direct video API.** Gen-4.5 supports text and first-frame video; Gen-4 Turbo
  requires a first frame. The existing Replicate adapter deliberately admits pinned image/text
  contracts and does not already cover Runway. A separate optional `runway` adapter reuses the
  shared submit/poll/download runner and the OS keychain account `runway`.
- **Quote the parameters sent.** Standard MP4 generation is priced at 0.12 USD per second for
  Gen-4.5 and 0.05 USD for Turbo, excluding tax. Duration is always sent explicitly, 2–10 seconds.
  Gen-4.5's catalog offers the two frame sizes shared by its modes; Turbo offers six. Premium
  formats, video editing, reference sets and third-party models are outside this contract.
- **No automatic replay of an uncertain paid create.** The API documents no submission
  idempotency guarantee. Local validation can prove no request was sent; errors after dispatch
  remain uncertain. An accepted UUID is resumed with five-second polls. Optional reported
  nonnegative credit cost becomes USD usage when representable. Missing cost, negative credit
  adjustments and overflowing conversions stay unknown and retain the conservative estimate,
  without discarding the video. Turbo omits `outputFormat`, which is not documented for Turbo;
  this adapter sends it only on Gen-4.5's image route.
- **Bound the data and isolate credentials.** First-frame PNG/JPEG/WebP inputs are decoded within
  4096-pixel/128 MiB limits and capped at 3 MiB before base64 encoding. Metadata responses are
  capped at 64 KiB; output is capped at 256 MiB and its MP4 headers must have bounded dimensions
  and duration. Downloads accept the documented public CloudFront HTTPS origin pattern, without
  API headers or redirects. Diagnostics never include provider messages, URLs or credentials.
- **Cancel without intentionally deleting a finished result.** DELETE cancels active tasks but
  deletes completed output. Read the task first and refuse deletion when already complete;
  completion can still race the DELETE because the API has no conditional cancel. A missing or
  already cancelled task is an idempotent success. Cancellation does not promise a refund.

Sources checked 2026-10-24: [OpenAPI](https://docs.dev.runwayml.com/openapi.json),
[pricing](https://docs.dev.runwayml.com/guides/pricing/),
[outputs](https://docs.dev.runwayml.com/assets/outputs/), and
[task failures](https://docs.dev.runwayml.com/errors/task-failures/).

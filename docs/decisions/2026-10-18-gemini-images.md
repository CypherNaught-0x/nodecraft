## 2026-10-18 · Stateless supported Gemini images (#14)

Use explicitly selected supported Gemini image models through Interactions REST. Retired Imagen and Vertex are excluded. Catalog and adapter share a conservative capability subset; the REST 1K enum wins over Lite model prose 1024px. Request JPEG inline because that is the selected raw response-format enum, rather than assuming SDK convenience examples define wire support.

One no-redirect paid submit has no automatic ambiguous retry, undocumented remote idempotency, resumable handle or confirmed remote cancellation. Arm audited local preparation, then taint delivery before constructing the no-redirect HTTP future. Unknown money remains unknown under ByUsage. Parse optional token usage independently so malformed accounting does not destroy valid paid images.

Retain every final output and validate whole collections. Existing graph conversion requires matching modality routes and Run list ports; this adapter does not filter outputs or add asset resolution. Hosts must keep the original Completed when conversion refuses. See [provider details](../providers.md) for local bounds, memory qualifications, browser refusal and live-test guards. No paid validation is implied by synthetic fixtures.


### Documented geometry and local admission

The [official image guide](https://ai.google.dev/gemini-api/docs/image-generation), checked 2026-10-18, gives the same selected Nano and Pro geometry tuples. The adapter stores the 1K column below; the documented 2K and 4K columns are exactly twice and four times each axis. These are local requested-envelope admission facts, not an assertion that a service cannot return different content. The complete returned collection still undergoes actual byte, probe and full codec validation.

| Aspect | 1K dimensions | 4K dimensions |
| --- | --- | --- |
| 1:1 | 1024 × 1024 | 4096 × 4096 |
| 2:3 | 848 × 1264 | 3392 × 5056 |
| 3:2 | 1264 × 848 | 5056 × 3392 |
| 3:4 | 896 × 1200 | 3584 × 4800 |
| 4:3 | 1200 × 896 | 4800 × 3584 |
| 4:5 | 928 × 1152 | 3712 × 4608 |
| 5:4 | 1152 × 928 | 4608 × 3712 |
| 9:16 | 768 × 1376 | 3072 × 5504 |
| 16:9 | 1376 × 768 | 5504 × 3072 |
| 21:9 | 1584 × 672 | 6336 × 2688 |

The largest area is 17,203,200 pixels; the 34,406,400 aggregate deliberately covers two such images. Sixteen smaller outputs remain possible within all other bounds, while three 4K outputs can refuse. Whole-collection validation never filters away an excessive later result. For known Nano/Pro requests, lower host geometry/codec limits must cover the requested tuple, twice its area and eight bytes per pixel for possible 8-bit JPEG channels plus orientation. Lite has no verified separate tuple table, so lowering its HARD geometry or codec envelope conservatively refuses before POST. PNG/WebP reference preflight remains decoder-derived, including higher bit depths, rather than assuming four bytes per pixel.

The 300-second HTTP allowance supplies bounded service/body-transfer headroom for potentially expensive 4K work without making ambiguous replay safe. It is not a measured latency, service SLA or propagated RunLimits deadline. Cancellation can still leave a paid remote request running. The catalog's 120-second approximate latency hint is separate.

### Resource ownership

These are source-derived requested logical extents per operation, not measured allocation capacities, process RSS or a universal codec allocation bound. Caller-owned reference bytes are conservatively included. The operation does not retain decoded rasters across items. Native transport drops the request body before response parsing; custom host retention and concurrent calls need separate budgets.

| Phase | Conservative simultaneous logical owners (MiB) | Total MiB |
| --- | --- | --- |
| Input validation | caller 48 + pixel/orientation allowance 256 + known JPEG copies 24 + auxiliary 2 | 330 |
| Request writing | caller 48 + body 64 + auxiliary 2 | 114 |
| Native dispatch | caller 48 + two body extents 128 + auxiliary 2 | 178 |
| Response DTO parse | caller 48 + raw 65 + normalized strings 65 + JSON scratch 22 + auxiliary 2 | 202 |
| Sequential output decode | caller 48 + raw 65 + normalized strings 65 + retained compressed outputs 48 + current compressed Vec 8 + Vec/Arc overlap 8 + pixel/orientation allowance 256 + known JPEG copies 24 + auxiliary 2 | 624 |

The pinned image 0.25.10 JPEG decoder owns an encoded-input Vec (at most 8 MiB), and its orientation/header path can retain metadata plus an EXIF clone (conservatively another 16 MiB). These known copies are explicitly charged as 24 MiB, even though header processing and full pixel decode have shorter, partly disjoint lifetimes. JPEG output color maps to at most four 8-bit channels: pixels plus a possible orientation copy use at most 137,625,600 bytes at the admitted maximum. The 256 MiB named pixel/orientation allowance is retained conservatively; the extra 24 MiB is not silently counted as zero inside it. Internal decoder coefficients, other private codec workspace, allocator capacity growth and transport internals are additional and excluded. This is not a claim that media::Limits enforces every codec allocation. PNG/WebP input codec-private workspace is likewise not certified by this logical ledger.

JSON scratch belongs to from_str and drops before output decoding. The 22 MiB allowance is twice the largest admitted normalized image string; raw and normalized strings are separately charged. Auxiliary 2 MiB covers bounded descriptor/key/table slots, metadata and the fixed 4 KiB writer at native 64-bit sizes. These phases fit the existing 628 MiB envelope without increasing it. Test generators destroy their temporary RGB raster before invoking production parsing; their separate fixture memory does not establish production RSS.

The adapter requests image-only delivery but preserves every final text/image block actually returned. Host mixed/list conversion and durable retained-result ownership remain follow-up work; no output is dropped and no paid call is replayed to repair routing. Feature-on CI includes prior optional OpenAI/Replicate coverage together with Gemini and the separate provider-facade wasm gate. No paid live test is enabled in CI.

### Current-main provider readiness

The existing host factory builds `gemini-images` from the family keychain account `gemini`,
reports missing keys before routing, and sends no request at construction. Unknown-price
admission still requires partial-price consent plus an explicit unknown-job count; finite
budgets refuse unknown estimates. This adds neither a paid-run host nor a media-input resolver.

Loopback native transport controls pin the request path, secret header, generic User-Agent and
redirect refusal without paid calls. A direct non-UTF8 fixture refuses before completion.
The encoded-output and pixel policies above were superseded by
[paid image output admission (#421)](2026-10-22-paid-image-output-admission.md), which also
corrects the historical response-phase total and records the shipping asset-resolver regression.

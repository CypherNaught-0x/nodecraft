## 2026-10-22 · Paid image output admission (#421)

Accept Gemini JPEG outputs up to 16 MiB each, retaining the 48 MiB collection, 65 MiB raw
response and 64 MiB aggregate encoded-string limits. References remain 8 MiB each. An original
procedural 4800×3584 textured JPEG exceeds the previous 8 MiB output cap and decodes intact
with the new default. This gives detailed 4K responses more room without promising every
possible JPEG fits. Lower-only host configuration still applies and an excessive later image
refuses the complete collection.

The selected ten aspect ratios in the [official image guide](https://ai.google.dev/gemini-api/docs/image-generation),
checked 2026-10-22, include a largest area of 17,203,200 pixels. Admit 18,000,000 pixels per
image and 36,000,000 across the collection, giving modest response headroom while preserving
the 8192-axis and 256 MiB pixel/orientation allowances. Nano and Pro requested size/ratio tuples
still use exact documented dimensions during pre-submit admission. Reference geometry uses the
same pixel policy; input byte bounds remain unchanged. Tests cover two 18M-pixel outputs, one
pixel-row over the cap, lowered aggregate rejection and every existing selected 4K shape.

Seedream response `size` is advisory. Absence, duplication, wrong type, malformed spelling or
disagreement with the JPEG must not discard an otherwise valid paid result. Actual JPEG
headers determine dimensions, area and aspect ratio before full decoding. Exact dated model,
one unique required image blob, complete JPEG framing, no trailing bytes, MIME, structural
JSON/base64, byte and codec checks remain strict. Adversarial real JPEG headers demonstrate
that misleading metadata cannot bypass the geometry limits. Optional usage remains independent.

### Resource ownership

These are conservative requested logical extents per operation, including caller references,
not allocation capacities or process RSS. The earlier worksheet's sequential output terms sum
to **524 MiB**, despite its printed total of 624 MiB. The corrected current worksheet is:

| Phase | Simultaneous logical owners (MiB) | Total MiB |
| --- | --- | --- |
| Input validation | caller 48 + pixel/orientation 256 + known JPEG copies 24 + auxiliary 2 | 330 |
| Request writing | caller 48 + body 64 + auxiliary 2 | 114 |
| Native dispatch | caller 48 + two body extents 128 + auxiliary 2 | 178 |
| Response DTO parse | caller 48 + raw 65 + normalized strings 65 + JSON scratch 44 + auxiliary 2 | 224 |
| Sequential output decode | caller 48 + raw 65 + normalized strings 65 + retained outputs 48 + current compressed Vec 16 + Vec/Arc overlap 16 + pixel/orientation 256 + known JPEG copies 48 + auxiliary 2 | 564 |

JSON scratch reserves twice a largest normalized 16 MiB image's base64 string, rounded up.
Known output JPEG copies reserve 16 MiB encoded-input storage plus 32 MiB for metadata and
an EXIF clone. The larger output bound does not change the 24 MiB corresponding input charge.
All phases fit the existing 628 MiB logical envelope. Codec-private coefficients/workspace,
allocator growth, transport internals and concurrent host ownership remain additional and
excluded; the codec allowance is not a universal memory ceiling. Fixture rasters drop before
production response parsing, and fixture allocations establish no production RSS claim.

The shipping RunHost already resolves image assets through `queued_job_with_media` (#502).
An offline regression now selects each Gemini, Seedream and OpenAI Images catalog entry,
wires a stored JPEG, accepts a quote, runs the real adapter through a canned no-redirect
transport and records the complete returned JPEG. This corrects stale resolver documentation
without adding another resolution path. No paid service, live key, service quality or platform
availability is established by these fixtures. Uncertain paid dispatches are never replayed.

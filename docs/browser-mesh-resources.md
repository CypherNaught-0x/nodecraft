# Browser mesh worker resource worksheet (#1013)

This is an ownership and admission worksheet, not a process-RSS guarantee. Decimal MB below
means 1,000,000 bytes; MiB means 1,048,576 bytes. Collection capacities, allocator granularity,
codec-internal metadata/working state, Wasm heap high-water retention and browser/GPU driver
memory add overhead. A worker moves computation off the frame; it does not make these copies
free or grant an unbounded model/storage profile.

| Path | Admission / largest owned data | Lifetime and overlap |
|---|---|---|
| Original store | existing 16 MiB per asset /48 MiB total | Immutable insertion-verified Arc; no full-store clone, no changed source/history/export bytes. |
| Input transfer | one owned JS ArrayBuffer ≤16 MiB | Copied in ≤256 KiB logical chunks. Transfer detaches this staging buffer, never Wasm memory or the store original. |
| Worker input | received JS original ≤16 MiB + one admitted Rust Vec ≤16 MiB | Both may coexist during decode. Hashing occurs on the worker. No paths, URLs or service objects in the request. |
| glTF typed metadata | pre-count ≤250,000 JSON values /16,384 objects; all source text ≤16 MiB | Pre-count before serde parsing includes sparse values. Strings/attribute maps/typed arrays and their capacity overhead are separate from geometry. URI string content is bounded by encoded input. These are count/byte inputs to metadata allocation, not a claim of exact serde heap bytes. |
| Embedded buffers/images | data-URI base64 decoded content ≤about12 MiB in aggregate for a16 MiB JSON original, plus per-URI capacity slack | Retained embedded buffers plus the current image URI share the encoded source ceiling; GLB buffer views borrow existing input bytes. No external URI fetch. |
| Traversal | ≤8,192 visits/depth32; scene root references and cloned child lists additionally bounded by JSON counts | The current reader builds the initial root world-matrix stack before visit refusal: up to250,000×72B≈18 MB on wasm32. Do not misreport visit cap as a stricter allocation bound. Subsequent pushes are visit bounded; root/child numeric vectors add up to1 MB each. |
| Accessor work | cumulative (24V+6T)=16.2 million scalar values, mostly4B=>64.8 MB work budget | A single accessor is bounded by max(V,3T)=900,000 entries. Positions/normals can each hold10.8 MB, raw color floats14.4 MB, UV7.2 MB, and index order3.6 MB before a later builder refusal. Color mapping may overlap another14.4 MB; strip/fan triangulation can temporarily add up to10.8 MB before triangle admission. |
| Raw mesh |40V+12T≤27.6 MB logical arrays | Builder vectors may retain growth slack (up to roughly2×logical capacity, plus an old buffer during growth). Normal repair may add12V≤7.2 MB. Reader metadata/textures are released when decode returns. |
| Texture probe/decode | at most4 distinct image attempts, ≤1,048,576 pixels each; side-derived header limits up to4096 per axis; decoder max_alloc32 MiB | Probe opens a decoder before the pixel-product test, so its decoder budget matters too. Probe and final decoder operate serially. Shared image code independently precharges output and an EXIF orientation copy before pixels: RGBAf32 is16 MiB per image, twice with rotation. The allocation budget is passed to codec internals; codec overhead is not a global-RSS accounting guarantee. |
| Texture conversion/retention | current to_rgba8 copy≤4 MiB; four cached RGBA images≤16 MiB | Conservative current decoder/output/rotation/conversion +three older RGBA textures:32 MiB configured decoder budget +32 MiB output/rotation +4 MiB conversion +12 MiB retained≈80 MiB. Not all components necessarily coexist. Oversized/over-budget textures fall back to material/vertex color; geometry remains readable. |
| Unique edges | ≤900,000 pairs /1,800,000 u32 words=7.2 MB logical | HashSet scratch and Vec capacity/growth overhead are additional; both are bounded by the unique-edge count. The set is dropped before packing. Too many edges omit wireframe. |
| Worker packing |40V+12T+8E≤34,800,000 B | Raw mesh, edge words and packed buffers can coexist:69.6 MB logical data plus vector capacity slack. Geometry fields are finite and indices are validated before Prepared exists. |
| Output transfer | three owned JS ArrayBuffers totaling≤34.8 MB | They copy admitted packed bytes, then transfer ownership. Worker Wasm buffers can coexist briefly; no Wasm memory view is transferred. Worker termination releases its listeners/references, subject to browser GC. |
| Main adoption | validated incoming JS buffers≤34.8 MB +reserved Rust buffers≤34.8 MB +≤256 KiB copy scratch | Raw finite integral counts/products/extents, fixed buffer type and matching current job identity checked before large reservation. Exactly one chunk progresses per first egui pass/frame; finite attributes, indices, bounds and fingerprint validated incrementally. |
| Main cache |64 MiB prepared bytes /256 total descriptors | Cache admission refuses another current model if it cannot fit; a small exact-byte Budget descriptor retries only after unused entries can be evicted and headroom reserved. Moving another model offscreen makes its old Ready entry eligible for this pressure eviction; invalid-data failures do not retry. Arc paint-callback leases can briefly outlive a cache reset and release with the frame. |
| GPU /targets | existing512 MiB model and512 MiB target soft budgets /32 target policy | These are separate existing eviction policies, not added to the CPU cap as if it were an RSS budget. Current visible entries can survive eviction. Upload still writes admitted vertex/index/edge data on the frame and must be measured. |

The browser holds one active job and up to eight FIFO entries of one hash and owner ID each.
It looks up the current hash before reading a queued asset or adopting an active reply. Session,
project, canvas and asset-context changes terminate the worker and clear the queue. Deleted
or replaced owners, wrong generation/hash/build, malformed extents and late replies cannot
supply current geometry. Pan, zoom, orbit and unrelated document edits keep the same scope
and do not decode again. The startup deadline is 15 seconds and decode/adoption uses a
30-second deadline. The browser never falls back to inline decode.

Coverage: real9,000,084-byte original300×300 tessellated pyramid,180,000 facets/540,000 decoded
vertices/540,000 unique indexed edges/28,080,000 prepared bytes. Native/browser-profile exact
geometry/bounds/edge/fingerprint comparison; main-protocol raw-number/buffer checks and
zero-large-reservation stale/oversize rejection; original-store Arc sharing; context invalidation;
actual glTF lowered JSON/texture budget, retained-image count and oriented JPEG preflight tests.
The texture tests distinguish probe/output/rotation refusal from retained RGBA conversion.

## Run the browser probe

Build the test-only page from `apps/nodecraft-web` with the pinned Trunk 0.21.14.

```sh
trunk build --offline --locked --release --dist ../../target/web-conformance --public-url ./ mesh-conformance.html
```

Serve that output directory over localhost HTTP and open it in a browser. Click **Run
conformance**, then click **Edit counter** when the status reports decoding. The page runs the
worker against the native reference fingerprint and checks raw protocol admission, cancellation,
timeout, build mismatch, message errors, stale and oversized replies, and restart. Serve a copy
under a nested path and repeat to verify relative loader resolution. For the missing-worker test,
serve another copy under `/missing/probe/` with the three `nodecraft-mesh-worker*` files omitted.
The page should start, report an unavailable worker after **Run conformance**, and still accept
the edit-counter click. Production packages exclude this page.

## Packaged browser checks

The production package was built from `cb7733b38f99cb5f7e45cbf3e532736322797918`
(tree `df2389a7a76bbdc46e6475b97959f9c7bf56ab29`) using Trunk 0.21.14 and
Binaryen 133. Both optimized Wasm files fit the 25 MiB per-file limit. The served
directory and the archive contained the same 12 files, including exactly two Wasm files.

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| Application Wasm | 25,232,639 | `5e19d8437479221ee6bcc780d0958bf898d56f817ffe6e75bd8fe04d6d3e1407` |
| Mesh worker Wasm | 961,005 | `72384e6574c7eb1896f3639cf4c0ad08f1e95fb4a8b6325e51d1bd817a1b89e9` |
| Release ZIP | 10,487,103 | `cc9d74cdcf15b984a408bf2ae3e861133004d6490b9808b9fa5400df734022e7` |

Chrome 156.0.8078.80 on macOS 27.0 with an Apple M3 Pro loaded the packaged app and its
worker at `/` and `/nested/site/`. The browser accepted a pointer click and a Text card
edit before adoption began. Both project downloads contained the edit and the unchanged
9,000,084-byte original. Panning away and fitting the canvas reused the preview. Removing
the worker loader produced a 404 while the packaged app started, accepted edits, and saved
the original. Undo during decode terminated that worker before adoption. Reimporting the same
original started a new worker and rendered. Two distinct originals completed in FIFO order
with both cards offscreen and a maximum of one active worker. Fitting the canvas reused their
prepared data without starting another worker.

| Measurement for the 9,000,084-byte STL | Root | Nested |
|---|---:|---:|
| Start of decode to start of adoption | 216.6 ms | 214.7 ms |
| Pointer click after start of decode | 5.8 ms | 4.6 ms |
| Text edit after start of decode | 12.0 ms | 9.3 ms |
| Elapsed adoption across frames | 1,816.3 ms | 1,817.1 ms |
| Main-thread GPU allocation and write submission | 5.2 ms | 3.2 ms |

The test-only probe, built before the UI queue and desktop scoping fixes, exercised the
same worker protocol and matched the native prepared fingerprint
`79dc46f44cc13d0aa3b6932125d6e5013f4d9ef41d5ad4712590de324374088b`
at root and under a nested path. Its 10 protocol checks and 10 lifecycle checks passed,
including the separate missing-loader run. The longest observed single main-thread poll
was 2.7 ms. The timings describe these runs. GPU submission timing excludes GPU execution,
and elapsed adoption includes the intervals between frames.

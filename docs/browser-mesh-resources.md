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
| Main cache |64 MiB prepared bytes /256 total descriptors | Cache admission refuses another current model if it cannot fit; unused entries can be evicted. Arc paint-callback leases can briefly outlive a cache reset and release with the frame. |
| GPU /targets | existing512 MiB model and512 MiB target soft budgets /32 target policy | These are separate existing eviction policies, not added to the CPU cap as if it were an RSS budget. Current visible entries can survive eviction. Upload still writes admitted vertex/index/edge data on the frame and must be measured. |

One active job, no queued bodies. Session/project/canvas/asset-context changes terminate it;
the active owner's current hash is looked up before reply adoption. Deleted/replaced owners,
wrong generation/hash/build, malformed extents and late replies cannot supply current geometry.
Pan, zoom, orbit and unrelated document edits keep the same scope and do not decode again.
Startup deadline15s; decode/adoption30s. No large inline fallback.

Coverage: real9,000,084-byte original300×300 tessellated pyramid,180,000 facets/540,000 decoded
vertices/540,000 unique indexed edges/28,080,000 prepared bytes. Native/browser-profile exact
geometry/bounds/edge/fingerprint comparison; main-protocol raw-number/buffer checks and
zero-large-reservation stale/oversize rejection; original-store Arc sharing; context invalidation;
actual glTF lowered JSON/texture budget, retained-image count and oriented JPEG preflight tests.
The texture tests distinguish probe/output/rotation refusal from retained RGBA conversion.
Actual browser/GPU upload and maximum poll timing are PENDING and must be recorded with the
frozen browser-build commit/tree and artifact digests before publication.

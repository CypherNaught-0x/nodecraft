## 2026-10-28 · Trusted browser mesh preparation in one Rust worker (#1013)

- The browser viewer uses a dedicated Rust/Wasm worker for every model, including small ones.
  There is no synchronous decode fallback. An unavailable worker affects previews, while the
  rest of the document remains editable. This supersedes the browser inline-decode policy in
  the original 3D-node decision; it does not add browser storage or materials capabilities.
- Trunk 0.21.14 generates the module worker loader, wasm-bindgen wrapper and Wasm from Rust.
  The three worker artifacts stay flat beside the main application. The fixed loader is
  resolved against the document/application base, with a build query. Ready and every reply
  carry the protocol and a source/build digest, so stale stable-name worker files fail visibly
  instead of being silently adopted. Hosts should revalidate stable worker files on deployment.
  No handwritten JavaScript, Blob bootstrap, eval, unsafe memory view or guest executable is used.
- There is one active worker and a FIFO of up to eight waiting model hashes and owner IDs.
  The queue holds no asset bytes. Same-hash requests share one job and cache entry. The worker
  and queue belong to the browser session, project, canvas and asset context. Replacing any of
  these drops both and resets the CPU preview cache. The active owner’s hash is checked before
  adoption. Waiting owners and hashes are checked before reading original bytes. Deleted and
  changed entries are discarded. Panning or zooming a card offscreen preserves its work.
  Waiting jobs schedule deadline checks and copy/adoption chunks request their next frame.
  Cancellation terminates the worker because synchronous Wasm cannot receive a cancel message
  while decoding. The desktop keeps its shared hash cache and one decoder across canvas changes.
- The worker receives insertion-verified immutable resident original bytes, never paths, URLs,
  keys or a store/session object. Owned JavaScript ArrayBuffers are copied from these originals
  and transferred; Wasm memory is never detached. The worker rechecks the original hash. The
  AssetStore original remains available for save, history and export.
- The browser profile is 16 MiB original bytes (the existing per-asset storage boundary),
  600,000 vertices, 300,000 triangles, 900,000 unique edges, 8,192 scene visits, depth 32, and
  four at-most-1-megapixel base-color textures, a 32 MiB per-texture decoder budget, and glTF pre-parse ceilings of 250,000 JSON values/16,384 objects. Existing vertex-color baking and opaque preview
  semantics stay in force. More geometry returns a named error; excessive edges omit wireframe
  like the native policy. This is intentionally smaller than the native decoder profile.
- Layout admission checks finite integral raw JavaScript numbers, exact counts/products and
  actual ArrayBuffer extents before casts, allocation or owned copies. Stale job identity is
  checked before geometry. A prepared model is private validated little-endian GPU bytes.
  Decode, bounds, unique edges and CPU vertex/index packing happen on the worker. Main-thread
  transfer and geometry validation advance by at most 256 KiB per frame, including finite
  attributes, index ranges and recomputed bounds, before a model becomes drawable.
- Resource accounting is logical ownership, not a process-RSS guarantee. At the maximum profile,
  raw geometry is 27.6 MB, packed vertex/index/edge bytes 34.8 MB, and edge words 7.2 MB.
  Raw plus packed plus retained edge words can coexist during preparation (69.6 MB); the bounded
  edge set is additional scratch and is dropped before packing. Encoded bytes, JSON/base64 and
  image decoder scratch, worker Wasm memory high-water retention, transfer staging and GPU
  allocations add further memory. Original storage retains its independent 48 MiB total cap.
  Main CPU prepared cache is capped at 64 MiB and all cache descriptors together at 256. A new model
  that cannot fit alongside current visible entries is refused, rather than repeatedly evicting
  and decoding them. Per-frame copy scratch is at most 256 KiB. GPU/target cache budgets are
  separate existing policies and must not be presented as a whole-browser memory cap.
- GPU creation and write_buffer remain on the main thread and cost proportional to admitted
  bytes. This is not zero-copy and does not guarantee a frame time on every device. Real browser
  conformance and upload timing are required; compile-only checks do not prove this boundary.
- Startup and decode/adoption deadlines are 15 and 30 seconds. Worker errors, messageerror,
  protocol mismatch, allocation refusal and timeout become named preview failures. The worker
  is terminated and callbacks/transfers are released on terminal paths and drop. Reload gives
  a deliberate retry. Generated worker files are lazy, so a missing loader cannot prevent the
  main application from starting.

Parent #569 remains open for retained materials/UV rendering and any larger browser
storage profile. The original synthetic conformance model is generated in Rust; no third-party
model, decoder code or screenshot is shipped. The conformance-only binary and fault probes are
feature gated and excluded from the release package inventory.

Detailed allocation paths, admission/lifetime overlap, texture/JSON/accessor scratch and test
coverage are tracked in the [resource worksheet](../browser-mesh-resources.md), together with
the packaged browser measurements and artifact digests.

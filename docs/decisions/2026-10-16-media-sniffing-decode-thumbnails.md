## 2026-10-16 · media: sniffing, decode, thumbnails (#56)

- **Formats are sniffed from bytes, never from file names**, and the sniffed MIME type picks the
  decoder (`ImageReader::set_format`), so a mislabelled file can't reach a different decoder.
- **`image` without default features**, decoding PNG, JPEG, GIF, WebP, BMP and TIFF. No `rayon`
  (wasm) and no AVIF/HEIC decode yet: those are sniffed and reported as unsupported. Animated GIF
  and WebP decode to their first frame.
- **Every decode is capped** (`media::Limits`: 256 MiB input, 16384 px per side, 1 GiB decoder
  allocations by default). The caps are checked from the header before pixels are allocated:
  most `image` decoders ignore `max_alloc`, so `media` reserves the decoded size (plus the copy an
  EXIF rotation makes) against it itself.
- **Images are upright:** EXIF orientation is applied on decode, and `ImageMeta` reports the
  displayed size.
- **`media::ContentHash` is BLAKE3**, matching the asset store's `assets/<blake3>` names. `media`
  can't depend on `types`, so `types`' hash type should convert from its `[u8; 32]`.
- **Thumbnails are RGBA8 with straight alpha**, cached by (content hash, max edge) in a
  memory-budgeted LRU (`ThumbnailCache`). The cache isn't thread-safe; its owner decides where
  thumbnails are made.


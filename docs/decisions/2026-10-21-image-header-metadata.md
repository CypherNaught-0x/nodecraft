## 2026-10-21 · Image header facts travel with references; absent means unknown (#393)

Stage A of #393: `MediaMeta` gains three optional image facts, read from the file's header. This
describes what an image already is; it converts nothing. The colour/HDR contract (Stage B of #393)
is still open, and the sRGB assumption in the
[CPU image reference](2026-10-17-cpu-image-reference.md) stands until it lands.

- **The fields.** `has_alpha` (the stored layout has an alpha channel, even if every pixel is
  opaque), `bits_per_channel` (as stored: 1, 2, 4, 8, 16 or 32) and `color_space`: `untagged`,
  `srgb`, `icc_profile` or `other` (a PNG `cICP`, `gAMA` or `cHRM` chunk). Each is an `Option`
  with `skip_serializing_if`, so a reference without them serialises exactly as before, and older
  NodeCraft builds ignore them.
- **Absent means unknown, never a default.** A missing field is never read as 8-bit, opaque or
  sRGB. Every project written before #393 has none of them, and nothing infers them on open.
- **A tag is only claimed when every common signal is read.** For PNG, a bounded walk over the
  chunk headers before `IDAT` (8 bytes read per chunk, at most 4,096 chunks) finds `cICP`, `iCCP`,
  `sRGB`, `gAMA`, `cHRM` and `eXIf`. For JPEG and WebP the decoder reports an embedded ICC
  profile. EXIF can name a colour space and isn't parsed, so a file with EXIF and no stronger
  signal gets no tag. BMP (v4/v5 colour spaces), GIF and TIFF (whose ICC tag the decoder can
  miss) get no tag either. A profile that can't be read still counts as `icc_profile`: calling it
  `untagged` would claim more than the file proves.
- **Recorded at import, verified at write.** Import fills every fact the header proves, through
  one mapping (`nodecraft_format::image_header_meta`). The format crate's two verification paths
  (in-memory and streamed) compare each claimed fact with a fresh header probe through the same
  mapping, and refuse a contradiction, on save and on open, as they already refused wrong
  dimensions. An unclaimed fact is not checked. Both probes (bytes and bounded reader) read the
  same signals, so they never disagree.
- **Who fills them.** Only import, for now. Action, split, timeline and provider outputs record
  only their size, as before, so their facts are unknown. Filling them is additive and needs no
  format change.
- **No pixel changes.** Decoding, thumbnails and the GPU upload stay RGBA8 straight alpha, and no
  golden fixture changes. The tag records what a file says; NodeCraft still decodes every image
  as sRGB and applies no profile. Applying one is Stage B's job.

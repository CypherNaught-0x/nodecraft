## 2026-10-18 · HDR recognition without decoding (#394)

- Leading OpenEXR magic `76 2f 31 01` is recognized as an image with the conventional,
  unregistered MIME `image/x-exr`. The [OpenEXR layout](https://openexr.com/en/latest/OpenEXRFileLayout.html)
  defines those four bytes. [Magnum's OpenEXR documentation](https://doc.magnum.graphics/magnum/classMagnum_1_1Trade_1_1OpenExrImageConverter.html)
  documents this MIME convention. We do not call generic EXR `image/aces`: the
  [registered ACES container](https://www.iana.org/assignments/media-types/image/aces)
  imposes additional version and attribute requirements that four bytes cannot prove.
- Exact leading `#?RGBE\n` and `#?RADIANCE\n` lines are recognized as
  `image/vnd.radiance` before the text heuristic. This includes header-only text. It is a
  recognition heuristic, not validation of image content: Radiance also uses headers for other
  binary formats. See the [registration](https://www.iana.org/assignments/media-types/image/vnd.radiance)
  and [maintainer's explanation](https://discourse.radiance-online.org/t/correct-magic-number-for-hdr-files/6299/2).
  Prefix prose, truncated lines and CRLF variants do not match these exact signatures.
- Recognition grants no decoder or storage permission. Probe/decode return `Unsupported` with
  the recognized MIME. File drops skip these images with an HDR notice suggesting PNG or JPEG;
  they are not silently stored or turned into text. Existing HEIC/HEIF/AVIF deferred storage is
  unchanged. Provider conversion refuses an entire result containing these formats during
  borrowed admission, before copying, hashing or producing any asset, and retains the caller's
  original paid result on failure.
  `media::is_undecoded_image` states that boundary for the two ComfyUI paths that only ask "is this
  an image?": a run output in one of these formats is skipped like a latent (so a PNG saved
  alongside it still lands), and such an input is refused before `/upload/image`, never written to
  the user's server.
- This changes public recognition/error classification, not project bytes, codec features,
  colour management or HDR/ACES support. Decoding and colour policy remain separate (#393).

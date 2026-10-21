## 2026-10-21 · fal maps Wan 2.2 image and speech to video (#607)

The fal adapter pins two more live aliases, as their own catalog models rather than modes of
`fal.wan22`: `fal-ai/wan/v2.2-a14b/image-to-video` (`fal.wan22-i2v`) and
`fal-ai/wan/v2.2-14b/speech-to-video` (`fal.wan22-s2v`). Each fal endpoint is a separate model id
with its own schema and price, so one catalog model per endpoint keeps the adapter's per-model
parameter whitelist and the quote honest. All three share the `fal-ai/wan` queue root.

- **Roles map to fields, not positions.** `first_frame` and `portrait` go to `image_url`,
  `last_frame` to `end_image_url`, `speech` to `audio_url`; the existing `image` role stays for Trellis
  and ESRGAN. A role the model doesn't own, or one sent twice, is refused before anything is
  encoded.
- **Speech is sniffed, not trusted.** The header must sniff as WAV, MP3, Ogg or M4A and match the
  declared type; that list is NodeCraft's own (fal's docs show only WAV). Only the header is
  checked, before any image codec runs: the track isn't decoded, so a truncated one still goes
  out. The clip's length comes from `num_frames` (40–120, multiple of 4, at 16 fps), so the mode
  says `duration = "fixed"`, and speech longer than the longest clip, 7.5 s, is refused before
  sending rather than cut somewhere unknown.
- **The soundtrack is unverified.** The speech mode declares `sound = "input"`, which is what the
  endpoint is for, but no paid run has confirmed the MP4 carries the track. The model
  description says so until a live test does.
- **The request cap rose from 2 to 4 MiB** so two full 1 MiB inputs fit after base64. Inputs stay
  capped at 1 MiB each and at 1024 px, and the encoded size is still counted exactly before any
  codec runs.
- **`application/octet-stream` declares nothing** for a clip, from any of the three Wan models,
  `fal.wan22` included: fal labels some videos that way.
  The downloaded bytes are still sniffed as MP4. Any other declared type that isn't
  `video/mp4` is refused, and non-video outputs keep their exact type check.
- Defaults stay cheap: 480p, interpolation off, image-to-video's aspect ratio follows the frame.

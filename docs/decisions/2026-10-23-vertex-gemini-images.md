## 2026-10-23 · Vertex AI image generation runs the Gemini image models (#47)

- **Gemini image models, not Imagen.** Issue #47 asked for Imagen on Vertex AI, but Imagen 4 is
  discontinued there. The adapter `google-gemini-images` runs the Gemini image models through
  `generateContent` instead: 3.1 Flash Image, 3 Pro Image, Nano Banana 2.1, 3.1 Flash-Lite Image
  and the deprecated 2.5 Flash Image. It is a separate adapter from the Gemini API's
  `gemini-images`. That one uses a Gemini API key; this one signs in with the `google-cloud`
  service account that Veo and Google TTS share, and bills the account's project.
- **Global endpoint.** The Gemini 3 image models are served only at `locations/global`, so every
  model runs there, unlike Veo's `us-central1`.
- **Sizes.** Only the documented `imageConfig.imageSize` values 1K, 2K and 4K are offered. 3.1
  Flash's 512 size is omitted until Vertex AI documents it.
- **Price before, tokens after.** The quote is one image at the chosen size, from the pricing
  page's tokens per image. To support that, `per_unit_by_choice` gained an optional
  `default_count`: a fixed count used when no parameter supplies one (`count` is now optional). A
  finished run is charged from `usageMetadata` with the catalog's `usage` rates: prompt tokens at
  the input rate, reply tokens at the image output rate. This counts reference images and replies
  with several images. It is slightly high for the reply's text tokens and slightly low because
  thinking tokens, billed at the text rate, are left out. Exact per-modality billing would need a
  usage breakdown the engine doesn't carry.
- **Several images per reply.** Gemini may return more than one image in one candidate; every one
  is kept as an output rather than dropped.
- **Filtered replies count as paid.** A blocked prompt or a reply whose images were filtered is
  blocked content, but it arrived on a 200, so it isn't reported as refused: Google may have billed
  it. Only documented HTTP refusals are marked refused. The submit is never replayed after a lost
  reply.

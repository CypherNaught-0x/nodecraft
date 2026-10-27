## 2026-10-27 · Offer Ideogram V3 through the existing fal queue (#34)

Use fal's explicitly named `fal-ai/ideogram/v3` endpoint rather than add a direct-provider
credential, factory or transport. The existing async queue owns polling, saved handles,
cancellation, redacted authentication and bounded result downloads. This follows the issue's
preference to reuse an aggregator when it covers the model.

The catalog offers one Balanced image per call. The typed request fixes both rendering speed
and image count, and refuses overrides before HTTP, keeping the dated 0.06 USD estimate valid.
Size presets, style, MagicPrompt, optional seed and negative prompt do not change that tier.
The seed range is a conservative signed-32-bit subset. Catalog costs remain estimates; absent
monetary usage from the provider does not become an observed invoice. Ambiguous creates
retain fal's no-replay policy; only an accepted validated handle can resume.

The output is a generic file list containing exactly one image. Optional metadata is checked
when present; PNG/JPEG/WebP bytes must pass bounded codec validation. Credentials are sent
only to the canonical queue API and never to the image CDN. Handles retain the selected model
and accept only its canonical `fal-ai/ideogram/requests/<id>` routes.

Sources checked 2026-10-27:
- [fal model and pricing](https://fal.ai/models/fal-ai/ideogram/v3)
- [fal request/output schema](https://fal.ai/models/fal-ai/ideogram/v3/api)

No live availability or invoice has been verified. Direct Ideogram API access and its other
model families, image edits and character references can be added under their own contracts.

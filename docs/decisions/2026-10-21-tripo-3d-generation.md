## 2026-10-21 · 3D generation is its own card, and Tripo is three fixed-price models (#22)

- **A Generate 3D card (`generation.mesh`)** joins the image and text cards. It exists when the
  catalog has a model with a mesh mode and makes one model per run (its output is not a list).
  Its modes may have no prompt role at all: image-only modes plan with just their images, where
  the image and text cards still require a prompt.
- **One catalog model per Tripo task type** (text, image, multiview). Catalog prices are per
  model and can't follow parameters, so each model fixes the settings that change Tripo's price
  (texture and PBR on, standard quality) and its estimate covers exactly what is sent. Detailed
  textures or untextured models would be further models, not settings.
- **Uploads are free and prove nothing was billed; the task request is the paid step.** With no
  idempotency key and no cancel endpoint, a submit is never retried automatically and cancelling
  only stops waiting.
- **The adapter downloads the result itself**, without the key, because Tripo's links expire in
  minutes and the engine refuses URL outputs.

## 2026-10-18 · Image previews on cards (#386)

- **Which image a card shows:** an Image card's asset, or the first image of another node's
  active result (`engine::split::image_of`, so splitting and previews agree on it). Types
  NodeCraft can't decode aren't previewed.
- **Layout:** cards with an image reserve `card_preview_height` (a theme token, 180) above their
  parameters, and the image is fitted inside it, keeping its aspect ratio.
- **Decoding never blocks the frame natively.** One worker thread decodes thumbnails with
  tighter limits than `media`'s defaults (512 MiB of decoder allocation), with at most four in
  flight. Until one is ready, the card shows "Loading preview…" and repaints when it lands.
  Browsers have no worker yet: images up to 512 KiB decode inline, and larger ones say
  "Preview not available here" until an async path exists (#319).
- **A bounded cache.** Textures are keyed by asset hash and size bucket (128, 256 or 512 px,
  picked from the card's on-screen size), so zooming in asks for a sharper one while the
  smaller one keeps showing. Textures are evicted least recently used past 96 MiB, never one
  used this frame. Decode failures are remembered ("Can't preview this image"), so a broken
  file isn't decoded again every frame.
- **Only what's seen:** culled cards and cards below the zoom where bodies hide request
  nothing.

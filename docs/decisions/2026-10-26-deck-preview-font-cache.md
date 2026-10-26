## 2026-10-26 · Share font payloads across Deck card previews (#695)

- Card previews share validated font payloads by content hash, independently of raster scale.
  Only fonts used by the first three displayed slides are requested. A zoom step can rerasterize
  a layer from retained bytes without rereading its font or copying its full resident payload.
- The cache retains at most 64 MiB and 256 entries, including failures. It admits at most 64 MiB
  of font reads in one frame, under the existing 16 MiB single-font limit. Failed reads are
  remembered while visible and retried on returning to view, resident arrival or explicit
  re-import. A new adopted document clears the cache.
- Entries used this frame or the previous frame remain resident. When a visible working set
  exceeds the allowance, additional fonts use Lato in the preview rather than cycling through
  all font files during every zoom step. A hover notice reports the fallback. Panning to fewer
  cards makes unused entries eligible for LRU eviction. Editor and export font selection and
  stored Deck data are unchanged. Faces are rebuilt only when a vector layer needs rendering.
- Logical preview reservation belongs to `card::layout::Metrics`, so card geometry, ports and
  the shell's thumbnail strip agree with the current layout.

## 2026-10-23 · Timeline source versions belong to rendered generations (#578)

- **Content identifies a source version.** A successful Timeline action records a bounded
  `Generation.source_versions` list: each rendered clip's stable id (`role`), its input asset's
  BLAKE3 content hash, and the result item index when fanned. Split clips each record their own
  role; pictures and sounds use the same rule. Disabled and disconnected clips contribute
  nothing to that output. The list comes from the actual materialized job recipe, so a source
  changed while rendering cannot rewrite its provenance. Failed/cancelled items contribute no
  records; partial-result completion preserves the completed items' records.
- **The result owns this evidence.** It travels with ordinary generation changesets, undo,
  project saves and recovery, rather than mutating the Timeline's config after rendering (which
  would itself make the result stale). The hashes do not pin the old media. There are at most
  65,536 records, unique per clip/item, with roles at most 64 bytes; the existing serialized
  generation/node limits also apply. Unlike optional provider job settings, these records are
  not shed to fit. An over-limit local render returns an actionable recording error.
- **List and show each clip's last use.** `timeline.list` adds `source_hash` (current
  wired media, or null), `rendered_sources` and `rendered_generation` (last use in history no
  later than the active result) to each clip. One bounded history scan groups the current roles:
  disabling/unwiring a clip and rendering again preserves its prior last-used record. Selecting
  older history never borrows evidence from a later result. Without an eligible recorded use it
  is unknown, never attributed to today's source; deleting/pruning that history removes its
  evidence as with other history metadata. Old optional-field readers may
  discard this evidence on resave; the frozen version-1 writer/reader refuses it, as it already
  refuses input hashes, item tags and provider settings. Equivalent cached outputs keep caching:
  cross-node copies re-plan only the recipient to bind the same content to its own clip ids.
- **Warn without changing stored edits.** Before any run, the editor marks enabled, wired video
  or sound clips whose stored start/end exceed the current known source duration. It uses asset
  metadata while waiting for the existing video probe, then prefers the actual measured length
  even when metadata advertises a longer source. Unknown lengths wait for measurement. Still display
  durations are not media trims. Regeneration keeps the trim/id, disabled clips and missing-wire
  clips remain listed, and the native renderer still validates against its actual probe before
  encoding. This warning starts no render and makes no edit. Invalid old trims remain editable
  without creating reversed numeric ranges.
- **Keep one staleness system.** Action generations already record the shared input hash and the
  canvas already renders stale badges for them. Tests prove Timeline config edits mark the
  existing output stale, undo restores freshness and a successful rerender clears it. Clip
  source hashes supplement that evidence; they do not create a second stale flag or change
  content-based freshness (an identical upstream rerun remains fresh).

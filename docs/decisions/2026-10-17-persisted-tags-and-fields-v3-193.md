## 2026-10-17 · Persisted Tags/Fields compatibility bridge (#193)

- Introduce archive v3 and NCRJ0003 before enabling Tags/Fields command and UI branches.
  Frozen v1/v2 contextual guards remain unchanged. Both older archive versions upgrade to a
  v3 checkpoint before adoption/append; journal replay authenticates original archive bytes.
- Persist Field rectangle/color/collapse and Tag color, plus compact SetNodeTags, SetNodeFields
  and absolute MoveNodes. Legacy fields default to (0,0,320,240), no color, expanded; membership
  remains explicit node data even outside the zone. Total bulk occurrences are capped at10000
  across the batch and inverse, independently of Comments' pin metadata and byte budgets.
- Preserve Comments' certified forward/inverse/actual-redo staging, relative pins and final
  world-position validation. Compatibility lands from tag-free Comments before feature UI,
  so no intermediate writer emits data rejected by its own declared version.

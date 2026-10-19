## 2026-10-17 · Local discussions, notes and format v2 (#68)

- Keep legacy root text and add defaulted flat replies, resolved state and optional node-relative
  anchors. Node deletion detaches discussions instead of deleting their content; undo stores only
  pin IDs/offsets beside the restored node/edges.
- Preserve valid-final transaction ordering. Structural batches intersecting original or incoming
  pin relationships (canvas plus node identity, including restored pin metadata) certify
  A→B→A→B and identical inverse spelling, preflighting actual redo under graph/history/journal
  caps. One extra clone/two probe applications are an explicit bounded tradeoff; move-only edits
  stay on their existing single-stage path.
- Retain 8 draft allocations/2 MiB String capacity with exact held Session identity and revision.
  Blur never saves; explicit Save/Cancel and document undo avoid retaining egui's 100 full-text
  draft snapshots. Inactive previews are bounded; focused editing still has transient egui copies.
- Notes use structure.note with no ports/jobs, not an executable source-text alias. Local-owner
  editing has no fabricated account/ACL metadata; #121 owns future central sharing permissions.
- Freeze Comments archive v2/NCRJ0002 with exact reviewed-v1 fixtures, original checkpoint-byte
  recovery identities and atomic conversion before new appends. Scalar Values and Node.config
  are v2 additions too. Later Tags/Fields integrate as v3 with lossless v1/v2 defaults; old headers
  reject their actual reserved roles even when values are null/default, without rejecting harmless
  unrelated extension objects. This avoids competing incompatible v2 writers.
- Gate exact serialized archive/recovery JSON on writes and replay admission too, so a future
  typed model cannot emit reserved semantic roles under a frozen header.
- Generate shell IDs as fallible UUIDv4 using native entropy or browser Web Crypto. Comment IDs
  check live roots/replies plus retained drafts; Notes use the independent NodeId namespace.
  Retry at most16 collisions, preserve document/history/drafts on failure, and keep randomness
  out of pure graph/types crates. Tags can adopt the same typed shell pattern in its own change.

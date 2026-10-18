# 2026-10-16 · Project archives and recovery (#54)

- **Stored ZIP with a narrow, versioned surface.** Format v1 accepts only a bounded manifest and
  canonical content-addressed assets. Metadata is checked before library parsing; no archive path
  is extracted. CRC and BLAKE3 checks catch both transport corruption and content-name mismatch.
  Pure codec and recovery replay run on wasm; native I/O stays behind `ProjectStorage`.
- **One publication boundary per operation.** Graph edits are staged, journaled, then published.
  Opening is an explicit session lifecycle replacement after complete validation, not a synthetic
  enormous undo transaction. Saving publishes a file before changing the session savepoint.
- **Dirty follows content state, not notification revision.** Undo entries restore state IDs;
  saves break coalescing so a drag cannot swallow a saved boundary. Status has its own cursor.
- **Recovery is append-only between checkpoints.** Checksummed sequential frames carry actual
  forward changesets and newly referenced asset bytes. Physical framing/length/checksum damage
  stops at the complete validated prefix, even if later frames survived. Recovery reports the
  first damaged frame's absolute byte offset and reason alongside truncated_tail; it never skips
  damage or replays later bytes. Invalid checkpoints and checksum-valid semantic/compatibility
  violations remain hard errors. Native append handles stay open and a worker group-syncs every 250 ms (plus scheduling/I/O
  latency); the latest window may be lost on power failure. Save/clean-close flush explicitly.
  There is no synchronous fsync for every drag move. Worker failures latch until a new checkpoint. Checkpoint identities include an advancing
  epoch; stale frames cannot replay onto a new checkpoint. Failed appends latch editing until save.
  Recovered snapshots are explicitly untitled and dirty to avoid overwriting a newer destination.
  Reading preserves the original bytes; native recovery latches damaged journals against append
  until a new checkpoint is published. Hosts must preserve/report the damage before replacing it.
- **Replay validates assets incrementally at every frame boundary.** The checkpoint establishes
  asset validity. Graph apply validates the frame's final graph; only surviving nodes with changed
  asset-bearing payloads are rechecked for missing bytes, MIME and dimensions. This includes
  AddCanvas snapshots, add/set/restore nodes, params/config/nested Values and generation history.
  Later frames cannot erase an earlier invalid reference. Same-batch replacements retain the
  existing valid-final semantics. Moves do no asset walk, but graph apply still clones/validates
  the document each frame: overall replay is not claimed to be linear in frames plus nodes.
- **Publication and durability differ.** `craft-atomic` reports a rename as successful even if
  directory sync fails afterward. We retain that contract. If subsequent recovery checkpointing
  fails, save still reports published and separately reports recovery unavailable.
- **Initial limits are conservative.** Archives cap at 64 MiB with 48 MiB aggregate media; buffering
  and shared immutable assets keep resource behavior explicit. Lazy disk-backed large projects
  need a streaming storage implementation before these limits can safely grow.

- **Clean close and asset lifetime are explicit.** Hosts flush/discard recovery after saving or
  an explicit dirty discard; dirty/crashed sessions retain recovery. Save-time GC retains live
  and undo/redo asset references. Per-user canonical destination locks coordinate cooperating
  native writers; only active/pending guards are retained, with no historical64-location cap.
- **Container checks are not media decoders.** Known signatures and dimensions from available image probes are
  checked; audio/video/document metadata needs later probes. Project hashes use types::Blake3,
  bridged to media::ContentHash at format. The bounded initial codec still needs a separately
  tracked file-backed/lazy asset implementation to meet plan 4.6 for large projects; see [lazy asset storage #147](https://github.com/CypherNaught-0x/nodecraft/issues/147), dependent on #54.

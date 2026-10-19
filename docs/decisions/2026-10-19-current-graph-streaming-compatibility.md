## 2026-10-19 · Preserve current graph families with native streaming (#359)

The landed Comments-v2 and Tags-v3 decisions own graph schemas 2 and 3. Lazy storage changes
container capacity, not supported graph features. This supersedes the frozen-v1-only claims in
the lazy storage and native wire-compatibility decisions: native small output uses current v3;
large/ZIP64 output uses v4 carrying graph-v3 vocabulary. NCRJ0004 alone selects its
80-byte envelope. Classic NCRJ0001/2/3 retain their 16-byte envelopes and original archive hash
through every replay frame; historical v1/v2 gates remain unchanged.

Native Open accepts v1/v2/v3 and v4; legacy graph snapshots are migrated before adoption.
Native recovery reports every classic envelope as requiring migration, and Session atomically
publishes an owner-bound NCRJ0004 checkpoint before any new append. Small/default hosts write
v3/NCRJ0003 under unchanged byte limits. Existing fixture bytes and provenance stay intact.
This preserves previous desktop saves and Comments/Tags rather than narrowing NativeStorage.

Wire family 4 is permanently reserved for streaming graph-v3 content. The next graph schema
must skip to 5; it must not reinterpret old v4 bytes using a newer typed graph. Both classic 3
and streaming 4 enter the current-v3 reader gate before typed parsing. If the writer/schema
declaration changes from 3, that gate refuses them until a real frozen-v3 reader/migration is
provided. This fail-closed guard does not claim current serde is an exhaustive frozen-v3 DTO.
Future schema work must also migrate classic archives before selecting newer checkpoint output;
the archive wire discriminator, graph vocabulary and checkpoint envelope remain separate.

The landed desktop recovery offer follows the same native dispatch: startup work detection
replays through its already locked host, and kept-copy offers/deletion qualification use
native recovery readers. They never buffer whole journals through the small byte decoder.
Offers retain private snapshot ranges after their reader closes, and Session adopts those
assets into its authentic live context before checkpointing or applying recovered work.

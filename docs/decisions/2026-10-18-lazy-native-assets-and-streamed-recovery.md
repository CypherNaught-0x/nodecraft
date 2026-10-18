## 2026-10-18 · Lazy native assets and streamed recovery (#147)

Native storage explicitly advertises a streaming capability and uses immutable typed archive,
checkpoint and replay plans. Frozen small v1 and NCRJ0001 remain bounded compatibility paths;
container v2 adds canonical stored ZIP64 and large extents while retaining the frozen-v1 graph
role gate. This version allocation does not implement the separate newer-graph format proposal.

Indexed descriptors are unverified. Native Open copies/revalidates a self-contained private base
before installing state, so external overwrite/deletion cannot invalidate recovery. Body checks
are deferred until verified consumption or persistent reference admission. Clones, readers,
prepared imports and undo/redo share exact logical identity accounting; dead caches are pruned,
retired files and cleanup debt keep their disk charges. A public logical collection seam does
not shrink recorded journal admission or promise immediate disk reclamation.

Use one serialized staging pool, pre-reserved base/journal generations, bounded cleanup slots
and retained actual File/OS-lock ownership. Same-file identity is only a fallible mismatch
check; protected program-owned paths and cooperative locks define lifecycle authority. Atomic
publication never invents an error after rename. Stored corruption requires checkpoint repair;
operational failure and caller-association conflicts remain distinct.

Unknown-length native readers stage/hash once in 64 KiB chunks under unchanged quotas and EOF/
Interrupted/cancellation bounds. Opaque Session batches bind the exact candidate and source stamp.
No default byte fallback may silently materialize a lazy/large plan; unsupported browser hosts
refuse before reading. See [storage.md](../storage.md) for APIs, exact bounds and limitations.

The ignored physical harness passed on native macOS with a real 4,294,967,297-byte asset and a
later entry at actual ZIP64 offset 4,294,968,519. Atomic failure after 4,294,967,296 successfully
forwarded file-write bytes (above u32::MAX) preserved the complete old destination/recovery and
its identity. The Session roundtrip, pinned-reader full hash/CRC and cold torn-tail recovery
passed. Virtual tests and small calibration remain separate evidence. These results do not
claim cross-platform physical execution, total RSS, durable device writes, whole media payload
decode or final coherent CI; [storage.md](../storage.md) records the limitations and remaining gates.

The existing small byte/drop helper participates in the same opaque candidate transaction (#334),
with unique hash/length admission and one paste Edit. It never uses logical pruning as rollback:
unrelated staged assets and old prepared authority survive refusal. The public pruning name
remains explicit guarded housekeeping. UI acquisition limits remain separate from native logical
capacity; blocking/hostile native acquisition is tracked separately in #335. Ordered skipped
results retain their existing input-count-proportional ownership rather than a fabricated fixed
whole-call memory bound.

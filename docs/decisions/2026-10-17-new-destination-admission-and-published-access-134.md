# 2026-10-17 · New destination admission and published access (#134)

- Canonical names for absent destinations are established by the actual publication, with no
  lowercasing or scratch-name normalization assumptions. A bounded per-directory operation gate
  covers every cooperating native prepare-open/save admission through guard adoption. Existing
  destination guards are acquired before writing; new destination guards use the published file's
  canonical path. Directory guards are never retained for a document's lifetime.
- Publication cannot be undone by a later identity/lock error. `save_with_access` returns explicit
  ReadOnly after such failures while preserving successful publication, new location/savepoint,
  and ordinary recovery-checkpoint handling. Future inplace writes are refused; independent
  Save As remains available. Writable nonlocking hosts retain the default adapter.
- The pinned upstream writability helper uses a PID-specific probe. A process mutex serializes
  all format-native calls rather than forking the shared helper or falsely treating a same-process
  probe race as evidence of a read-only directory. Other external helper callers are out of scope.
- Unresolved destination symlinks are rejected before admission: the atomic writer may follow
  a dangling link into a different parent, so admitting the alias parent would not serialize
  publication. Existing canonicalizable symlinks/chains and ordinary absent leaves remain
  supported. Metadata failures, cycles and missing parents fail before publication. Recovery
  path comparison retains its prior behavior; no upstream resolver or atomic writer is forked.

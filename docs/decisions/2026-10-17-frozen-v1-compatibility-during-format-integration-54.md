# 2026-10-17 · Frozen-v1 compatibility during format integration (#54)

- Version1 and NCRJ0001 remain frozen to08586e. Contextual raw read/write guards reject newer
  graph representations before typed interpretation or publication, with value-free Unsupported
  errors. Existing reviewed v1/v2/v3 migration fixtures and numbering remain authoritative.
- Memory-only sessions remain full current graph editors. Storage-backed sessions admit only
  v1-compatible prospective documents and canonical forward/inverse records before append;
  compatibility failures neither latch recovery nor publish document/history/status changes.
- A borrowed graph-role check avoids serializing large prospective documents, while shared
  parity cases bind it to the raw guard. Bounded record preflight adds a transient JSON buffer
  and parsed tree; it does not change durability semantics or claim zero-copy behavior.

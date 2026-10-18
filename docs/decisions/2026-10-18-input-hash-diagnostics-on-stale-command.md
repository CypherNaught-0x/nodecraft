## 2026-10-18 · Input-hash diagnostics on canvas.stale (Part of #254)

- `canvas.stale` now returns the known stale `nodes`, `unknown_total`, and `unknown` (null or
  `{node, cause, kind, reason}`). The count covers checked results with locally invalid input
  hashes; only active results with recorded hashes are checked. One deterministic first cause
  follows canvas NodeId order. More causes are counted, not retained as an unbounded error list.
- The reusable `exec::staleness::stale_report` uses the same sweep and InputHasher caches as the
  legacy `stale` set API. Source/router identity and cached reason survive without rerunning
  validation hooks. The affected result and actual failing node are separate UUIDs. Original
  source/target ErrorKind is preserved; invalid routers use InvalidInput.
- The existing 576-byte diagnostic reservation covers a message of at most512 bytes and64 bytes
  of inline metadata (two NodeIds plus Error). Admission precedes construction; global refusal
  remains sticky and yields no partial report. Successful hash bytes/domain/budgets are unchanged.
  The owned reason moves into JSON without another copy; the new diagnostic fragment has a finite
  4096-byte encoded upper bound. This is no whole-response or allocator-RSS bound.
- Reasons retain the existing UTF8 prefix/ellipsis, empty fallback and control/bidi normalization.
  `types::Error` requires public-safe messages. This is not detection of an arbitrary secret in
  a malicious native hook's message. No original Error Debug, node titles, raw config/params,
  provider responses or credential inventory is added to this surface.
- Unknown does not itself seed the stale set. Existing transitive stale propagation remains, so
  a result can be both locally unknown and downstream-stale. Legacy stale/quote behavior is
  preserved; completed paid results still survive with unknown hashes after hash failure. This
  wires the real shared command/control/MCP path, not a new desktop badge or standalone CLI run.
- This is only the diagnostic follow-up. Canvas/history copies, pending-output limits and durable
  original run/job identity remain separate work; #254 stays open.

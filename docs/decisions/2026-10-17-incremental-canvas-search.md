## 2026-10-17 · Incremental canvas search (#69)

- Search indexes persisted Text leaves and linked tag titles, with checked actual title fallbacks.
  Stored model identifiers match today; catalog display names wait for verified catalog integration.
  Unicode scalar lowercase literal matching is explicit, without normalization/full case folding.
- Work is incremental over borrowed text and bounded structural cursors. Only IDs, query/matcher
  state and bounded fallback labels survive; no full prompt/lowercase copies. A pending step belongs
  to its query generation and waits for complete results before wrapping. Idle frames do no scans.
- Search commands use a scoped live host capability, retaining document/history/event identity.
  Only the exact search editor's current/prior focus permits framing; unrelated editors, menus and
  gestures retain ownership. Opening search never steals an unrelated dirty editor's focus.



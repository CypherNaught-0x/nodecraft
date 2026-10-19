## 2026-10-17 · Staleness hashes content, within a budget (#213)

- **Upstream results count by content, not id.** A consumer's input hash covers the digest of
  the upstream active result's output on the port it reads. Result ids aren't immutable: `SetNode`
  can rewrite a result's outputs under the same id, or it can be removed and re-added. With
  content hashing, that makes downstream results stale, while an identical re-run upstream keeps
  them fresh. A result recorded for another port layout counts as "empty", as the planner
  treats it.
- **One query, bounded work.** Each node's recipe and each upstream output is digested once per
  query (memoized). Values are streamed into the hasher instead of being serialized. Every byte
  is charged against a 256 MiB budget (`staleness::MAX_QUERY_BYTES`), and a canvas past it gets an
  actionable error from `stale` and `canvas.stale`.
- **Older recipes read as stale, deliberately.** The stored hash is still a bare Blake3 (no
  format change), and its domain tag moved to `v2`. A result stamped under another tag can't be
  told from one whose inputs changed, so it is reported stale: we never show a result as fresh
  when we can't vouch for it. The producer that stamps hashes isn't wired yet, so no saved
  project carries `v1` hashes.
- **`input_hash` is a new semantic field in saved generations.** Archive and journal versions
  that accept it, and the guards that make old readers reject it, are coordinated in #134 and
  #162. This change doesn't alter the stored field.
- This supersedes the #75 entry's "identity of an upstream node's active result" and "old hashes
  become unknown".


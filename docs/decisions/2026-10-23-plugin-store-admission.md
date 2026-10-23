## 2026-10-23 · Plugin store admission: negotiated versions and reserved namespaces (#172)

- **Negotiate at import and again at enable.** The store runs the contract's `negotiate()` with
  this build's `HostVersions` on every incoming package (dry runs included) and on every enable.
  A package whose protocol major differs, whose minor range doesn't overlap, whose descriptor
  contract is newer or of another major, or which requires a feature this build lacks is refused
  as `Unsupported`, never installed to fail later. Unknown *optional* features are ignored. A newer
  NodeCraft may have put a package into the shared store: enable negotiates a disabled one again,
  an enabled one is left out of what this build runs, and enable refuses a dependency it can't
  use. The signature checked is the kept archive's when the digest is already kept, since the
  digest doesn't cover `signature.json`.
- **NodeCraft's namespaces need a trusted signature.** Package IDs equal to or under `nodecraft`
  or `io.github.cyphernaught0x` install and enable only when a key the trust file trusts signed
  them (`Forbidden` otherwise, also for a known-but-untrusted key). A DNS-looking name proves no
  ownership, so this keeps an unsigned package from passing itself off as part of NodeCraft.
  Registered IDs were already namespaced under `plugin.` and host IDs can't use it.
- **Dependencies may run code, under their own grant.** The RFC's "data packages only" rule (still
  in `lock::resolve`) is not applied by the store: since #173 a dependency must itself be enabled,
  and an executable one only under its own grant; revoking its last live grant for the active
  digest disables it and its enabled dependents. Locks
  pin exact content digests, so a cycle would need two digests each covering the other: the store
  needs no cycle check, and its depth walk keeps a seen-set regardless.
- **Failure on a real disk.** A unix test makes the folder backend's `objects/` and then its root
  read-only: a failed archive write and a failed index write each leave `index.json`, the open
  store and the first package as they were, and the store works again afterwards.

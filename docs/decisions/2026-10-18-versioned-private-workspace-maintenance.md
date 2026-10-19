## 2026-10-18 · Versioned private workspace maintenance (#363)

Private native assets receive a versioned root-matched lifetime marker held by their workspace
Arc, including retained sources/readers/captures. Payload handles and cleanup permits close before
the final lock. Recovery is self-contained; an unlocked valid new-protocol root can therefore be
reclaimed without inspecting asset bodies. Legacy roots and unknown/partial contents remain
untouched; upgrading does not grant deletion authority over old directories.

Use a permanent content-agnostic parent gate, freshly independently opened and exclusively locked
for the entire creation/sweep/marker-retirement interval. Never unlink/recreate it or share/clone
an admission handle. Same-parent contention refuses without waiting; independent parents can
progress without a global mutex/registry. Windows marker contents are read only after exclusive
locking, while pre-lock metadata/type/reparse/length checks remain conservative. PID is not proof.

Bound startup work to4,096 parent entries,64 inventory ATTEMPTS and256 payload unlinks. Validate
all known flat entries before deleting any; charge rejected inventories too. Fixed kind/u64
records retain no per-file heap path; actual inventory backing stays within512KiB and reusable
paths are bounded separately. Limits are per operation, not whole-process RSS or filesystem-time
promises. A named basename-only maintenance API provides a bounded fallback beyond the startup
window; no eventual scan progress or new UI/CLI routing is claimed.

Close the marker before unlink, keep parent admission held, retain valid markers on partial
payload failure, and leave unmarked residue after failed final retirement without inventing new
authority. Never recursively remove unknown files or sweep source/recovery/destination paths.
The cooperative private namespace excludes hostile same-user replacement; same-file diagnostics
do not turn that limitation into a portable no-follow guarantee. See [storage.md](../storage.md)
for exact APIs, bounds, legacy policy, reports and runtime qualifications.

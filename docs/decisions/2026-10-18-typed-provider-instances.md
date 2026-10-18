## 2026-10-18 · Typed provider instances and captured routing (Part of #275)

Provider ownership uses an adapter string and an optional opaque 16-byte instance UUID.
None preserves the legacy default; Some, including the all-zero UUID, is a separate
namespace. Adapter strings remain arbitrary bounded legacy strings. Encoded strings cannot
prove ownership: all provider scheduler lanes use canonical `p:<adapter hex>:d` or
`p:<adapter hex>:i:<instance hex>` names, while deterministic work retains `local`.

The host explicitly registers and selects targets in ProviderRouting. Configuration has a
separate 64 MiB logical budget and 100,000 combined binding/selection count. Charges include
independent binding shells, Arc headers, compact adapter ownership, one compact outer adapter key, inner instance payloads and a 48-byte
logical allowance per BTree entry. Borrowed adapter/instance lookups require no target copy. These are logical ownership charges, not allocator/RSS claims;
native adapter internals behind the existing Arc remain trusted host ownership outside them.

Session consumes routing privately and mints a fresh fallible nonce on installation. Generic
resource replacement does not change that authority. Quote binding v3 covers the routing nonce
alongside the existing live source/catalog/request identities. The same pure TargetSelection
reaches the first eager/source planning hook and deferred hooks. Every generation job matches
its step's selected target before any job prefix is registered. The legacy empty-view planner
and unadmitted Run remain compatible with source work.

PreparedRun retains selected independent Binding Arcs only, charging every retained occurrence
under the existing 64 MiB witness budget. A binding has no root/back-reference, so replacing a
large registry releases unrelated adapters. Outside dispatch, authorization rechecks live
source/catalog/routing/consent/budgets and transfers those same objects into AuthorizedRun.
ProviderRunner receives that captured binding, preserving attempt/incarnation/retry accounting
while handles, failures, permits and restored generations use the full typed identity. Reusing
an instance UUID for a different physical endpoint remains an unsupported trusted-host mistake;
this is not endpoint attestation.

Usage rows and metadata retain the optional instance. Old string-based charge/ownership/settle/
release/restore methods address defaults only. The first admitted named hold sets sticky provider
identity version 1 before invocation; cancel/drop never downgrades it. That version is separate
from historical open-session ownership. Identity 0 preserves its old ownership 0/1/2 algorithms;
identity 1 rejects ownership 0 and permits established 1 or immutable default-only quarantine 2.
Established named open money adds to the retained legacy named floor, never consumes it.
Legacy CSV rejects a selected stream containing scoped rows before its header; explicit scoped
CSV and engine paging retain a provider_instance column.

Retired identities remain finite probabilistic deduplication, never monetary ownership proof.
Default hashes preserve the old 16-byte algorithm; named hashes use a distinct framed BLAKE3
256-bit domain and a scope tag. A250000-key FIFO and RandomState HashSet stage only actual
retirement deltas. B = 0 does no retired-history work; ordinary B = 1 does not scan/copy N. Bounded
input discovery, journal, overlay and new-tail owners are charged explicitly; growth builds
replacement containers off-ledger and commit allocates nothing. Reported capacity guards bound
logical fixed-key ownership, not private table buckets or RSS. Hashing has standard expected/
amortized CPU guarantees, with rare growth/churn rebuilds; no adversarial worst-case hash bound
or mathematically collision-free digest claim is made. Named finalization still moves admitted
metadata without allocation. Old wire digests remain 32 hex; sticky version 1 uses one tagged FIFO
with 32 hex default and 64 hex named digests and rejects mixed/unknown representations.

This addresses requirement 2 only. Catalog admission, output contracts and the composed retry
ownership are preserved foundations; finite mock scripts, real-driver golden harnesses and
other #275 requirements retain their own scope and review.

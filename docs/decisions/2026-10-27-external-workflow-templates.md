## 2026-10-27 · Independent workflow catalog with a pinned offline snapshot (#953)

The official authored `.ncraft` workflows live in `CypherNaught-0x/nodecraft-templates`, with
MIT OR Apache-2.0 licensing, fixture provenance and filtered assets-prefix history from #952.
The external repository is authoritative. A dependency-free Rust data package supplies static
index/archive bytes, pinned by a full Git revision and lockfile in NodeCraft. Native and wasm
binaries embed that snapshot; there is no build script, submodule, copied archive directory or
build-time network refresh. Normal Cargo dependency acquisition is separate from app startup.

The public index requires schema_version 1 and bounded id/title/description/version/features,
BLAKE3 digest, root canvas and canonical immutable archive path. Content changes require a new
positive version and distinct canvas IDs; old published paths remain available. An append-only
history.json ledger contains every published record (bounded at 1 MiB/1024 records). CI compares the
complete previous ledger, checks exact archive inventory and validates every historical graph's
root/auxiliary canvas IDs against all other versions; adjacent-index comparison cannot protect v1
after v2 has been replaced by v3. Current index descriptions may be refined without rewriting the
historical record, while immutable identity fields must match. Runtime clients fetch only the index
and selected body, never this authoring ledger. External CI checks
out an immutable NodeCraft validator commit and calls the same production schema, digest,
ordinary-format asset/graph and kind validation as the client. It has negative proofs for malformed
content and mismatched digests. It does not copy a competing schema implementation or make the
static data package depend on engine code, avoiding a Cargo dependency cycle. CI publication pins
must remain explicit; merge commits preserve the filtered history and bundled revision ancestry.

Refresh retrieves only the fixed official public index, asynchronously. A second explicit command
retrieves one selected archive. Neither command asks for keys, edits a graph, starts a provider job
or imports on completion. Native transport refuses redirects; browser Fetch owns its public GET
redirect policy. Body and timeout limits apply before admission. Unknown schemas, invalid content
or an unavailable repository leave bundled and last-good versions usable.

A Session resource owns at most 64 MiB/128 verified downloaded archives; old cached and compiled
versions remain selectable when a newer indexed body is unavailable. This cache is in memory on
both platforms, with no new filesystem authority or persistent personal-library scope. One host
request runs at a time (128 KiB index or 2 MiB stored archive), and synchronous decode/import
preparations are bounded by the stored-archive limit. The Catalog is not Clone, the worker never
copies the complete cache, and no caller-visible lease/token can accumulate hidden evicted bodies.
A late queued/completed request belonging to another Session is discarded.

The list carries a catalog stamp and exact id/version/digest identities. UI cache lifetime is also
bound to Session identity. Imports carry an opaque admitted entry/snapshot into existing atomic
PreparedImports and AddCanvas changesets without resolving a mutable latest ID again. Once added,
assets and graphs are normal savable project data, independent of catalog/cache lifetime. CLI/MCP
and UI use the same commands; unsupported hosts retain the bundled set without opening dialogs.

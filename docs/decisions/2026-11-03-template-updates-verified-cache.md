## 2026-11-03 · Template updates: digest trust from the HTTPS index, a persistent verified cache (#954)

Builds on [the external workflow catalog](2026-10-27-external-workflow-templates.md) and replaces
its "the cache is in memory on both platforms" rule for the desktop app.

**Does the index need a signature? Not yet; it is optional and checked when a key exists.** The
templates repository has no signing key, no secret in its CI and no release job that could sign an
index, and the update feed's key (#540) signs NodeCraft binaries, not community-reviewed workflow
data, so it must not be reused for it. A signature check with no published key would verify
nothing and only suggest trust that isn't there. The trust model is therefore:

- **The index is trusted for what it names, because of where it comes from.** It is read only from
  the configured templates repository (the official `CypherNaught-0x/nodecraft-templates` raw URL),
  only over HTTPS, with redirects refused on native, the generic `NodeCraft/<version>` User-Agent
  and no other header, cookie or identifier. Changes to that repository are reviewed pull requests
  whose CI validates every archive and digest against the same production validator; the index is
  the signed-off record of that review.
- **Each template is trusted only through its index digest.** Every downloaded archive is hashed
  (BLAKE3) and compared with its index entry before it is decoded, stored or listed; it is then
  decoded and its graph, assets and node kinds validated. A mismatch is rejected, reported in the
  update outcome and in `templates.status`, and never written; the last good copy (cached or
  bundled) stays selectable. A published `id`/`version` whose digest changes is refused as a
  conflict, so a later index cannot silently rewrite a version people already have.
- **Optional Ed25519 index signature.** `ed25519-dalek` is already in the tree for the update feed.
  A `Source` can carry template-index keys; when it does, `index.json.sig` (64 raw bytes, signing
  `NodeCraft templates index v1\n` followed by the index bytes) must verify against one of them or the
  update is refused. The official source carries no key today, so official updates are unsigned.
  The verified signature is cached as `index.json.sig` beside the cached index, and a keyed host
  verifies it again when it reads the cache; an index without a valid cached signature is ignored.
  Adding a key is a follow-up for the templates repository (a signing secret in its publication job)
  plus one constant here.
- **Requirement for that follow-up: rollback protection.** A signature alone lets anyone who can
  serve the URL replay an older signed index, hiding newer versions. Before a key is published, the
  index must gain a monotonic `serial` (a new `schema_version`), covered by the signature, and the
  client must refuse an index whose serial is lower than the cached one's. Today the official source
  is unsigned, so trust already rests on HTTPS to the fixed repository and this adds nothing to
  exploit.

**Bounds.** The index is at most 128 KiB and 128 entries, each archive at most 2 MiB, every request
times out after 10 s, and one `templates.update` downloads at most 16 changed archives (the rest are
reported as skipped; run it again). Hostile input is an error: unknown schema, bad paths (the URL is
built only from a validated `templates/<id>/<version>/<blake3>.ncraft` path under the fixed
origin), oversize bodies, wrong status, wrong digests and damaged archives.

**Explicit only.** Nothing touches the network at startup or in the background. `templates.update`
(the Library's **Update templates** button) runs one bounded update; `templates.status` reports the
last outcome, the update in flight and cache problems. There is no automatic-update setting; one can
be added later as an opt-in. `template.refresh`/`template.retrieve` remain for single steps.

**Cache.** Verified archives and the last good index are written under the app data directory
(the settings folder) in `templates/v1/`: `index.json` (with `index.json.sig` in signed mode), and
per archive `<blake3>.ncraft` with its index record `<blake3>.json`. The `v1` directory versions the
layout; another version's directory is ignored, and clearing removes only `v1/`. A cache read admits
at most 258 ordinary files (128 archives with their records, the index and its signature; hidden and
temporary files don't count), 64 MiB of archives and per-file caps. Each file is checked to be an
ordinary file (`symlink_metadata`) just before it is opened; a file swapped in after that check is
still hashed against its record, so it can't be admitted. A pair left incomplete by a cut-off write
or removal (a 64-hex `.ncraft` without its record, or a record without its archive) is removed at
the next start instead of being reported every time.
Every archive is hashed against its record's digest, decoded and its node kinds validated again, so
`template.add` never sees an unchecked cached graph. The cached index is installed first; a cached
archive that contradicts it, a bundled version or another cached copy of the same version is dropped
and removed (the index wins), so a stale copy can't block later updates with a conflict. A damaged,
oversize, renamed or unreadable file is skipped and reported, never a panic, and the bundled set is
always the floor. `templates.clear_cache` (or deleting the folder) removes it. Cached templates are
read-only: adding one copies its canvas and assets into the project through the same undoable
`template.add` import as a bundled one.

**Off the UI thread.** The desktop's template worker thread owns the file store: it reads and
verifies the cache at startup and hands the verified records back (`disk::load` / `disk::adopt`),
and the session only queues writes (`disk::Write`) that the worker applies in order after each
update, eviction or clear. `templates.update` waits (Busy) until the startup read is adopted, so it
never re-downloads what the cache holds. A read that panics or a worker that stops hands back an
empty, failed read, so that wait always ends. A clear empties the session at once and reports the
folder removal only when the worker has run it (`templates.status` `cache.clearing`); a failure is
shown as an error, never as success. Writes still queued when the app exits are lost; the cache
only loses downloads that the next update fetches again. Hosts without a worker (tests, a failed worker start)
use `disk::install`, the same steps inline.

**Transport.** The same `ReqwestHttp` as the update check (#123), with redirects off on native. Not
`ReqwestHttp::public_only()`: it disables system proxies, which would break updates behind a proxy
for a fixed public GitHub host where address checks add nothing that the digests don't already give.
Filesystem access is a `TemplateCacheStore` (a file store on native, memory in the browser and
tests), so core crates stay wasm-safe. The browser build runs the same update with an in-memory
cache; GitHub's raw host CORS behaviour has not been verified live and is a follow-up. Debug builds
may point the desktop at a local fixture server (`NODECRAFT_TEMPLATES_FEED`, loopback HTTP only);
release builds read only the official repository.

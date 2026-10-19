## 2026-10-19 · Plugin data packages: archive format and local store (#172)

Phase B of the [plugin RFC](../plugins-rfc.md) starts with the `plugin-store` crate (L1).

- **Archives are strict stored ZIPs, checked on the raw bytes.** A `.ncraft-plugin` file is a ZIP
  of stored (uncompressed) regular files laid end to end from the first byte, then the central
  directory and an end record without a comment; every central record matches its local one
  (name, flags, method, CRC, sizes). Duplicate names, directory entries, links, encryption, data
  descriptors, ZIP64, multi-disk archives, comments and the Unicode path/comment extra fields are
  refused, and the entry count is checked from the end record before any entry is read. So one
  archive has exactly one reading (no ZIP reader can see other files), a declared size can't hide
  a bomb, and entries are read straight from the checked layout (file integrity is the
  inventory's BLAKE3, not the ZIP CRC). Unportable or case-colliding names, unlisted or missing
  files and hash mismatches are refused next. A package needs a `LICENSE`, `LICENCE`, `COPYING`
  or `NOTICE` file at its root.
- **Data packages only.** `rust_wasm` packages are refused as unsupported until phase D.
- **The store keeps verified archives by digest and one index.** The index (canonical JSON,
  versioned) names every kept digest and the active digest and state of each package. Every
  change is made on a copy and written atomically before it takes effect; a failed write changes
  nothing. Before writing, the store checks the index is still the one it read, so a second
  NodeCraft (app and CLI) is refused rather than overwritten. Files the index no longer names are
  removed when the store is next opened, after the index dropping them is on disk. A digest
  already kept is never overwritten by a differently built archive (the digest covers the files,
  not the signature or ZIP bytes); the kept one is what counts. Archives are re-verified whenever
  they are loaded.
- **Lifecycle.** Import installs disabled (or only reports, as a dry run), and refuses a package
  spelling an ID another installed package registers, so no two installed packages share one,
  whichever is enabled first. Enable checks the signature against the trust file again (a key
  revoked since import blocks it), requires every dependency installed at its locked version and
  digest *and enabled*, and stages registrations against the host's IDs and every installed
  package's: any collision enables nothing. Entries
  bound to a host implementation the host doesn't allow are reported and stay off. Disable is
  refused while an enabled package depends on it. Update and rollback need the package disabled,
  keep the previous digest, and leave it disabled. Uninstall keeps digests projects still
  reference and installed packages lock (and names those dependents); `prune` drops old versions
  nobody uses. Nothing here edits a project.
- **Backends**: a folder on native (`index.json` and `objects/`, written aside and renamed over)
  and memory (tests; the browser build, where packages last for the session).

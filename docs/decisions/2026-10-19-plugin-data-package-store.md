## 2026-10-19 · Plugin data packages: archive format and local store (#172)

Phase B of the [plugin RFC](../plugins-rfc.md) starts with the `plugin-store` crate (L1).

- **Archives are stored ZIPs.** A `.ncraft-plugin` file is a ZIP whose entries are stored
  uncompressed (like `.ncraft` projects), so a declared size can't hide a decompression bomb and
  no inflate code is needed. Entries are read with a cap one byte past their declared size; links,
  encryption, unportable or case-colliding names, unlisted or missing files and hash mismatches
  are refused before anything is kept. Directory entries are allowed and ignored. A package needs
  a `LICENSE`, `LICENCE`, `COPYING` or `NOTICE` file at its root.
- **Data packages only.** `rust_wasm` packages are refused as unsupported until phase D.
- **The store keeps verified archives by digest and one index.** The index (canonical JSON,
  versioned) names every kept digest and the active digest and state of each package. Every
  change is made on a copy and written atomically before it takes effect; a failed write changes
  nothing. Archives are re-verified whenever they are loaded.
- **Lifecycle.** Import installs disabled (or only reports, as a dry run). Enable requires every
  dependency installed at its locked version and digest *and enabled*, and stages registrations
  against the host's IDs and every enabled package's: any collision enables nothing. Entries
  bound to a host implementation the host doesn't allow are reported and stay off. Disable is
  refused while an enabled package depends on it. Update and rollback need the package disabled,
  keep the previous digest, and leave it disabled. Uninstall keeps digests projects still
  reference. Nothing here edits a project.
- **Backends**: a folder on native (`index.json` and `objects/`, written aside and renamed over)
  and memory (tests; the browser build, where packages last for the session).

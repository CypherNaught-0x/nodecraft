## 2026-10-18 · Backup container: XChaCha20-Poly1305 STREAM with Argon2id (#182)

- **Own container, not ZIP.** A backup must stream entries of any size (media up to 64 GiB)
  through authenticated encryption with integrity checked before anything is published. ZIP's
  encryption is weak or proprietary, and its central directory sits at the end. A small
  versioned container with a manifest first, then the entry bytes, does both. `.ncraft` projects
  stay ZIP and go into a backup as entries.
- **Crypto choices.** We use the RustCrypto crates `chacha20poly1305` 0.11 and `argon2` 0.6:
  maintained, MIT/Apache-2.0, pure Rust, and they build for wasm.
  - XChaCha20's 24-byte nonce takes a random 19-byte prefix per backup, plus the STREAM counter
    and last-chunk flag. That rules out nonce reuse, chunk reordering and truncation.
  - The header is authenticated as associated data.
  - Argon2id runs at 64 MiB and 3 passes for new backups (above OWASP's minimum). Readers accept
    at most 256 MiB and 6 passes, so a hostile file can't demand huge memory or time.
- **Unencrypted stays checked.** Without a passphrase, a trailer hash over the header and payload
  plus the manifest's per-entry hashes still catch truncation and accidental corruption, but not
  deliberate edits (the hashes have no key). The format and the UI label such a backup as
  readable and editable by anyone.
- **Pure container.** The format crate takes randomness (salt, nonce prefix) from the caller and
  touches no files, so it builds for wasm. Selection, size estimates, staging, atomic publication,
  restore previews and commands (`backup.create`/`inspect`/`restore`) are the host and engine
  phase of #182. They depend on the native storage in #147/#348 and the library in #96.

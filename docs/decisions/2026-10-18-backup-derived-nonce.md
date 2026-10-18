## 2026-10-18 · Backup nonce prefix derived from the passphrase and salt (#385)

- **Only the salt is caller randomness.** The first container design took both the Argon2id
  salt and the XChaCha20 STREAM nonce prefix from the caller. A host that reused both with one
  passphrase would reuse the keystream. Argon2id now outputs 51 bytes: the first 32 are the
  key, the last 19 the nonce prefix. A fresh salt gives a fresh key and a fresh prefix, and
  nothing the caller picks can make two backups share a nonce under one key.
- **Sound to split.** Argon2's variable-length output is pseudorandom across every byte, so key
  and prefix are independent, the usual "KDF to key and IV" pattern. Reusing a salt with the
  same passphrase still repeats both; hosts take the salt from the platform's random source.
- **Still version 1.** The container was not in any release (`v0.1.0-rc.1` and `v0.2.0-rc.1`
  predate it) and nothing outside the format crate wrote one, so the header simply drops the
  stored prefix. A file from an unreleased build that stored one fails to decrypt as damaged,
  never silently.

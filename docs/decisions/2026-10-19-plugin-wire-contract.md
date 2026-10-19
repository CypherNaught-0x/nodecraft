## 2026-10-19 · Plugin package and wire contract, protocol 1 (#171)

Phase A of the [plugin RFC](../plugins-rfc.md) is the standalone `nodecraft-plugin` crate and
[its specification](../plugin-wire.md), frozen by fixtures. Choices others will build on:

- **Standalone L0 crate, no workspace dependencies.** The contract is meant to be implemented
  twice (host and guest SDK, or another host), so it shares nothing with engine types and has its
  own error codes. It reads bytes and checks them; installation, storage and execution are later
  phases.
- **One JSON profile, own reader.** serde_json silently keeps the last duplicate key and accepts
  floats and unbounded depth, so the contract has a small bounded reader: unique keys, depth 32,
  integers within ±(2^53−1), `u64` values as canonical decimal strings, no floats outside opaque
  plugin data. Canonical JSON sorts keys by UTF-8 bytes and escapes only what JSON requires.
- **Ed25519 for signatures** (`ed25519-dalek`, BSD-3-Clause, pure Rust, verify-strict), over a
  signature domain distinct from the digest's (`nodecraft-plugin-signature-v1\0`; the RFC's
  digest domain stays `nodecraft-plugin-v1\0`). Trust is a local file of public keys with
  provenance and a trusted/revoked status; revoked keys stay listed; no registry or rotation.
- **Inventory paths are stricter than the RFC's minimum**: ASCII `[A-Za-z0-9._-]` segments, no
  trailing dots or Windows device names, so a package extracts identically on every platform.
- **Floats are not in protocol 1 messages.** Fractional parameter values use #162's string forms;
  a later minor can add a canonical float rule.
- **Opaque plugin data is stored as a JSON string in the project envelope**, so serde
  re-serialisation of `.ncraft` (#54) can't alter its bytes; it is still checked to be bounded
  JSON with unique keys.
- **Registered IDs are refused, never rewritten.** IDs of packages that prefix each other can
  collide; the second registration is refused whole.

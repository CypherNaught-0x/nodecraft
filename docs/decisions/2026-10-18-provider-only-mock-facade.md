## 2026-10-18 · Provider-only mock facade (Part of #275)

- The one deterministic mock implementation lives in `providers::mock`, gated by `cfg(test)`
  or the opt-in `testkit` feature. Existing adapter defaults do not enable it. Local provider
  unit tests use that crate's local trait; no dev facade depends back on the unit-test crate.
- `nodecraft-provider-testkit` reexports mock/provider/types for lower-crate tests. Its only
  workspace dependencies are providers and types, enforced for normal/dev/build edges before
  the generic same-package bypass; facade self-edges are forbidden. It is Class::Testkit:
  non-exempt layered consumers may only use it as a dev-dependency; standalone media refuses
  workspace tooling and apps/tooling keep their existing exemption. Other layer rules are unchanged. The higher testkit
  preserves the public `nodecraft_testkit::mock` path via direct providers reexport, rather than
  normally depending on another Class::Testkit crate.
- The facade disables provider adapter defaults via the canonical explicit workspace path.
  This avoids relying on Cargo1.99's inherited default-feature override beyond our1.95 MSRV,
  without changing existing consumer defaults. Feature unification can enable mock when test
  tooling is selected; this is opt-in testing API, not a security boundary or zero-dependency
  claim. Mock cfg(test) hashing has a dev blake3 dependency as well as the optional feature edge.
- The extraction preserves output bytes, request hashing, accepted-key identity, poll progress,
  cost/failure/constructor evidence, cancellation and Clone behavior. The original mock tests,
  goldens and strict delivery compatibility remain controls. Media remains signature/digest
  fixtures, not a codec-valid image/video/audio guarantee. No snapshots are re-blessed.
- Testkit classes are excluded from the normal wasm set. `cargo xtask wasm` follows its layered
  sweep with an explicit facade and providers/testkit check, propagating failure from either.
  This command-local feature selection does not enable mocks in shipping defaults. No OS IO or
  HTTP transport is added by the facade.
- This addresses requirement3 only. Finite fail-N scripting, real Driver goldens, catalog/job
  admission and instance lanes remain separate #275 work; existing permanent failures are not
  reinterpreted by this move. No durable #254 retention/recovery contract is added.

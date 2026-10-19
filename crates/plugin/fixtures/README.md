# Plugin contract fixtures

Original, public-safe fixtures for the [plugin package and wire contract](../../../docs/plugin-wire.md).
`tests/conformance.rs` runs them; another implementation conforms when it gives the same answers.

- `manifest/valid/*.json`: manifests that decode.
- `manifest/invalid/<code>--<what>.json`: manifests that fail with error `<code>`.
- `frame/valid/*.json`: message payloads that decode and re-encode to the same bytes.
- `frame/invalid/<code>--<what>.json`: payloads that fail with `<code>`.
- `package/textpack/`: a complete package (its `guest.wasm` is only a module header).
  `package/expected.json` records its digest; `package/trust.json` trusts the key that signed it.

Files have no trailing newline: manifests and payloads must be canonical byte for byte.

The signing key was generated for these fixtures and discarded; only its public half is here.
To regenerate the inventory, signature, trust file and digest after changing the package, run
the ignored `regenerate_package_fixture` test with a fresh `NODECRAFT_FIXTURE_SEED` (32 bytes of
hex), as its doc comment shows. Never commit the seed.

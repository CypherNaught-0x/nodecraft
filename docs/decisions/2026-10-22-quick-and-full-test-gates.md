## 2026-10-22 · Quick and full local gates (#781)

`cargo xtask ci` stays the full gate and must pass before a pull request is opened or pushed to.
`cargo xtask ci --quick` is for iterating: fmt, clippy and layers as before, then only the tests of
the packages changed since the merge base with `origin/main` and of their dependents, without the
slow tests, the doctests and the wasm check. Details are in
[development.md](../development.md#quick-and-full).

Why this shape:

- **Most of the time is a few tests.** A full parallel run measured 4139 s of test time, 84% of it in
  the ~100 tests over five seconds, the longest 407 s. They are deliberate stress tests at production
  caps, so quick leaves them out instead of shrinking them; the full gate and CI still run them.
- **nextest, not `cargo test`, in the full gate.** `cargo test` runs one test binary at a time, so
  every binary waits on its own slowest test; on the same machine it was estimated at three times
  nextest's wall time. The full gate uses nextest when installed (then `cargo test --doc`, which
  nextest cannot run) and falls back to `cargo test`, so nextest stays optional for it. Quick
  requires it.
- **Same build for both.** Cargo resolves features per selection, and stable Cargo cannot unify them
  across the workspace (`resolver.feature-unification` is unstable). Selecting a subset with `-p`
  rebuilt the dependencies under another feature set (4 minutes after a full build) and kept a
  second copy of every binary. Quick therefore builds `--workspace` with the full gate's features and
  narrows only what runs, with nextest's `rdeps()` filter.
- **The slow list is generated, not curated.** `cargo xtask slow-tests` times every test and rewrites
  a marked block of `.config/nextest.toml` from nextest's JUnit report. The same block gives those
  tests a high nextest `priority` in every profile, CI's included, so the longest start first rather
  than finishing last. Entries name a package and a full test name, never a binary: nextest
  refuses a `binary_id` that matches nothing, and test files move between binaries.
- **Not taken: `opt-level = 1` for workspace crates.** On exec, graph and types it cut test time by
  63% but raised compile time by 47%, and every edit pays the compile. Revisit if the stress tests
  grow.

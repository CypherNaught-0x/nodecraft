## 2026-10-22 · One integration test binary per crate (#781)

Crates with more than one integration test file set `autotests = false` and build them as one
binary, `tests/it.rs`, which includes each file with `#[path]`. A test that docs or scripts run by
`--test <name>`, or that re-runs its own executable by test name, stays its own `[[test]]`. Details
are in [development.md](../development.md#one-integration-test-binary-per-crate).

Why:

- **Disk.** Every `tests/*.rs` file is a binary of 40–170 MB that statically links its crate and
  everything below it (engine had 67). In one checkout, a test build of the old layout linked 135
  integration test executables (5.7 GB); merged, all 70 test executables come to 1.9 GB. Old copies
  pile up as code changes, so a long-lived worktree measured about 30 GB, 20.6 GB of it 495 linked
  executables. Dependency rlibs were about 2 GB, so sharing
  dependencies between checkouts would save little, and stable Cargo cannot do it safely: it names a
  workspace crate's artifacts the same in every checkout and judges freshness by file times, so one
  checkout can link another's stale code.
- **Time.** One link per crate instead of one per file.
- **Files stay put.** Moving them into `tests/it/` would conflict with every open pull request that
  edits a test and break relative `include_str!` paths; `#[path]` keeps both.

The cost: a file's tests are now `<file>::<test>` in the `it` binary, and Cargo silently ignores a
new `tests/*.rs` file that neither `tests/it.rs` nor a `[[test]]` lists. `cargo xtask layers` and an
xtask test (`every_integration_test_file_is_built`) fail on such a file.

## 2026-11-03 · One build directory shared by parallel worktrees, through xtask (#1033)

**Context.** Agents and contributors build in several git worktrees at once. Each worktree's own
`target/` holds about 15 GB, most of it the same third-party dependencies, so a machine with about
30 GB free fits one build at a time. The rule in `AGENTS.md` until now was never to point two
checkouts at one target dir. That rule is correct for a plain shared `CARGO_TARGET_DIR`. A
workspace crate's artifacts get the same names in every checkout, because Cargo hashes a path
package by its path relative to the workspace root. Cargo judges freshness by comparing file times.
So a checkout whose sources are older than another checkout's last build reuses that build.

We reproduced this (`xtask` itself, two worktrees on one commit, one with a changed usage line):
with `CARGO_TARGET_DIR` shared, building B and then A ran B's code from A.

**Decision.** Sharing becomes opt-in and safe through xtask, trading compute for disk:

- **Opt in.** `NODECRAFT_SHARED_TARGET=<absolute dir>` routes every cargo command xtask runs into
  that directory: `ci`, `ci --quick`, `wasm`, `catalog-check` and `slow-tests`. `cargo xtask
  shared -- <cargo args>` runs any other cargo command the same way. It refuses `clean`,
  `--target-dir` and `--manifest-path`. xtask itself still builds in the worktree's own `target/`
  (about 200 MB), because `cargo xtask` compiles it before xtask can take the lock. For the same
  reason, xtask refuses to run when it was built inside the shared directory, whether through
  `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR` or `build.target-dir`. `perf` keeps its own target.
- **One build at a time.** An exclusive file lock (`std::fs::File::lock`) on the directory is held
  for the whole xtask command, so builds from different checkouts queue rather than interleave.
  - **Nested xtasks.** The holder writes a random token beside the lock and gives it to its children
    in `NODECRAFT_SHARED_TARGET_HELD`. An xtask started inside a held command uses its parent's lock,
    rather than deadlocking on it, only while that token is still the live holder's. That nested
    xtask must also belong to the checkout the directory was prepared for. The token is withdrawn
    when the holder finishes.
  - **A replaced lock file.** A lock file deleted and replaced while someone waited is detected
    (same inode, on Unix), and the lock is taken again.
  - **A cargo outliving its xtask.** Cargo's own per-profile `.cargo-lock` is taken before anything
    is deleted.
- **Rebuild the workspace crates on a switch, and nothing else.** A stamp file names the checkout
  that built last. When a different checkout, or an unknown one, takes the directory, xtask deletes
  the fingerprint directories of the workspace packages (`<package>-<hash>` under every
  `.fingerprint/`, in every profile and target triple) and the incremental state, which Cargo keeps
  for path packages only. A missing fingerprint makes Cargo rebuild those units, overwriting the
  same-named artifacts in place. Third-party artifacts are reused.
- `cargo xtask shared --clean` empties the directory under the lock.

**Alternatives considered.**
- **sccache.** It saves compile time, but every worktree still keeps its full `target/`.
- **Separate dependency and workspace directories.** Cargo has no supported split.
- **Touching every tracked file instead of deleting fingerprints.** It changes the worktree's
  timestamps, and it misses inputs that live outside the checkout.
- **Per-checkout `RUSTFLAGS` to separate the hashes.** That changes the hash of every dependency
  too, which defeats the sharing.

**Consequences.**
- **Disk.** One full build (about 15 GB) plus about 200 MB per worktree, instead of about 15 GB per
  worktree.
- **Compute.**
  - Each switch to a different checkout rebuilds the workspace crates.
  - Builds wait for each other instead of running side by side.
  - Consecutive builds from one checkout are as fast as before.
- **Unchanged default.** Builds in each worktree's own `target/` remain the default, and are faster
  when disk allows.
- **Bypassing xtask is unsafe.** A plain cargo command with `CARGO_TARGET_DIR` set to the shared
  directory skips the lock and the invalidation, so it can still link another checkout's code. The
  documentation says to go through `cargo xtask shared`.

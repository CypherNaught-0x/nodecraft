## 2026-10-23 · Agent worktrees build without incremental compilation (#781)

Agents export `CARGO_INCREMENTAL=0` in their own worktrees for the whole task (AGENTS.md, "Parallel
agents on one machine"). The repository does not set it, so people in the primary checkout keep fast
incremental rebuilds.

Why:

- **Disk.** After `cargo xtask ci`, a worktree's `target/debug` measured 13 GB, 6 GB of it in
  `incremental/`, the cache rustc keeps for every workspace crate in every profile and feature set.
  With many worktrees on one machine, that cache costs more than anything else left.
- **The cost is small.** Rebuilding every test binary after a change to `nodecraft-engine` took
  69–81 s without the cache and 27–57 s with it, on a machine under a load average of about 50.

Setting it once matters: Cargo treats the two modes as different builds, so switching rebuilds
every workspace crate (142 s here).

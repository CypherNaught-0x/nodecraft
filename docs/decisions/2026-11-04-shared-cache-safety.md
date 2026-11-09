## 2026-11-04 · Own and reserve shared Cargo caches before cleanup (#1037)

**Context.** The original [shared cache decision](2026-11-03-shared-build-directory.md) coordinates
worktrees using a lock inside the cache. An ancestor cache can delete a nested cache's lock.
A wrapper interrupted while Cargo is running can leave child processes writing after the lock is
released. Cargo aliases can hide a workspace override or cleanup from the argument check.

**Decision.** Bind an ownership marker to the cache's canonical path and admit only new or empty
directories. Acquire a permanent sibling lock before checking ownership, and reject nested caches
with an ancestor reservation. Write a marker before running Cargo and leave it in place when
the wrapper or its Cargo child may have been interrupted. Require deliberate recovery after all
child processes have stopped. Route the shared paths through Cargo options so a nested
`cargo xtask` launcher and its Cargo children can use an isolated target and outlive the parent.
Forward only known Cargo commands and invoke the three supported extensions directly.

**Alternatives considered.** Keeping the lock inside the directory leaves it vulnerable to
ancestor cleanup. Waiting on observed Cargo profile locks during recovery misses writers outside
those profiles and subprocesses left behind after Cargo exits. A denylist of raw arguments cannot
inspect arbitrary aliases or extensions.

**Consequences.** Older caches must be set aside and updated worktrees start with an empty
dedicated directory. The sibling reservation survives cleanup. An interrupted lease may require
manual recovery. Ordinary isolated builds and the supported shared build and test workflows keep
the same entry points.

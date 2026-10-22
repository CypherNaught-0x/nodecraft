## 2026-10-22 · Text action packages run only under a grant (#173)

- **The first executable runtime is text actions, not modules.** A package with runtime
  `{"kind":"text_actions","schema":1}` registers only `action`s whose descriptors are text action
  definitions (#165): bounded expressions the engine compiles and evaluates itself, so running one
  needs no sandboxed module, no capability and no host binding. `rust_wasm` stays refused at install.
- **Installing is not running.** An executable package can be installed but not enabled until a
  person grants it (`plugins.grant`). A grant names the exact active digest; it lives in its own
  `grants.json` beside the index, so a build from before grants still reads the index.
- **A changed package needs a new yes.** Update, rollback and uninstall revoke the package's grants
  before the index changes, and fail closed: if the revocation can't be saved, the index isn't
  changed either. Revoking disables a package that was enabled under the grant.
- **Epochs only go up.** Each grant takes the next epoch, and a failed write leaves the counter
  where it was, so a host holding the epoch it started under can tell a revoked grant from a new
  one. Revoked grants stay as tombstones, oldest dropped first past 1024.
- **A damaged grants file runs nothing.** It is read strictly; anything it doesn't expect means no
  package is granted until a person grants again. A file from a newer NodeCraft is "unsupported".
- **Definitions are checked at grant and at enable.** The engine compiles every action before a
  grant is given and again before enabling, and allows one output per action for now.
- **Only a person grants or revokes.** Both commands are human-only over MCP, like install.

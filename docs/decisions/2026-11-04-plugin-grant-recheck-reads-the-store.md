## 2026-11-04 · A plugin broker rechecks its grant in the store before each request (#174)

**Problem.** A broker refused to send only once its window had read the plugin store again and
flagged the provider revoked. Windows share one store folder, so a grant revoked in one window
left the others submitting paid jobs until they were restarted. `Grants::holds` existed for a
precommit recheck but nothing called it.

**Decision.**

- **Recheck in the store, every request.** The broker's grant callback asks a `GrantCheck` before
  every paid submit, poll, cancel and download. On the desktop it reads `grants.json` afresh and
  requires `holds(package, digest, epoch)` for the stamp the provider was published under. A
  revoke or re-grant anywhere moves the epoch, so an old stamp never holds again.
- **No lock, fail closed.** The read skips the store's transaction lock: grants are replaced
  atomically, so it sees the old or the new file, and a check never waits behind another window's
  import. Grants that can't be read hold nothing. The file is at most 1 MiB and is usually a few
  KiB, so the read is cheap enough for every request.
- **The window catches up as well.** The plugin worker reads an open store again every 5 s
  (`plugin_cmds::worker::REREAD`). It publishes only when the index or grants bytes changed. The
  catalog and the revoked flags then follow, and restored jobs learn that their plugin is gone.
- **The browser keeps its own view.** Its store lives in memory for one page and no other window
  shares it, so `GrantCheck::this_window()` relies on the in-memory flag only.

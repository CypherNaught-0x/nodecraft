## 2026-11-04 · A plugin broker rechecks its grant in the store before each request (#174)

**Problem.** A broker refused to send only once its own window had read the plugin store again
and flagged the provider revoked. Windows share one store folder, so a grant revoked (or a
package disabled) in one window left the others submitting paid jobs until they restarted.
`Grants::holds` existed for a precommit recheck, but nothing called it.

**Decision.**

- **Recheck in the store, every request.** Before every paid submit, poll, cancel and download,
  the broker asks a `GrantCheck`. On the desktop that calls `store::standing_at`, which reads the
  folder afresh. The provider's stamp passes only if:
  - its digest is the package's active, *enabled* version in `index.json`;
  - `Grants::holds(package, digest, epoch)` is true in `grants.json`.

  So a disable, update, rollback, uninstall, revoke or re-grant in any window ends the stamp, and
  an old stamp never holds again.
- **Cheap, and fails closed without losing billed work.** The check takes no lock: both files are
  replaced atomically, so each read sees an old or a new file whole. It creates no folders. It
  reads each file only up to its own limit (4 MiB for the index, 1 MiB for grants). It has three
  outcomes:
  - **Granted:** the request goes ahead.
  - **Withdrawn:** the store was read and the stamp doesn't stand. The request is refused (`Auth`),
    polls included.
  - **Unknown:** the store can't be read just now (a lock, a file mid-replace, damage, oversize).
    Nothing new is sent. A submit is refused with nothing sent, and a poll or download of a job
    already sent fails transiently (`Busy`), so it is tried again rather than abandoned with
    billed outputs lost.
- **The window catches up as well.** The plugin worker reads an open store again every 5 s
  (`plugin_cmds::worker::REREAD`). Only a store read in full may withdraw a provider: when the
  grants can't be read or the store is busy in another window, the worker publishes nothing and
  tries again on the next tick.
  - It compares the index and grants bytes first, without the lock, and publishes only when they
    changed.
  - It reads those bytes before reading the store, so a write landing in between can only make
    the next read publish again, never leave a change unpublished.
  - The catalog, the revoked flags and restored jobs then follow.
- **Restored jobs keep their grant.** The run journal records, for each plugin job, the package
  digest and grant epoch it was sent under (an additive field). After a restart a job is followed
  only if that is the grant its provider is published under now. Otherwise it is given up as
  possibly charged, and its estimate settles as unresolved. A plugin job saved by an earlier
  build, with no grant recorded, is given up the same way.
- **Downgrading with a plugin job pending loses the run journal.** Older builds read the journal
  with unknown fields refused, so a journal holding a plugin job's grant is damaged to them.
  `RunHost::open_journal` sets the whole file aside (the desktop's `runs.json` becomes `runs.json.damaged`). That drops
  every pending paid job, of every provider, and the whole usage log: spend history and open
  reservations. They can be recovered only by hand, from the set-aside file. The project file is
  unaffected. This is the same property the `staged` field (#664) already has; the field is
  written only for plugin jobs, so a journal without one reads as before.
- **The browser keeps its own view.** Its store lives in memory for one page and no other window
  shares it, so `GrantCheck::this_window()` relies on the in-memory flag only.

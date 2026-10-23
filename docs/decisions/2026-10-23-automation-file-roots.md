## 2026-10-23 · Agents open and save projects only inside folders the person allows (#423)

- **Folders come from the person, at launch.** `nodecraft --control --root <folder>` (save and
  open) and `--read-root <folder>` (open only), repeatable, at most 32. Each must exist; a bad one
  stops the app with the reason rather than starting with fewer folders than asked for. There is
  no command to add a folder, so an agent can't widen its own reach.
- **Checked where admission happens.** `automation::roots::admit` runs inside `mcp::admitted`,
  which `command_run`, the `tools` impact and the live control host all share. The engine and the
  app's own menus never see it: the person's own file commands stay unrestricted.
- **Resolved, then checked, then passed on resolved.** A location must be absolute; it is
  resolved by the OS (`..`, symbolic links, including a link already at the save target) and the
  command receives the resolved path, so what was checked is what is opened or written. Errors
  never echo the requested path, only the allowed folders. The check is at admission: a link
  swapped into the path between the check and the write isn't guarded against (that would need
  `openat`-style writes in `format`). `file.save` resolves the open project's location the same
  way, so a `..` in how it was opened, or a file that is gone, can't lead outside.
- **No discarding work.** `file.open` with `discard_dirty: true` is refused for agents (a
  `conflict`); they save first. `file.save` is refused when the open project lives outside the
  save folders, and for key-backed (browser) storage.
- **`file.cancel` isn't limited** to the folders: it takes no location, and cancelling loses
  nothing written.
- **Not yet:** the folders aren't kept when the app relaunches after an update (#887), and the plain
  `nodecraft-cli mcp` server has none, so it refuses file commands. Configuring folders in the app
  belongs with the in-app server setting (#116).

## 2026-10-23 · Plugin reload remembers a path beside the index, and never carries a grant (#866)

- **A separate `sources.json`.** `plugins.reload` needs the file a package was last imported or
  updated from. The store's index decodes strictly (an unknown key makes it damaged), so adding a
  field would make older builds refuse the whole store. Sources live in their own file through
  the backend, like grants. A missing or damaged file reads as nothing remembered: it costs only
  the Reload button, never a package. A damaged file, or one from a newer format, is never written
  over: saving a location is refused (the import still succeeds and reports `location_not_saved`).
  A write that wouldn't read back within the read limits is refused too, so no saved location is
  lost. `plugins.list` shows agents only `reloadable` and the file's name. Only absolute paths are kept (never a browser pick), at
  most 4 KiB each, and uninstalling forgets the package's path.
- **Reload is an update.** It verifies the whole file with a dry run first, refuses the same
  digest (unchanged) and a file that now holds another package, then disables an enabled package,
  updates it, and enables it again only if it needs no grant and its signer (signature key, or
  unsigned) is unchanged: a new signer is shown to the person before the package goes live. Grants cover one digest and an
  update revokes them, so a package that runs anything stays disabled until the person grants the
  new version (`needs_grant: true`). If the update fails after disabling, the old version is
  enabled again where it still may be, and the error says when it was left disabled.
- **No watching.** Nothing polls the file or the folder; each reload is one explicit request, and
  agents can't make it (human-only in MCP, like `plugins.update`). The browser store refuses it:
  its picked bytes are let go once installed.

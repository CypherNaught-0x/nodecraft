## 2026-10-23 · Plugins in the browser last for the session and never run (#823)

- **The store is in memory.** The browser build registers the `plugins` service over a
  `MemoryBackend` store, so packages last until the page closes or reloads. `plugins.list` answers
  `session_only: true`, and the plugins panel says so above the install form and the list. A
  bounded IndexedDB backend can replace it later without changing the commands.
- **Bytes are picked, not passed.** Command params are capped at about 1 MiB of text, too small for
  a 32 MiB package as base64. As with `file.open` in the browser, the person picks a file and the
  host holds its bytes (`PickedPackage`, one file at a time). `plugins.import` and `plugins.update`
  then name the location the pick was given (`picked/<n>/<name>`). Any other location is NotFound,
  so there is no path to read. The picker checks the file's size before reading it, and the held
  bytes are checked against the same 32 MiB limit. The verifier, the dry run and the digest check
  stay as they are on the desktop.
- **Nothing runs.** `plugins.grant` is Unsupported in this store, and so is `plugins.enable` for
  any package except a data package. No text action or provider package reaches the session, and
  plugin cards in the browser keep saying their plugin can't run there yet. Running packages in
  the browser needs its own decision (see the RFC's browser column).

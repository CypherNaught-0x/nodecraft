## 2026-10-22 · Browser project files (#118)

- **The pickers save in place where the browser has them, and downloads are used elsewhere.**
  - On Chromium-based browsers, Open and Save As use `showOpenFilePicker` and `showSaveFilePicker`, and later saves write back to the same file handle.
  - Other browsers open through an `<input type="file">` and save every version as a download, and the notice says so.
  - `web-sys` only exposes the pickers behind an unstable flag, so they are called through `js-sys` reflection.
- **The session keeps one storage, and the browser does the I/O.**
  - `HostStorage` is the session's `ProjectStorage`. It hands over a picked file's bytes for exactly one `file.open` and keeps the bytes of the last save for the browser to write.
  - Every change still goes through `file.open`, `file.save` and `file.save_as`, so undo, the saved state and the command surfaces don't change.
  - The browser's file handles stay in the port under numeric ids, so the flow and the session hold no browser objects.
- **Locations are keys that are unique per pick** (`<n>/<file name>`). Two files with the same name never share a destination, and no path is involved.
- **The page asks before it is left** while there are unsaved changes, a write in flight, or a save the browser failed to write.
  - At first there was no recovery journal in the browser, and `checkpoint` and `append` did nothing. Recovery from site storage came later (see `2026-10-22-browser-recovery.md`, #717); the guard stays, since recovery is a safety net, not a save.
- **The 64 MiB archive limit applies before reading.** The browser's reported size is checked before reading and the byte count again after.

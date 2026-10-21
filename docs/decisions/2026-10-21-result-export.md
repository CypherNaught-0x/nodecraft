## 2026-10-21 · Exporting a result writes one file the person chose (#557)

- **One command, `history.export`.** `{canvas, id, generation, port?, item?, path, overwrite?}`
  writes one media value of a result (an output, or one item of a list output) to `path`. The
  canvas history chip and App mode's gallery both offer "Export…"; the native save dialog picks
  the path, and the command never opens one. Text results are refused: copying is the way out
  for text.
- **The path is absolute and its folder must exist.** A relative path would land wherever the
  process happens to run; a missing folder is refused rather than created. Messages never repeat
  the path.
- **Never replace without consent.** Without `overwrite` the file is created only if nothing is
  there (staged, then linked into place, so no partial file is left on failure). With it the file
  is replaced atomically. The UI passes `overwrite` because the save dialog has already asked.
- **Links are followed.** A symlink at the chosen path is resolved, as saving a project resolves
  one, so the export lands where the link points. Only a person picks the path, through the
  dialog.
- **Small results write on the frame.** The copy runs in the command, which is fine for images
  and short clips; a multi-gigabyte video blocks the window while it copies. Moving the write to a
  background job, as saving does, is a follow-up.
- **Verified bytes only.** The asset is checked against its hash before anything is created and
  streamed through the store's verifying copy, so a damaged store never yields a file that looks
  like the result.
- **The person's call, not the agent's.** Writing anywhere on the machine is outside what a
  project's agent should do unasked, so MCP lists `history.export` as human-only, as it does
  saving. The browser build reports the command unsupported and the UI says export needs the
  desktop app; a browser download is a follow-up.

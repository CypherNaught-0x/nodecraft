## 2026-10-18 · Project files in the desktop app (#406)

- **One wired session.** The desktop app builds its session over native storage
  (`Session::with_storage` with `format::NativeStorage`). `file.open`, `file.save` and
  `file.save_as` therefore work through the engine's existing paths, and the control channel
  shares this same session.
- **The recovery journal lives in the settings folder** (`recovery/session.journal`), and its
  lock keeps a second window from writing it. If storage can't start (another window holds the
  journal, the project won't open, or there's no settings folder), the window still opens,
  without project files, and a notice says why.
- **A project path on the command line is opened at startup**, and the window title shows its
  file name, with a dot while there are unsaved changes.
- **A crashed session's journal is never overwritten.** Starting a session checkpoints the
  journal, so with the lock held the app first copies aside a journal that holds work: edits,
  damage, or an old format. The copy is `session.journal.unrecovered-<n>`, synced to disk, and a
  notice says so. Recovery (#408) offers it. At most 99 copies are kept; past that, storage still
  works.
- **Quit:** a clean quit closes the journal when everything is saved, or when nothing was done
  (an untitled project with no edits). Otherwise the journal is kept.
- **A project that won't open at startup** leaves the new project in place, still saveable, with
  the reason as a notice.
- **Shortcuts:**
  - ⌘S/Ctrl+S saves a project that has a file.
  - Choosing a file (⇧⌘S, ⌘O, or ⌘S on an untitled project) needs the file dialogs (#407); until
    then a notice says to start NodeCraft with a path.
  - Programmatic calls always pass an explicit location and never open a dialog.
- **Clean quit:** when everything is saved, the journal is closed. Unsaved work keeps its journal,
  so recovery (#408) can offer it.
- **Agents still can't use files:** `file.*` stay human-only for MCP and control until allowed
  read and write roots are enforced (#116). The session is wired, so lifting that policy needs
  no host change.
- The browser build keeps files unsupported until the web build (#99).

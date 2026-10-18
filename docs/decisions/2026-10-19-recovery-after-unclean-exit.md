## 2026-10-19 · Recovery after an unclean exit is offered, never applied silently (#408)

When the desktop app starts and the recovery journal from the last session holds work that was
never closed cleanly, `storage` keeps it aside as `<journal>.unrecovered-<n>` and the shell asks
**Recover unsaved work?** before the canvas takes any input.

- **The open file is recorded beside the journal** (`<journal>.location`, rewritten whenever the
  session's file changes and copied with a kept journal), so recovered work goes back to the
  project it belongs to. Until the dialogs (#407) land, that is the command-line path or the
  last `file.save_as` location.
- **Recover** replays the kept journal and adopts it through `Session::recover`: the document
  is replaced, history starts fresh, the project is unsaved at its recorded file (untitled if it
  had none), and the new journal is checkpointed at once. `recover` refuses once the session has
  edits of its own. It is a host lifecycle method like `close_recovery`, not a command: only the
  person at startup decides, so agents and MCP can't drive it.
- **The kept copy stays until the recovered work is saved.** Quitting first, or crashing again,
  offers the newest copy that replays at the next launch; resolving it removes the older copies
  too, since the newer one holds their work.
- **Start from last save** deletes the kept copies and, when nothing was opened at startup,
  opens the recorded file as last saved, and says so.
- A **damaged** journal offers what replayed before the damage and says that later changes are
  lost. A journal that **can't be replayed** is named once, left on disk and not offered again.

Why: replaying silently would surprise someone who quit on purpose, and discarding silently loses
work. Keeping the copy until a save means no path between the crash and the save loses anything.

## 2026-10-19 · Recovery after an unclean exit is offered, never applied silently (#408)

When the desktop app starts and the recovery journal from the last session holds work that was
never closed cleanly, `storage` keeps it aside as `<journal>.unrecovered-<n>` and the shell asks
**Recover unsaved work?** before the canvas can be used.

- **Recover** replays the kept journal and adopts the result through `Session::recover`: the
  document is replaced, undo history starts fresh, the project is untitled and unsaved (so a save
  never overwrites a file the person didn't pick), and the new journal is checkpointed at once so
  a second crash doesn't lose the recovered work.
- **Start from last save** deletes the kept copy and keeps the fresh session.
- A **damaged** journal offers what replayed before the damage and says that later changes are
  lost. A journal that **can't be replayed** at all is left on disk and named, never deleted.

Why: replaying silently would surprise someone who closed the app on purpose after deciding
against the changes, and discarding silently loses work. Keeping the copy until the person decides
means quitting at the prompt (or crashing again) loses nothing.

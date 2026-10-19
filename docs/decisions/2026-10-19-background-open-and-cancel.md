## 2026-10-19 · Opens run on the save runner too, and `file.cancel` stops either before it publishes (#367)

- **Why.** Opening a large project freezes a private copy of the archive and writes a recovery
  checkpoint of it, roughly twice the archive's bytes, on the calling thread. That blocks the UI
  for as long as the disk takes, with no way to stop it.
- **Same runner, same lending.** `file.open` takes `background: bool` like the saves (#461). The
  open's work (`prepare_open_snapshot`, the checkpoint, then `commit_open` or `cancel_open`) runs
  as one `SaveJob` with the lent storage. The session publishes the new document when it settles
  the job, through `Session::poll_file` (renamed from `poll_save`, since it now covers both).
- **During an open, edits are refused, not queued.** A save makes edits wait because they apply
  to the document the save snapshotted. An open replaces that document, so an edit made meanwhile
  would be lost or land on the wrong project. Every edit, undo, run, import, recovery, close and
  file command returns `Busy` naming `file.cancel`. Pure reads (no-change, perf and service
  effects) still run.
- **Cancellation stops at the point of no return, and says which side it landed on.** One
  `TransferCancel` is shared with the job. A save checks it while preparing and writing the
  archive, up to the atomic publish of the destination. Its checkpoint after that gets a fresh
  token, because the file is already replaced and a cancel can't roll that back. An open checks
  it while freezing the archive and up to publishing its replacement checkpoint, which is what
  makes it final. When an operation that was asked to cancel then fails, the outcome is "the save
  was cancelled; the file is unchanged" or "opening was cancelled; the current project is
  unchanged". When it finished anyway, the outcome is the normal success; `cancel_requested` in
  the status shows the request was seen. A write already inside a legacy byte-buffer storage
  call can't be interrupted, and isn't claimed to be.
- **Progress is a phase, not a percentage.** `FileStatus::operation` reports `kind` (save or
  open), `phase` (`preparing`, `reading`, `writing`, `checkpointing`) and `cancel_requested`,
  read live from an atomic the job updates. The stream byte counters exist only in validation
  builds, so a byte-accurate bar would need new production counters; it is left to a follow-up
  rather than guessed.
- **Out of this slice** (still on #367): the disk-space inventory and advisory preflight, a
  progress overlay with a Cancel button in the app (after the file dialogs in #407 land), and
  using background opens from the app's own open paths.
- **Rejected:** letting edits during an open wait for it (the UI would block for the whole open,
  the problem this fixes), and a separate open thread pool (one storage owner means one operation
  at a time anyway).

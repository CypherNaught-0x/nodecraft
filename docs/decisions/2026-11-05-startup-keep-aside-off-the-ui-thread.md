## 2026-11-05 · The startup keep-aside runs off the UI thread, behind a window that doesn't yet own storage (#1053)

Follows [Recover in the background and what is left of #367](2026-11-04-background-recover-and-close-out.md),
which left the startup keep-aside on the main thread. After a crash, the desktop used to replay the
live journal, copy and sync all of it, and replay the copy again, all before the window existed,
with no progress and no cancel. While a kept copy was waiting to be offered, every launch also
replayed copies on that thread, up to `MAX_KEPT` (99) slots, until one replayed.

- **The window starts without storage; the session takes the journal over later.** #408's rule
  stays as it was: the session's first checkpoint replaces the journal, so the session may not be
  built over a journal whose keep-aside hasn't settled. The startup does the cheap parts on the UI
  thread: it takes the journal's lock (`NativeStorage::new`) and checks the metadata
  (`keep_aside::nothing_to_keep`: no journal or an empty one, and no kept copies). If there is
  nothing to keep aside, the session is built over the storage there, as before; that is every
  clean start. Otherwise the session starts without storage. A thread then owns the storage, and
  with it the lock, and runs `keep_aside::keep`. Once that settles, the new
  `Session::attach_storage` checkpoints the current document there. So a second window still finds
  the journal owned throughout, and nothing checkpoints over it early.
  - *Rejected:* building a second session once the keep-aside finished. Services, runs, templates,
    libraries and plugins all bind to the first one.
  - *Rejected:* letting the session write a checkpoint somewhere else first. That changes the
    journal format's ownership, which this change doesn't need.
- **What waits for the storage.** The startup open (`files.startup`), the recovery question and
  the run host all wait until the keep-aside settles. The run host waits so that no paid run can
  start unjournalled, and only a window that owns the folder's lock keeps `runs.json`.
  `recovery_deciding` is true meanwhile, so live recompute and the run pump hold off. The
  keep-aside's modal owns the frame's input. The control channel, MCP and the agent don't go
  through that input, so the session itself is held: while `Session::hold_for_storage` is set,
  every change is refused as `Busy` saying the work is being kept safe, and `file.status` gives
  the same reason; reads still answer. Without the hold, a client's edit would land unjournalled
  on the sample project and then block the startup open and Recover ("this window already has
  changes"). `attach_storage` lifts the hold, and so does a cancel or failure, after which the
  window works without storage. The desktop shell starts the #968 focus-owner endpoint once the
  window owns the journal, so a second window can still bring it forward.
- **One replay, reused.** The replay that decides whether the journal holds work
  (`keep_aside::live_work`, the old `holds_work`) also builds the question
  (`RecoveryOffer::from_replay`). The kept copy is byte for byte the replayed journal, so replaying
  the copy again only repeated the work. Its media live in this storage's private workspace, which
  the session then owns.
- **Earlier copies are checked off the UI thread.** When the live journal holds no work,
  `RecoveryOffer::earlier` walks the kept slots newest first on the keep-aside's thread, checking
  for a cancel before each copy and inside each replay (`replay_kept_with`). A run of damaged
  copies takes as long as before, but it no longer holds the window. The rule for which copy is
  offered is unchanged.
- **Cancelling never loses a byte.** "Decide later" and quitting cancel through one
  `TransferCancel`, which is checked:
  - before each 1 MiB chunk of the copy;
  - after the sync;
  - throughout the replay.

  When the live journal held no work and only the scan of earlier copies is cancelled, the
  session still takes the journal over: only the question is skipped, and the copies are offered
  next time.

  The copy is written to `<slot>.partial`, synced, checked again that its slot is still free,
  then renamed into its slot. A cancel or failure
  removes only the partial file. A partial file left by a forced quit is truncated and rewritten
  next time. The live journal is only ever read. After a cancel or failure, the storage (and so the
  lock) is dropped before the window hears about it. The window then runs without project storage
  and says so, and the next launch keeps the journal aside again.

  Quitting waits up to 5 s for a cancelled keep-aside to stop. If the keep-aside had already
  finished, the session takes the storage over first, so the journal's state is decided as it is
  for any session, and the kept copy is offered next time rather than the journal being kept twice.
- **The copy is charged.** `SpaceInventory::replay_journal` charges the replay's private copy of
  the checkpoint archive, at most the journal's bytes, before the replay starts.
  `SpaceInventory::keep_journal` charges the kept copy after the replay found work. By then the
  replay's copy is on disk, so the reported free space already includes it, and charging both up
  front would refuse a journal that holds no work. Each check is against the recovery drive's free
  space (`ProjectStorage::recovery_free`). A refusal is `Busy`, gives sizes and never paths, and
  says the work stays in the journal and is offered again after a restart. The sample project's
  checkpoint replaces the journal, which frees its space, so it isn't charged.
- **Progress.** The window shows phases ("Reading…", "Looking for earlier unsaved work…") with a
  spinner. Only the copy has a byte count and a bar. The replay reports phases only, as #475
  decided for opens.

- **The reused replay's media share the session's private quota.** The offer's sealed archive
  and frame captures live in the session's own workspace for as long as the offer, or the
  recovered work, does. Recovering media-heavy work and saving it repeatedly, and Start from last
  save beside it, fit (`keep_aside::tests`). Opening an older-format file that needs a rewrite,
  with an offer still held, isn't covered by a test.

Left as it was: replaying a kept copy still opens a `NativeStorage` over it, leaving a `.lock`
file beside the copy. The web build's recovery runs inline (`web_journal`) and is unchanged.
